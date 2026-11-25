//! Optional bounded text delivery. Display receipts may wake opt-in audio.
use super::*;
use crate::kimi::{
    acp_stream::SEGMENT_CHANNEL_CAPACITY,
    tutor_segments::{TutorSegment, contract_failure_code},
};
use serde::Deserialize;
use std::{fs::File, io::Read, path::Path};
use tokio::sync::mpsc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DeliveryMode {
    WholeReply,
    Segments,
    SegmentsSpeech,
}
impl DeliveryMode {
    pub(super) fn load(root: &Path) -> Result<Self> {
        let path = root.join("runtime/tutor-delivery.json");
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::WholeReply);
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = vec![];
        file.take(2049).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 2048, "教学投递配置超限");
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Config {
            protocol_version: u8,
            mode: String,
            #[serde(default)]
            segment_speech: Option<String>,
        }
        let config: Config =
            serde_json::from_slice(&bytes).map_err(|_| anyhow!("教学投递配置格式无效"))?;
        ensure!(config.protocol_version == 1, "教学投递配置版本不受支持");
        let speech = match config.segment_speech.as_deref().unwrap_or("disabled") {
            "disabled" => false,
            "after_display" => true,
            _ => bail!("分句播报配置无效"),
        };
        match config.mode.as_str() {
            "whole_reply" if !speech => Ok(Self::WholeReply),
            "segments" => Ok(if speech {
                Self::SegmentsSpeech
            } else {
                Self::Segments
            }),
            _ => bail!("教学投递模式不受支持"),
        }
    }
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::WholeReply => "whole_reply",
            Self::Segments | Self::SegmentsSpeech => "segments",
        }
    }
    pub(super) fn segments(self) -> bool {
        self != Self::WholeReply
    }
    pub(super) fn segment_speech(self) -> bool {
        self == Self::SegmentsSpeech
    }
    pub(super) fn status(self) -> Value {
        json!({"protocol_version":1,"mode":self.name(),"automatic_speech":self==Self::WholeReply,
            "segment_speech":if self.segment_speech(){"after_display"}else{"disabled"}})
    }
}

