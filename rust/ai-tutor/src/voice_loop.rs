//! Explicit, short-lived permission for hands-free half-duplex turns.
//! This owns no device. Capture and speech retain their existing precise tokens.
use super::*;
use crate::audio::{EndpointMode, InputBinding, InputSelection};
use serde::Deserialize;

const LEASE: Duration = Duration::from_millis(3000);
const OUTPUT_TAIL: Duration = Duration::from_millis(500);

#[path = "voice_barge_runtime.rs"]
mod barge;
#[cfg(test)]
#[path = "voice_barge_tests.rs"]
mod barge_tests;

enum PauseTarget<'a> {
    Current,
    Lease(&'a LoopTag),
    Recording(u64),
    Reply(&'a str, &'a str, i64),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct LoopTag {
    pub client_loop_id: String,
    pub loop_epoch: u64,
}
impl LoopTag {
    pub(super) fn parse(value: Option<&Value>) -> Result<Option<Self>> {
        value
            .map(|v| {
                let tag: Self = serde_json::from_value(v.clone())?;
                ensure!(
                    valid_id(&tag.client_loop_id) && tag.loop_epoch > 0,
                    "语音轮换标识无效"
                );
                Ok(tag)
            })
            .transpose()
    }
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Armed,
    Starting,
    Listening,
    Finalizing,
    ReadyToSubmit,
    Submitting,
    Reply,
    Cooldown,
    Paused,
}
impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Armed => "armed",
            Self::Starting => "starting",
            Self::Listening => "listening",
            Self::Finalizing => "finalizing",
            Self::ReadyToSubmit => "ready_to_submit",
            Self::Submitting => "submitting",
            Self::Reply => "reply",
            Self::Cooldown => "cooldown",
            Self::Paused => "paused",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Owner {
    namespace: String,
    learner_id: String,
    learner_epoch: i64,
    session_id: String,
    generation: i64,
    language: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LoopReply {
    pub session_id: String,
    pub request_id: String,
    pub generation: i64,
    pub audio_epoch: u64,
}
#[derive(Clone)]
struct Lease {
    tag: LoopTag,
    owner: Owner,
    input: InputBinding,
    deadline: Instant,
    phase: Phase,
    revision: u64,
    recording: Option<u64>,
    final_text: Option<String>,
    reply: Option<LoopReply>,
    cooldown: Option<Instant>,
    pause_reason: Option<String>,
    barge_in: bool,
    reply_acknowledged: bool,
    monitor_pending: bool,
    monitoring: bool,
    adoption: Option<Value>,
}
impl Lease {
    fn live(&self, now: Instant) -> bool {
        self.phase != Phase::Paused && now < self.deadline
    }
    fn change(&mut self, phase: Phase) {
        self.phase = phase;
        self.revision += 1;
    }
    fn revoke(&mut self, reason: &str) -> (Option<u64>, Option<LoopReply>) {
        self.change(Phase::Paused);
        self.pause_reason = Some(reason.into());
        self.final_text = None;
        self.monitor_pending = false;
        self.monitoring = false;
        self.adoption = None;
        (self.recording, self.reply.clone())
    }
    fn begin_recording(&mut self) -> Result<()> {
        ensure!(self.phase == Phase::Armed, "本轮尚不能重新打开麦克风");
        // A late close of the previous reply cannot revoke the new capture
        // while Start awaits the device owner.
        self.reply = None;
        self.cooldown = None;
        self.adoption = None;
        self.change(Phase::Starting);
        Ok(())
    }
    fn projection(&self) -> Value {
        json!({"client_loop_id":self.tag.client_loop_id,"loop_epoch":self.tag.loop_epoch,
            "session_id":self.owner.session_id,"session_generation":self.owner.generation,
            "language":self.owner.language,"device_key":self.input.device_key,
            "input_topology_revision":self.input.topology_revision,"revision":self.revision,
            "phase":self.phase.name(),"rearm_ready":self.phase==Phase::Armed && self.live(Instant::now()),
            "pause_reason":self.pause_reason,"lease_ms":LEASE.as_millis(),
            "barge_in":self.barge_in,"monitor_pending":self.monitor_pending,"monitoring":self.monitoring,
            "reply_request_id":self.reply.as_ref().map(|reply|&reply.request_id),
            "reply_audio_epoch":self.reply.as_ref().map(|reply|reply.audio_epoch),
            "barge_in_adoption":self.adoption})
    }
    fn matches_owner(&self, current: &Owner) -> bool {
        let allowance = match self.phase {
            Phase::Starting => 1,
            Phase::Submitting => 2,
            _ => 0,
        };
        self.owner.namespace == current.namespace
            && self.owner.learner_id == current.learner_id
            && self.owner.learner_epoch == current.learner_epoch
            && self.owner.session_id == current.session_id
            && self.owner.language == current.language
            && current.generation >= self.owner.generation
            && current.generation <= self.owner.generation + allowance
    }
    fn output_ready(&mut self, ready: bool, now: Instant) {
        if !matches!(self.phase, Phase::Reply | Phase::Cooldown | Phase::Armed) {
            return;
        }
        if !ready {
            self.cooldown = None;
            if self.phase != Phase::Reply {
                self.change(Phase::Reply);
            }
        } else if let Some(since) = self.cooldown {
            if now.duration_since(since) >= OUTPUT_TAIL && self.phase != Phase::Armed {
                self.recording = None;
                self.final_text = None;
                self.change(Phase::Armed);
            }
        } else {
            self.cooldown = Some(now);
            self.change(Phase::Cooldown);
        }
    }
    fn begin_submit(
        &mut self,
        sid: &str,
        token: Option<u64>,
        text: &str,
        auto_speak: bool,
        nonce: Option<&str>,
    ) -> Result<()> {
        ensure!(
            self.phase == Phase::ReadyToSubmit
                && self.owner.session_id == sid
                && self.recording == token
                && token.is_some()
                && self.final_text.as_deref() == Some(text)
                && auto_speak
                && nonce.is_some(),
            "自动发送只接受本轮未经改动的完整终稿"
        );
        self.final_text = None;
        self.change(Phase::Submitting);
        Ok(())
    }
}
#[derive(Default)]
pub(super) struct VoiceLoopControl {
    epoch: u64,
    lease: Option<Lease>,
    barge_task: Option<JoinHandle<()>>,
}
/// Created only after the phase-specific request has been accepted under control.
/// A rejected old request must never revoke a later round of the same lease.
pub(super) struct LoopOperation<'a> {
    app: &'a Workstation,
    tag: LoopTag,
    completed: bool,
}
impl LoopOperation<'_> {
    pub(super) fn complete(&mut self) {
        self.completed = true;
    }
}
impl Drop for LoopOperation<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.app
                .pause_voice_loop(Some(&self.tag), "operation_failed");
        }
    }
}
impl VoiceLoopControl {
    fn live_mut(&mut self, tag: &LoopTag) -> Result<&mut Lease> {
        self.lease
            .as_mut()
            .filter(|l| &l.tag == tag && l.live(Instant::now()))
            .ok_or_else(|| anyhow!("语音轮换已暂停，请明确继续"))
    }
}

