//! One opt-in stream, one segment at a time. SQLite owns the delivery ledger.
use super::*;
use crate::store::TutorSpeechWork;
use futures_util::future::BoxFuture;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AutoSpeechKey {
    namespace: String,
    learner_id: String,
    session_id: String,
    request_id: String,
    generation: i64,
    learner_epoch: i64,
    audio_epoch: u64,
}

#[cfg(test)]
#[path = "segment_speech_tests.rs"]
mod tests;
pub(super) struct AutoSpeechFlow {
    key: AutoSpeechKey,
    task: Option<JoinHandle<()>>,
}
impl AutoSpeechKey {
    fn matches(&self, sid: &str, rid: &str, generation: i64) -> bool {
        self.session_id == sid && self.request_id == rid && self.generation == generation
    }
}

trait SegmentSpeaker: Send + Sync + 'static {
    fn speak<'a>(
        &'a self,
        work: &'a TutorSpeechWork,
        started: oneshot::Sender<SpeechStarted>,
    ) -> BoxFuture<'a, Result<SpeechOutcome>>;
}
impl SegmentSpeaker for AudioService {
    fn speak<'a>(
        &'a self,
        work: &'a TutorSpeechWork,
        started: oneshot::Sender<SpeechStarted>,
    ) -> BoxFuture<'a, Result<SpeechOutcome>> {
        Box::pin(self.speak_with_receipts(
            work.text.clone(),
            work.language.clone(),
            &work.speech_speed,
            work.audio_epoch,
            started,
        ))
    }
}

impl Workstation {
    /// A segment callback or an empty queue is insufficient: require the full
    /// validated/displayed reply, its durable speech receipts and retired output.
    pub(crate) fn voice_loop_reply_output_ready_locked(
        &self,
        reply: &super::super::voice_loop::LoopReply,
    ) -> Result<bool> {
        self.voice_loop_reply_ready_with_monitor_locked(reply, None)
    }

    pub(crate) fn voice_loop_reply_ready_with_monitor_locked(
        &self,
        reply: &super::super::voice_loop::LoopReply,
        monitor: Option<u64>,
    ) -> Result<bool> {
        let delivery = self.speech_delivery.lock().unwrap();
        if delivery.active.is_some() {
            return Ok(false);
        }
        let Some(flow) = delivery.stream.as_ref().filter(|flow| {
            flow.key
                .matches(&reply.session_id, &reply.request_id, reply.generation)
                && flow.key.audio_epoch == reply.audio_epoch
        }) else {
            return Ok(false);
        };
        if flow.task.is_some() {
            return Ok(false);
        }
        let key = flow.key.clone();
        drop(delivery);
        if !self.segment_flow_current(&key)
            || self.running.lock().unwrap().contains_key(&reply.request_id)
        {
            return Ok(false);
        }
        let audio = self.audio.status();
        let own_monitor = monitor.is_some_and(|token| {
            audio["barge_in"]["recording_generation"].as_u64() == Some(token)
                && audio["barge_in"]["expected_audio_epoch"].as_u64() == Some(reply.audio_epoch)
                && ["monitor", "candidate"].contains(&s(&audio["barge_in"], "phase"))
        });
        if !own_monitor
            && (audio["state"] != "idle"
                || audio["recording"] == true
                || audio["capture_pending_result"] == true)
        {
            return Ok(false);
        }
        let snapshot = self.stores.lock().unwrap()[&key.namespace]
            .tutor_stream_snapshot(&reply.session_id, &reply.request_id)?;
        Ok(snapshot["phase"] == "completed"
            && snapshot["final_validated"] == true
            && snapshot["speech_gate"] == "open"
            && snapshot["speech_complete"] == true
            && snapshot["generation"] == reply.generation
            && snapshot["speech_gate_audio_epoch"].as_u64() == Some(reply.audio_epoch))
    }

    /// The caller has already detached the old reply from the lease and the
    /// audio owner has revoked exactly its epoch. Never stop a newer playback.
    pub(crate) fn close_barge_in_speech_locked(
        &self,
        reply: &super::super::voice_loop::LoopReply,
    ) -> Result<()> {
        let key = self
            .speech_delivery
            .lock()
            .unwrap()
            .stream
            .as_ref()
            .filter(|flow| {
                flow.key
                    .matches(&reply.session_id, &reply.request_id, reply.generation)
                    && flow.key.audio_epoch == reply.audio_epoch
            })
            .map(|flow| flow.key.clone());
        if let Some(key) = key {
            let (task, result) = self.retire_segment_flow_locked(&key, "barge_in");
            drop(task); // already aborted; never await the old worker under control
            result?;
        }
        Ok(())
    }