impl Workstation {
    pub(super) async fn generate_whole_cancellable(
        &self,
        request: &Value,
        cancellation: &mut watch::Receiver<bool>,
    ) -> Result<Value> {
        ensure!(!*cancellation.borrow(), "请求已取消");
        let operation = self.provider.generate(request);
        tokio::pin!(operation);
        // Poll the provider first so registration exists before a simultaneous
        // cancellation signal is forwarded. Never drop/abort its cleanup path.
        let reply = tokio::select! { biased;
            result=&mut operation=>result,
            _=cancellation.changed()=>{ self.provider.cancel(s(request,"request_id")).await?; operation.await }
        }?;
        ensure!(!*cancellation.borrow(), "请求已取消");
        Ok(reply)
    }
    async fn persist_segment(
        &self,
        ns: &str,
        sid: &str,
        rid: &str,
        segment: &TutorSegment,
        cancellation: &mut watch::Receiver<bool>,
    ) -> Result<()> {
        ensure!(!*cancellation.borrow(), "请求已取消");
        let _guard = tokio::select! { biased;
            _=cancellation.changed()=>bail!("请求已取消"),
            guard=self.control.lock()=>guard,
        };
        ensure!(
            !*cancellation.borrow()
                && self.current.lock().unwrap().as_ref() == Some(&(ns.to_owned(), sid.to_owned())),
            "教学流已过期"
        );
        self.stores.lock().unwrap()[ns].append_tutor_segment(sid, rid, segment)?;
        self.notify("tutor.segment");
        Ok(())
    }
    pub(super) async fn generate_stream(
        self: &Arc<Self>,
        ns: &str,
        sid: &str,
        request: &Value,
        epoch: i64,
        foreground: ForegroundGuard,
        cancellation: &mut watch::Receiver<bool>,
    ) -> Result<()> {
        let rid = s(request, "request_id");
        let generation = request["generation"]
            .as_u64()
            .ok_or_else(|| anyhow!("教学流代次无效"))?;
        let result=async {
            ensure!(!*cancellation.borrow(),"请求已取消");
            let (sender,mut receiver)=mpsc::channel(SEGMENT_CHANNEL_CAPACITY);
            let operation=self.provider.generate_segments(request,generation,sender);
            tokio::pin!(operation);
            let mut local_error=None;
            let mut receiver_open=true;
            let mut cancellation_seen=false;
            let outcome=loop {
                tokio::select! { biased;
                    result=&mut operation=>break result,
                    _=cancellation.changed(),if !cancellation_seen=>{
                        cancellation_seen=true;
                        local_error=Some(anyhow!("请求已取消"));
                        receiver.close();
                        self.provider.cancel(rid).await?;
                    }
                    segment=receiver.recv(),if receiver_open=>match segment {
                        Some(segment) if local_error.is_none()=>{
                            if let Err(error)=self.persist_segment(ns,sid,rid,&segment,cancellation).await {
                                local_error=Some(error);
                                receiver.close();
                                self.provider.cancel(rid).await?;
                            }
                        }
                        Some(_)=>{},
                        None=>receiver_open=false,
                    }
                }
            };
            // A completed future may win select before the last two items.
            while let Ok(segment)=receiver.try_recv() {
                if outcome.is_ok() && local_error.is_none() {
                    if let Err(error)=self.persist_segment(ns,sid,rid,&segment,cancellation).await { local_error=Some(error); }
                }
            }
            if let Some(error)=local_error { return Err(error); }
            let reply=outcome?.into_reply();
            ensure!(!*cancellation.borrow(),"请求已取消");
            let _guard=self.control.lock().await;
            ensure!(self.current.lock().unwrap().as_ref()==Some(&(ns.to_owned(),sid.to_owned())),"教学流已过期");
            {
                let stores=self.stores.lock().unwrap();
                let store=&stores[ns];
                let snapshot=store.tutor_stream_snapshot(sid,rid)?;
                let tid=s(&snapshot,"turn_id").to_owned();
                store.tx(|| { store.mark_tutor_final(sid,rid)?; store.set_status(sid,"idle") })?;
                let mut pending=self.pending.lock().unwrap();
                pending.insert(tid,Pending { sid:sid.into(),generation:generation as i64,epoch,reply,_foreground:Some(foreground) });
                // Display and applying the final candidate remain separate.
                if let Err(error)=Self::commit_stream_if_ready(store,&mut pending,sid,rid) {
                    self.error(format!("完整回复已验证，显示提交可重试：{}",error.to_string().chars().take(160).collect::<String>()));
                }
            }
            self.kick_segment_speech_locked(ns,sid,rid).await;
            Ok(())
        }.await;
        if let Err(error) = &result {
            self.preempt_segment_speech(sid, rid, generation as i64);
            let _guard = self.control.lock().await;
            let phase = if *cancellation.borrow() {
                "cancelled"
            } else {
                "failed"
            };
            let code = contract_failure_code(error).unwrap_or(if *cancellation.borrow() {
                "request_cancelled"
            } else {
                "provider_or_delivery_failed"
            });
            let _ = self.stores.lock().unwrap()[ns].fail_tutor_stream(sid, rid, phase, code);
            self.fail_segment_speech_locked(ns, sid, rid, "provider_failed")
                .await;
        }
        result
    }
    fn commit_stream_if_ready(
        store: &Store,
        pending: &mut HashMap<String, Pending>,
        sid: &str,
        rid: &str,
    ) -> Result<Value> {
        let before = store.tutor_stream_snapshot(sid, rid)?;
        if !store.tutor_stream_ready(sid, rid)? {
            return Ok(before);
        }
        let tid = s(&before, "turn_id");
        let candidate = pending
            .get(tid)
            .ok_or_else(|| anyhow!("完整教学候选不可用，请重新提问"))?;
        ensure!(
            candidate.sid == sid
                && candidate.generation == before["generation"].as_i64().unwrap_or(-1),
            "完整教学候选已过期"
        );
        let snapshot = store.tx(|| {
            // Mark complete first inside the same transaction: an accepted
            // correction may legitimately bump epoch/generation in apply_reply.
            store.complete_tutor_stream(
                sid,
                rid,
                &strings(&candidate.reply["used_evidence_ids"]),
            )?;
            let mut snapshot = store.tutor_stream_snapshot(sid, rid)?;
            apply_reply(store, sid, &candidate.reply, candidate.epoch)?;
            // Applying an explicit correction may close speech through a
            // generation/learner-epoch trigger. Never return the earlier open gate.
            let speech = store.tutor_speech_state(sid, rid)?;
            for key in [
                "revision",
                "speech_gate",
                "speech_close_reason",
                "speech_completed_through",
                "speech_complete",
            ] {
                snapshot[key] = speech[key].clone();
            }
            Ok(snapshot)
        })?;
        pending.remove(tid);
        Ok(snapshot)
    }
    pub(super) async fn tutor_stream(&self, sid: &str, rid: &str) -> Result<Value> {
        let _guard = self.control.lock().await;
        let current = self
            .current
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("请先选择档案"))?;
        ensure!(current.1 == sid, "会话已过期");
        self.stores.lock().unwrap()[&current.0].tutor_stream_snapshot(sid, rid)
    }
    pub(super) async fn display_segment(
        self: &Arc<Self>,
        sid: &str,
        rid: &str,
        generation: i64,
        sequence: usize,
    ) -> Result<Value> {
        let _guard = self.control.lock().await;
        let current = self
            .current
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("请先选择档案"))?;
        ensure!(current.1 == sid, "会话已过期");
        let result = {
            let stores = self.stores.lock().unwrap();
            let store = &stores[&current.0];
            let mut pending = self.pending.lock().unwrap();
            // The display receipt commits even if final candidate application fails.
            store.ack_tutor_segment(sid, rid, generation, sequence)?;
            Self::commit_stream_if_ready(store, &mut pending, sid, rid)
        };
        self.kick_segment_speech_locked(&current.0, sid, rid).await;
        self.notify("tutor.displayed");
        result
    }
}

#[cfg(test)]
#[path = "tutor_delivery_tests.rs"]
mod tests;