impl Workstation {
    pub(super) fn loop_operation(&self, tag: &LoopTag) -> LoopOperation<'_> {
        LoopOperation {
            app: self,
            tag: tag.clone(),
            completed: false,
        }
    }
    fn voice_loop_owner(&self, sid: &str) -> Result<Owner> {
        let (namespace, session) = self.writable(sid)?;
        let learner_id = s(&session, "learner_id").to_owned();
        let learner = self.stores.lock().unwrap()[&namespace].learner(&learner_id)?;
        Ok(Owner {
            namespace,
            learner_id,
            learner_epoch: learner["epoch"].as_i64().unwrap_or(-1),
            session_id: sid.into(),
            generation: session["generation"].as_i64().unwrap_or(-1),
            language: s(&learner, "language").into(),
        })
    }
    pub(super) fn voice_loop_status(&self) -> Value {
        let mut status = self
            .voice_loop
            .lock()
            .unwrap()
            .lease
            .as_ref()
            .map(Lease::projection)
            .unwrap_or_else(|| json!({}));
        status["available"] = json!(
            self.delivery_mode.segment_speech() && !self.shutting_down.load(Ordering::SeqCst)
        );
        status["barge_in_available"] = json!(self.audio.barge_in_available());
        status
    }
    fn voice_loop_context(&self, lease: &Lease) -> Result<()> {
        ensure!(!self.shutting_down.load(Ordering::SeqCst), "工作台正在关闭");
        ensure!(
            lease.matches_owner(&self.voice_loop_owner(&lease.owner.session_id)?),
            "轮换所用档案、会话或语言已变化"
        );
        if lease.barge_in {
            ensure!(
                self.audio.barge_in_available(),
                "实验性语音打断能力已不可用"
            );
            let learner = self.stores.lock().unwrap()[&lease.owner.namespace]
                .learner(&lease.owner.learner_id)?;
            ensure!(
                learner["speech_speed"] == "normal",
                "语音打断语速设置已变化"
            );
        }
        self.audio
            .validate_input_selection(&InputSelection::Bound(lease.input.clone()))
    }
    /// Revocation never waits for control, ASR, a model, or the browser.
    /// The copied tokens can only discard this lease's capture/speech.
    pub(super) fn pause_voice_loop(
        &self,
        tag: Option<&LoopTag>,
        reason: &str,
    ) -> Option<LoopReply> {
        self.pause_matching_voice_loop(
            tag.map(PauseTarget::Lease).unwrap_or(PauseTarget::Current),
            reason,
        )
    }
    fn pause_matching_voice_loop(
        &self,
        target: PauseTarget<'_>,
        reason: &str,
    ) -> Option<LoopReply> {
        let (recording, reply, task) = {
            let mut state = self.voice_loop.lock().unwrap();
            let lease = state.lease.as_mut()?;
            let matches = match target {
                PauseTarget::Current => true,
                PauseTarget::Lease(tag) => tag == &lease.tag,
                PauseTarget::Recording(token) => lease.recording == Some(token),
                PauseTarget::Reply(sid, rid, generation) => {
                    lease.reply.as_ref().is_some_and(|reply| {
                        reply.session_id == sid
                            && reply.request_id == rid
                            && reply.generation == generation
                    })
                }
            };
            if !matches || lease.phase == Phase::Paused {
                return None;
            }
            let (recording, reply) = lease.revoke(reason);
            (recording, reply, state.barge_task.take())
        };
        if let Some(task) = task {
            task.abort();
        }
        self.apply_voice_loop_revocation(recording, reply)
    }
    fn apply_voice_loop_revocation(
        &self,
        recording: Option<u64>,
        reply: Option<LoopReply>,
    ) -> Option<LoopReply> {
        if let Some(token) = recording {
            let _ = self.audio.discard_recording_expected(token);
        }
        if let Some(reply) = &reply {
            self.preempt_segment_speech(&reply.session_id, &reply.request_id, reply.generation);
        }
        self.notify("audio.changed");
        reply
    }
    pub(super) async fn enable_voice_loop(
        self: &Arc<Self>,
        sid: &str,
        data: &Value,
    ) -> Result<Value> {
        allowed_keys(
            data,
            &[
                "client_loop_id",
                "expected_generation",
                "language",
                "device_key",
                "expected_input_topology_revision",
                "barge_in",
            ],
        )?;
        let id = text(data, "client_loop_id", 1, 96)?;
        ensure!(valid_id(id), "语音轮换标识无效");
        let expected =
            recording::expected_generation(data)?.ok_or_else(|| anyhow!("缺少会话代次"))?;
        let language = text(data, "language", 2, 5)?;
        let barge_in = data
            .get("barge_in")
            .map(|value| {
                value
                    .as_bool()
                    .ok_or_else(|| anyhow!("语音打断选项必须为布尔值"))
            })
            .transpose()?
            .unwrap_or(false);
        let input = InputBinding {
            device_key: text(data, "device_key", 1, 96)?.into(),
            topology_revision: data["expected_input_topology_revision"]
                .as_u64()
                .filter(|v| *v > 0)
                .ok_or_else(|| anyhow!("缺少麦克风版本"))?,
        };
        let _control = self.control.lock().await;
        ensure!(
            self.delivery_mode.segment_speech() && !self.shutting_down.load(Ordering::SeqCst),
            "当前未启用分句播报，语音轮换不可用"
        );
        let owner = self.voice_loop_owner(sid)?;
        ensure!(
            owner.generation == expected && owner.language == language,
            "会话或教学语言已变化"
        );
        if barge_in {
            ensure!(
                self.audio.barge_in_available(),
                "请先显式启用实验性语音打断与回声处理"
            );
            let learner =
                self.stores.lock().unwrap()[&owner.namespace].learner(&owner.learner_id)?;
            ensure!(
                learner["speech_speed"] == "normal",
                "实验性语音打断只支持正常语速"
            );
        }
        self.audio.validate_endpoint_selection(
            &InputSelection::Bound(input.clone()),
            EndpointMode::VoiceEndpointV1,
        )?;
        let audio = self.audio.status();
        ensure!(
            audio["asr_ready"] == true && self.provider.foreground_status()["available"] == true,
            "请先准备本地转写和教学模型"
        );
        ensure!(
            audio["state"] == "idle"
                && audio["recording"] != true
                && audio["capture_pending_result"] != true
                && self.running.lock().unwrap().is_empty()
                && self.local_voice_installation_epoch_locked()?.is_some(),
            "请先完成录音、转写和播报"
        );
        let tag = {
            let mut state = self.voice_loop.lock().unwrap();
            ensure!(
                state
                    .lease
                    .as_ref()
                    .is_none_or(|l| l.phase == Phase::Paused),
                "已有语音轮换，请先暂停"
            );
            state.epoch += 1;
            let tag = LoopTag {
                client_loop_id: id.into(),
                loop_epoch: state.epoch,
            };
            state.lease = Some(Lease {
                tag: tag.clone(),
                owner,
                input,
                deadline: Instant::now() + LEASE,
                phase: Phase::Armed,
                revision: 1,
                recording: None,
                final_text: None,
                reply: None,
                cooldown: None,
                pause_reason: None,
                barge_in,
                reply_acknowledged: false,
                monitor_pending: false,
                monitoring: false,
                adoption: None,
            });
            tag
        };
        let weak = Arc::downgrade(self);
        let watch_tag = tag.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let Some(app) = weak.upgrade() else {
                    break;
                };
                let expired = {
                    let state = app.voice_loop.lock().unwrap();
                    let Some(lease) = state
                        .lease
                        .as_ref()
                        .filter(|l| l.tag == watch_tag && l.phase != Phase::Paused)
                    else {
                        break;
                    };
                    !lease.live(Instant::now())
                };
                if expired {
                    if let Some(reply) = app.pause_voice_loop(Some(&watch_tag), "heartbeat_expired")
                    {
                        let _ = app
                            .close_segment_speech(
                                &reply.session_id,
                                &reply.request_id,
                                reply.generation,
                                "user",
                            )
                            .await;
                    }
                    break;
                }
            }
        });
        self.voice_loop_echo(&tag)
    }
    pub(super) fn voice_loop_echo(&self, tag: &LoopTag) -> Result<Value> {
        let state = self.voice_loop.lock().unwrap();
        Ok(state
            .lease
            .as_ref()
            .filter(|l| &l.tag == tag)
            .ok_or_else(|| anyhow!("语音轮换已更换"))?
            .projection())
    }
    pub(super) async fn heartbeat_voice_loop(
        self: &Arc<Self>,
        sid: &str,
        id: &str,
        data: &Value,
    ) -> Result<Value> {
        allowed_keys(data, &["loop_epoch", "reply_ack"])?;
        let reply_ack = data
            .get("reply_ack")
            .map(barge::ReplyAck::parse)
            .transpose()?;
        let tag = LoopTag::parse(Some(
            &json!({"client_loop_id":id,"loop_epoch":data["loop_epoch"]}),
        ))?
        .unwrap();
        // Database/device checks never hold the lease lock or delay revocation.
        // Discard a result if Start/Submit changed phase during the owner read,
        // and recheck the deadline before renewing.
        let snapshot = { self.voice_loop.lock().unwrap().live_mut(&tag)?.clone() };
        ensure!(snapshot.owner.session_id == sid, "轮换会话不匹配");
        let validation = self.voice_loop_context(&snapshot);
        if !self.renew_loop_heartbeat(&tag, snapshot.revision, validation)? {
            return self.voice_loop_echo(&tag);
        }
        if let Ok(_control) = self.control.try_lock() {
            let snapshot = { self.voice_loop.lock().unwrap().live_mut(&tag)?.clone() };
            if snapshot.barge_in {
                self.barge_heartbeat_locked(&tag, &snapshot, reply_ack.as_ref())?;
            }
            let snapshot = { self.voice_loop.lock().unwrap().live_mut(&tag)?.clone() };
            if let Some(reply) = &snapshot.reply {
                if snapshot.monitor_pending || snapshot.monitoring {
                    return self.voice_loop_echo(&tag);
                }
                let ready = self.voice_loop_reply_output_ready_locked(reply)?;
                self.voice_loop
                    .lock()
                    .unwrap()
                    .live_mut(&tag)?
                    .output_ready(ready, Instant::now());
            }
        }
        self.voice_loop_echo(&tag)
    }
    fn renew_loop_heartbeat(
        &self,
        tag: &LoopTag,
        revision: u64,
        validation: Result<()>,
    ) -> Result<bool> {
        let revocation = {
            let mut state = self.voice_loop.lock().unwrap();
            let lease = state.live_mut(tag)?;
            if lease.revision != revision {
                return Ok(false);
            }
            match validation {
                Ok(()) => {
                    lease.deadline = Instant::now() + LEASE;
                    return Ok(true);
                }
                Err(error) => {
                    let revoked = lease.revoke("context_changed");
                    (revoked, error, state.barge_task.take())
                }
            }
        };
        if let Some(task) = revocation.2 {
            task.abort();
        }
        self.apply_voice_loop_revocation(revocation.0.0, revocation.0.1);
        Err(revocation.1)
    }
    pub(super) async fn stop_voice_loop(&self, sid: &str, id: &str, data: &Value) -> Result<Value> {
        allowed_keys(data, &["loop_epoch", "reason"])?;
        let reason = text(data, "reason", 1, 64)?;
        let tag = LoopTag::parse(Some(
            &json!({"client_loop_id":id,"loop_epoch":data["loop_epoch"]}),
        ))?
        .unwrap();
        ensure!(
            self.voice_loop_echo(&tag)?["session_id"] == sid,
            "轮换会话不匹配"
        );
        if let Some(reply) = self.pause_voice_loop(Some(&tag), reason) {
            let _ = self
                .close_segment_speech(
                    &reply.session_id,
                    &reply.request_id,
                    reply.generation,
                    "user",
                )
                .await;
        }
        self.voice_loop_echo(&tag)
    }
    pub(super) fn begin_loop_recording(
        &self,
        tag: &LoopTag,
        sid: &str,
        selection: &InputSelection,
        mode: EndpointMode,
    ) -> Result<()> {
        let snapshot = { self.voice_loop.lock().unwrap().live_mut(tag)?.clone() };
        self.voice_loop_context(&snapshot)?;
        ensure!(
            snapshot.owner.session_id == sid
                && selection.binding() == Some(&snapshot.input)
                && mode == EndpointMode::VoiceEndpointV1
                && snapshot.phase == Phase::Armed,
            "本轮尚不能重新打开麦克风"
        );
        if let Some(reply) = &snapshot.reply {
            ensure!(
                self.voice_loop_reply_output_ready_locked(reply)?
                    && snapshot
                        .cooldown
                        .is_some_and(|t| t.elapsed() >= OUTPUT_TAIL),
                "上一轮播报尚未完整结束"
            );
        }
        self.voice_loop
            .lock()
            .unwrap()
            .live_mut(tag)?
            .begin_recording()
    }
    pub(super) fn loop_recording_reserved(&self, tag: &LoopTag, token: u64) -> bool {
        let mut state = self.voice_loop.lock().unwrap();
        let Ok(lease) = state.live_mut(tag) else {
            return false;
        };
        if lease.phase != Phase::Starting {
            return false;
        }
        lease.recording = Some(token);
        true
    }
    pub(super) fn loop_generation(&self, tag: &LoopTag, sid: &str) -> Result<()> {
        let owner = self.voice_loop_owner(sid)?;
        let mut state = self.voice_loop.lock().unwrap();
        let lease = state.live_mut(tag)?;
        ensure!(lease.matches_owner(&owner), "轮换会话已变化");
        lease.owner.generation = owner.generation;
        lease.revision += 1;
        Ok(())
    }
    pub(super) fn loop_recording_started(&self, tag: &LoopTag, token: u64) -> Result<()> {
        let mut state = self.voice_loop.lock().unwrap();
        let lease = state.live_mut(tag)?;
        ensure!(
            lease.phase == Phase::Starting && lease.recording == Some(token),
            "轮换录音已过期"
        );
        lease.reply = None;
        lease.cooldown = None;
        lease.change(Phase::Listening);
        Ok(())
    }
    pub(super) fn loop_recording_finalizing(&self, token: u64) {
        let mut state = self.voice_loop.lock().unwrap();
        if let Some(lease) = state.lease.as_mut().filter(|l| {
            l.live(Instant::now()) && l.recording == Some(token) && l.phase == Phase::Listening
        }) {
            lease.change(Phase::Finalizing);
        }
    }
    pub(super) fn pause_voice_loop_recording(&self, token: u64, reason: &str) {
        self.pause_matching_voice_loop(PauseTarget::Recording(token), reason);
    }
    pub(super) fn pause_voice_loop_reply(
        &self,
        sid: &str,
        rid: &str,
        generation: i64,
        reason: &str,
    ) {
        self.pause_matching_voice_loop(PauseTarget::Reply(sid, rid, generation), reason);
    }
    pub(super) fn loop_recording_final(&self, token: u64, response: &Value) {
        let pause = {
            let mut state = self.voice_loop.lock().unwrap();
            let Some(lease) = state.lease.as_mut().filter(|l| {
                l.live(Instant::now()) && l.recording == Some(token) && l.phase == Phase::Finalizing
            }) else {
                return;
            };
            let endpoint = &response["recording_endpoint"];
            let text = s(response, "text").trim();
            if endpoint["recording_generation"] == token
                && endpoint["reason"] == "speech_end"
                && endpoint["input_closed"] == true
                && response["error"].is_null()
                && !text.is_empty()
                && text.chars().count() <= 4000
            {
                lease.final_text = Some(text.into());
                lease.change(Phase::ReadyToSubmit);
                None
            } else {
                Some(lease.tag.clone())
            }
        };
        if let Some(tag) = pause {
            self.pause_voice_loop(Some(&tag), "transcript_needs_review");
        }
    }
    pub(super) fn begin_loop_submit(
        &self,
        tag: &LoopTag,
        sid: &str,
        token: Option<u64>,
        text: &str,
        auto_speak: bool,
        nonce: Option<&str>,
    ) -> Result<()> {
        let snapshot = { self.voice_loop.lock().unwrap().live_mut(tag)?.clone() };
        self.voice_loop_context(&snapshot)?;
        let mut state = self.voice_loop.lock().unwrap();
        let lease = state.live_mut(tag)?;
        lease.begin_submit(sid, token, text, auto_speak, nonce)
    }
    pub(super) fn loop_reply_registered(&self, tag: &LoopTag, reply: LoopReply) -> Result<()> {
        let owner = self.voice_loop_owner(&reply.session_id)?;
        let mut state = self.voice_loop.lock().unwrap();
        let lease = state.live_mut(tag)?;
        ensure!(
            lease.phase == Phase::Submitting
                && reply.session_id == lease.owner.session_id
                && lease.matches_owner(&owner)
                && reply.generation == owner.generation
                && self.audio.speech_epoch() == reply.audio_epoch,
            "轮换提交已撤销"
        );
        lease.owner.generation = reply.generation;
        lease.reply = Some(reply);
        lease.recording = None;
        lease.reply_acknowledged = false;
        lease.monitor_pending = lease.barge_in;
        lease.monitoring = false;
        lease.adoption = None;
        lease.change(Phase::Reply);
        Ok(())
    }
}

#[cfg(test)]
#[path = "voice_loop_tests.rs"]
mod tests;