    /// Called under control, before sending a conditional configuration command.
    /// An empty current segment queue does not mean this reply has finished.
    pub(crate) fn local_voice_installation_epoch_locked(&self) -> Result<Option<u64>> {
        let delivery = self.speech_delivery.lock().unwrap();
        if delivery.active.is_some() || self.shutting_down.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let flow = delivery
            .stream
            .as_ref()
            .map(|flow| (flow.key.clone(), flow.task.is_some()));
        drop(delivery);
        if let Some((key, task_active)) = flow {
            let speech = self.stores.lock().unwrap()[&key.namespace]
                .tutor_speech_state(&key.session_id, &key.request_id)?;
            if !stream_allows_voice_installation(&speech, task_active) {
                return Ok(None);
            }
        }
        Ok(Some(self.audio.speech_epoch()))
    }

    pub(crate) fn register_segment_speech_locked(
        &self,
        namespace: &str,
        request: &Value,
        learner_epoch: i64,
        audio_epoch: u64,
    ) {
        self.speech_delivery.lock().unwrap().stream = Some(AutoSpeechFlow {
            key: AutoSpeechKey {
                namespace: namespace.into(),
                learner_id: s(request, "learner_id").into(),
                session_id: s(request, "session_id").into(),
                request_id: s(request, "request_id").into(),
                generation: request["generation"].as_i64().unwrap_or(-1),
                learner_epoch,
                audio_epoch,
            },
            task: None,
        });
    }

    // This synchronous invalidation precedes waiting for the teaching lock.
    // Matching a stream never authorizes stopping a later manual playback.
    pub(crate) fn preempt_segment_speech(&self, sid: &str, rid: &str, generation: i64) {
        let delivery = self.speech_delivery.lock().unwrap();
        if let Some(flow) = delivery
            .stream
            .as_ref()
            .filter(|f| f.key.matches(sid, rid, generation))
        {
            self.audio.invalidate_speech_epoch(flow.key.audio_epoch);
        }
    }

    fn segment_flow_matches(&self, key: &AutoSpeechKey) -> bool {
        self.speech_delivery
            .lock()
            .unwrap()
            .stream
            .as_ref()
            .is_some_and(|f| f.key == *key)
    }

    fn segment_flow_current(&self, key: &AutoSpeechKey) -> bool {
        if self.shutting_down.load(Ordering::SeqCst)
            || self.audio.speech_epoch() != key.audio_epoch
            || self.current.lock().unwrap().as_ref()
                != Some(&(key.namespace.clone(), key.session_id.clone()))
        {
            return false;
        }
        let stores = self.stores.lock().unwrap();
        let store = &stores[&key.namespace];
        let Ok(session) = store.session(&key.session_id) else {
            return false;
        };
        let Ok(learner) = store.learner(&key.learner_id) else {
            return false;
        };
        let Ok(speech) = store.tutor_speech_state(&key.session_id, &key.request_id) else {
            return false;
        };
        session["generation"] == key.generation
            && session["status"] != "finished"
            && s(&session, "learner_id") == key.learner_id
            && learner["epoch"] == key.learner_epoch
            && speech["speech_gate"] == "open"
            && speech["speech_gate_audio_epoch"].as_u64() == Some(key.audio_epoch)
    }

    /// Caller holds control. The returned handle is already aborted; external
    /// callers may await it, while the worker itself only drops its own handle.
    fn retire_segment_flow_locked(
        &self,
        key: &AutoSpeechKey,
        reason: &str,
    ) -> (Option<JoinHandle<()>>, Result<Value>) {
        self.audio.invalidate_speech_epoch(key.audio_epoch);
        let task = {
            let mut delivery = self.speech_delivery.lock().unwrap();
            if delivery.stream.as_ref().is_some_and(|f| f.key == *key) {
                delivery.stream.take().and_then(|f| f.task)
            } else {
                None
            }
        };
        if let Some(task) = &task {
            task.abort();
        }
        // An owned next recording uses superseded while transitioning from an
        // already drained reply. Every other terminal close revokes its loop.
        if reason != "superseded" {
            self.pause_voice_loop_reply(&key.session_id, &key.request_id, key.generation, reason);
        }
        let result = self.stores.lock().unwrap()[&key.namespace].close_tutor_speech(
            &key.session_id,
            &key.request_id,
            key.generation,
            reason,
        );
        self.notify("tts.status");
        (task, result)
    }

    pub(crate) async fn stop_segment_speech_locked(&self, reason: &str) -> Result<()> {
        let key = self
            .speech_delivery
            .lock()
            .unwrap()
            .stream
            .as_ref()
            .map(|f| f.key.clone());
        if let Some(key) = key {
            let (task, result) = self.retire_segment_flow_locked(&key, reason);
            if let Some(task) = task {
                let _ = task.await;
            }
            result?;
        }
        Ok(())
    }

    pub(crate) async fn close_segment_speech(
        &self,
        sid: &str,
        rid: &str,
        generation: i64,
        reason: &str,
    ) -> Result<Value> {
        ensure!(
            ["user", "auto_speak_off", "hidden", "history"].contains(&reason),
            "分句停声原因无效"
        );
        self.preempt_segment_speech(sid, rid, generation);
        let _control = self.control.lock().await;
        let key = self
            .speech_delivery
            .lock()
            .unwrap()
            .stream
            .as_ref()
            .filter(|f| f.key.matches(sid, rid, generation))
            .map(|f| f.key.clone());
        if let Some(key) = key {
            let (task, result) = self.retire_segment_flow_locked(&key, reason);
            if let Some(task) = task {
                let _ = task.await;
            }
            return result;
        }
        // An old/duplicate close only updates its original persistent row.
        // In particular, it cannot advance the current manual playback epoch.
        let stores = self.stores.lock().unwrap();
        for store in stores.values() {
            if store.tutor_speech_state(sid, rid).is_ok() {
                return store.close_tutor_speech(sid, rid, generation, reason);
            }
        }
        bail!("教学流不存在")
    }

    pub(crate) async fn fail_segment_speech_locked(
        &self,
        ns: &str,
        sid: &str,
        rid: &str,
        reason: &str,
    ) {
        let key = self
            .speech_delivery
            .lock()
            .unwrap()
            .stream
            .as_ref()
            .filter(|f| f.key.namespace == ns && f.key.session_id == sid && f.key.request_id == rid)
            .map(|f| f.key.clone());
        if let Some(key) = key {
            let (task, result) = self.retire_segment_flow_locked(&key, reason);
            if let Some(task) = task {
                let _ = task.await;
            }
            if let Err(error) = result {
                self.error(format!("分句停声状态未保存：{error}"));
            }
        }
    }

    pub(crate) async fn kick_segment_speech_locked(
        self: &Arc<Self>,
        ns: &str,
        sid: &str,
        rid: &str,
    ) {
        self.kick_segment_speech_with_locked(ns, sid, rid, self.audio.clone())
            .await;
    }
    async fn kick_segment_speech_with_locked<S: SegmentSpeaker>(
        self: &Arc<Self>,
        ns: &str,
        sid: &str,
        rid: &str,
        speaker: Arc<S>,
    ) {
        let key = {
            let delivery = self.speech_delivery.lock().unwrap();
            let Some(flow) = delivery.stream.as_ref().filter(|f| {
                f.key.namespace == ns && f.key.session_id == sid && f.key.request_id == rid
            }) else {
                return;
            };
            if flow.task.is_some() || delivery.active.is_some() {
                return;
            }
            flow.key.clone()
        };
        if !self.segment_flow_current(&key) {
            let (task, result) = self.retire_segment_flow_locked(&key, "identity_changed");
            if let Some(task) = task {
                let _ = task.await;
            }
            if let Err(error) = result {
                self.error(format!("分句停声状态未保存：{error}"));
            }
            return;
        }
        let foreground = self.memory_worker.foreground();
        let app = self.clone();
        let task = tokio::spawn(async move {
            let _foreground = foreground;
            app.run_segment_speech(key, speaker).await;
        });
        self.speech_delivery
            .lock()
            .unwrap()
            .stream
            .as_mut()
            .unwrap()
            .task = Some(task);
    }

    async fn run_segment_speech<S: SegmentSpeaker>(
        self: Arc<Self>,
        key: AutoSpeechKey,
        speaker: Arc<S>,
    ) {
        loop {
            let work = {
                let _control = self.control.lock().await;
                if !self.segment_flow_matches(&key) {
                    return;
                }
                if !self.segment_flow_current(&key) {
                    self.fail_segment_worker_locked(&key, "identity_changed");
                    return;
                }
                let claimed = self.stores.lock().unwrap()[&key.namespace]
                    .reserve_next_segment_speech(
                        &key.session_id,
                        &key.request_id,
                        key.generation,
                        key.learner_epoch,
                        &uid("speech_segment"),
                        key.audio_epoch,
                    );
                match claimed {
                    Ok(Some(work)) => work,
                    Ok(None) => {
                        // Same control lock as display ACK scheduling: an ACK
                        // cannot be lost between observing no work and retiring.
                        if let Some(flow) = self
                            .speech_delivery
                            .lock()
                            .unwrap()
                            .stream
                            .as_mut()
                            .filter(|f| f.key == key)
                        {
                            flow.task.take();
                        }
                        return;
                    }
                    Err(error) => {
                        self.error(format!("分句播报领取失败：{error}"));
                        self.fail_segment_worker_locked(&key, "receipt_store_failed");
                        return;
                    }
                }
            };
            let (started, receive) = oneshot::channel();
            let result = observe_speech(speaker.speak(&work, started), receive, |receipt| {
                self.segment_started(&key, &work, receipt)
            })
            .await;
            let _control = self.control.lock().await;
            if !self.segment_flow_matches(&key) {
                return;
            }
            if !self.segment_flow_current(&key) {
                self.fail_segment_worker_locked(&key, "identity_changed");
                return;
            }
            match result {
                Ok(outcome) => {
                    let committed = self.stores.lock().unwrap()[&key.namespace]
                        .record_tutor_speech_outcome(&work, &outcome);
                    if let Err(error) = committed {
                        self.error(format!("分句播放终态未保存：{error}"));
                        self.fail_segment_worker_locked(&key, "receipt_store_failed");
                        return;
                    }
                    self.metrics.lock().unwrap()["last_segment_speech_outcome"] = json!({
                        "request_id":key.request_id,"sequence":work.sequence,"receipt":outcome});
                    self.notify("tts.status");
                    if outcome.state != SpeechEnd::Completed {
                        self.fail_segment_worker_locked(&key, "audio_failed");
                        return;
                    }
                }
                Err(error) => {
                    self.error(format!("分句播放未完成：{error}"));
                    self.fail_segment_worker_locked(&key, "audio_failed");
                    return;
                }
            }
        }
    }

    fn fail_segment_worker_locked(&self, key: &AutoSpeechKey, reason: &str) {
        let (_, result) = self.retire_segment_flow_locked(key, reason);
        // Do not await this worker's own handle. Aborting it will release its
        // foreground guard at the next yield; the gate epoch is already stale.
        if let Err(error) = result {
            self.error(format!("分句停声状态未保存：{error}"));
        }
    }

    async fn segment_started(
        &self,
        key: &AutoSpeechKey,
        work: &TutorSpeechWork,
        receipt: SpeechStarted,
    ) {
        let _control = self.control.lock().await;
        if !self.segment_flow_matches(key) {
            return;
        }
        if !self.segment_flow_current(key) {
            self.fail_segment_worker_locked(key, "identity_changed");
            return;
        }
        let saved =
            self.stores.lock().unwrap()[&key.namespace].record_tutor_speech_started(work, &receipt);
        if let Err(error) = saved {
            self.error(format!("分句播放开始未保存：{error}"));
            self.fail_segment_worker_locked(key, "receipt_store_failed");
            return;
        }
        self.metrics.lock().unwrap()["last_segment_speech_started"] = json!({
            "request_id":key.request_id,"sequence":work.sequence,"receipt":receipt});
        self.notify("tts.status");
    }
}

fn stream_allows_voice_installation(speech: &Value, task_active: bool) -> bool {
    !task_active && (speech["speech_gate"] != "open" || speech["speech_complete"] == true)
}

#[cfg(test)]
mod voice_installation_tests {
    use super::*;
    #[test]
    fn silent_gap_and_text_final_cannot_switch_a_reply_voice() {
        let speech = json!({"speech_gate":"open","speech_complete":false,"phase":"completed","final_validated":true});
        assert!(!stream_allows_voice_installation(&speech, false));
    }
    #[test]
    fn completed_or_closed_voice_flow_allows_installation_only_after_worker_retires() {
        let complete = json!({"speech_gate":"open","speech_complete":true});
        assert!(stream_allows_voice_installation(&complete, false));
        assert!(!stream_allows_voice_installation(&complete, true));
        let closed = json!({"speech_gate":"closed","speech_complete":false});
        assert!(stream_allows_voice_installation(&closed, false));
        assert!(!stream_allows_voice_installation(&closed, true));
    }
}
