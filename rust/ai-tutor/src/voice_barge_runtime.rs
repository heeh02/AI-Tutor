//! One explicitly authorized monitor per reply. Onset adopts its existing input;
//! old teaching cleanup is bound to the reply, never to whatever is current later.
use super::*;
use std::sync::atomic::AtomicU64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReplyAck {
    request_id: String,
    generation: i64,
    audio_epoch: u64,
}
impl ReplyAck {
    pub(super) fn parse(value: &Value) -> Result<Self> {
        let ack: Self = serde_json::from_value(value.clone())?;
        ensure!(
            valid_id(&ack.request_id) && ack.generation >= 0 && ack.audio_epoch > 0,
            "教学回执标识无效"
        );
        Ok(ack)
    }
    fn matches(&self, reply: &LoopReply) -> bool {
        self.request_id == reply.request_id
            && self.generation == reply.generation
            && self.audio_epoch == reply.audio_epoch
    }
}

impl Lease {
    fn barge_reply_matches(&self, tag: &LoopTag, reply: &LoopReply) -> bool {
        self.live(Instant::now())
            && self.tag == *tag
            && self.barge_in
            && self.phase == Phase::Reply
            && self.reply.as_ref() == Some(reply)
            && self.reply_acknowledged
            && self.owner.generation == reply.generation
    }
    pub(super) fn claim_barge_candidate(
        &mut self,
        tag: &LoopTag,
        reply: &LoopReply,
        token: u64,
    ) -> Result<()> {
        ensure!(
            self.barge_reply_matches(tag, reply)
                && self.monitoring
                && self.recording == Some(token),
            "语音打断候选已过期"
        );
        // Every asynchronous old speech/provider exit is now unable to revoke
        // this recording through pause_voice_loop_reply.
        self.reply = None;
        self.monitor_pending = false;
        self.monitoring = false;
        self.cooldown = None;
        self.change(Phase::Starting);
        Ok(())
    }
}

struct CaptureGuard {
    audio: Arc<AudioService>,
    token: Arc<AtomicU64>,
    adopted: bool,
}
impl Drop for CaptureGuard {
    fn drop(&mut self) {
        let token = self.token.load(Ordering::SeqCst);
        if !self.adopted && token != 0 {
            let _ = self.audio.discard_recording_expected(token);
        }
    }
}

impl Workstation {
    /// Called only by a validated heartbeat while workstation control is held.
    pub(super) fn barge_heartbeat_locked(
        self: &Arc<Self>,
        tag: &LoopTag,
        snapshot: &Lease,
        ack: Option<&ReplyAck>,
    ) -> Result<()> {
        if (snapshot.monitoring || snapshot.monitor_pending) && snapshot.phase == Phase::Reply {
            if let Some(token) = snapshot.recording {
                self.audio.renew_barge_in_monitor(token)?;
            }
        }
        if snapshot.phase != Phase::Reply || snapshot.reply_acknowledged {
            return Ok(());
        }
        let Some(reply) = &snapshot.reply else {
            return Ok(());
        };
        // Delayed acknowledgements of an older reply grant nothing and cannot
        // revoke a more recent capture/reply belonging to this same lease.
        if !ack.is_some_and(|ack| ack.matches(reply)) {
            return Ok(());
        }
        let already_complete = self.voice_loop_reply_output_ready_locked(reply)?;
        {
            let mut state = self.voice_loop.lock().unwrap();
            let lease = state.live_mut(tag)?;
            ensure!(
                lease.revision == snapshot.revision && lease.reply.as_ref() == Some(reply),
                "语音轮换已变化"
            );
            lease.reply_acknowledged = true;
            lease.revision += 1;
            if already_complete {
                lease.monitor_pending = false;
                lease.output_ready(true, Instant::now());
                return Ok(());
            }
        }
        self.launch_barge_monitor_locked(tag.clone(), reply.clone());
        Ok(())
    }

    fn launch_barge_monitor_locked(self: &Arc<Self>, tag: LoopTag, reply: LoopReply) {
        let weak = Arc::downgrade(self);
        let task_tag = tag.clone();
        let task_reply = reply.clone();
        let task = tokio::spawn(async move {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if app
                .run_barge_monitor(task_tag, task_reply.clone())
                .await
                .is_err()
            {
                app.pause_voice_loop_reply(
                    &task_reply.session_id,
                    &task_reply.request_id,
                    task_reply.generation,
                    "barge_in_failed",
                );
                app.notify("audio.changed");
            }
        });
        let mut state = self.voice_loop.lock().unwrap();
        if state
            .lease
            .as_ref()
            .is_some_and(|lease| lease.barge_reply_matches(&tag, &reply))
        {
            if let Some(old) = state.barge_task.replace(task) {
                old.abort();
            }
        } else {
            task.abort();
        }
    }

    async fn run_barge_monitor(self: &Arc<Self>, tag: LoopTag, reply: LoopReply) -> Result<()> {
        let snapshot = {
            let _control = self.control.lock().await;
            let lease = self.voice_loop.lock().unwrap().live_mut(&tag)?.clone();
            ensure!(
                lease.barge_reply_matches(&tag, &reply),
                "语音打断请求已过期"
            );
            self.voice_loop_context(&lease)?;
            lease
        };
        let token_slot = Arc::new(AtomicU64::new(0));
        let mut guard = CaptureGuard {
            audio: self.audio.clone(),
            token: token_slot.clone(),
            adopted: false,
        };
        let reserve_app = self.clone();
        let reserve_tag = tag.clone();
        let reserve_reply = reply.clone();
        let started = self
            .audio
            .start_barge_in_monitor_reserved(
                InputSelection::Bound(snapshot.input.clone()),
                reply.audio_epoch,
                move |token| {
                    token_slot.store(token, Ordering::SeqCst);
                    let mut state = reserve_app.voice_loop.lock().unwrap();
                    let Ok(lease) = state.live_mut(&reserve_tag) else {
                        return false;
                    };
                    if !lease.barge_reply_matches(&reserve_tag, &reserve_reply)
                        || !lease.monitor_pending
                    {
                        return false;
                    }
                    lease.recording = Some(token);
                    lease.revision += 1;
                    true
                },
            )
            .await?;
        let token = guard.token.load(Ordering::SeqCst);
        ensure!(
            token > 0 && started["recording_generation"] == token,
            "语音打断监听回执不匹配"
        );
        {
            let _control = self.control.lock().await;
            let mut state = self.voice_loop.lock().unwrap();
            let lease = state.live_mut(&tag)?;
            ensure!(
                lease.barge_reply_matches(&tag, &reply) && lease.recording == Some(token),
                "语音打断监听已撤销"
            );
            lease.monitor_pending = false;
            lease.monitoring = true;
            lease.revision += 1;
        }
        self.notify("audio.changed");
        loop {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _control = self.control.lock().await;
            let snapshot = self.voice_loop.lock().unwrap().live_mut(&tag)?.clone();
            ensure!(
                snapshot.barge_reply_matches(&tag, &reply) && snapshot.recording == Some(token),
                "语音打断监听已过期"
            );
            let audio = self.audio.status();
            let monitor = &audio["barge_in"];
            ensure!(
                monitor["recording_generation"] == token
                    && monitor["expected_audio_epoch"] == reply.audio_epoch,
                "语音打断监听归属已变化"
            );
            match s(monitor, "phase") {
                "candidate" => {
                    let adopted = self
                        .adopt_barge_candidate_locked(&tag, &reply, token, &snapshot)
                        .await;
                    if adopted.is_ok() {
                        guard.adopted = true;
                    }
                    drop(_control);
                    // Cancel exactly the old cloud request. Its server worker
                    // owns draining transport cleanup and is never awaited here.
                    let cleanup = self.provider.cancel(&reply.request_id).await;
                    self.notify("state.changed");
                    adopted?;
                    if cleanup.is_err() {
                        self.error("被打断的教学请求清理尚未确认".into());
                    }
                    return Ok(());
                }
                "monitor" => {}
                _ => bail!("语音打断监听不可用"),
            }
            if self.voice_loop_reply_ready_with_monitor_locked(&reply, Some(token))? {
                let stopped = self.audio.stop_barge_in_monitor(token).await?;
                if stopped["onset_pending"] == true {
                    continue;
                }
                let mut state = self.voice_loop.lock().unwrap();
                let lease = state.live_mut(&tag)?;
                ensure!(
                    lease.barge_reply_matches(&tag, &reply) && lease.recording == Some(token),
                    "语音打断监听已撤销"
                );
                lease.monitoring = false;
                lease.monitor_pending = false;
                lease.recording = None;
                lease.output_ready(true, Instant::now());
                self.notify("audio.changed");
                return Ok(());
            }
        }
    }

    pub(super) fn cancel_barge_reply_request_locked(&self, reply: &LoopReply) {
        self.memory_worker.preempt();
        let first = self
            .running
            .lock()
            .unwrap()
            .get(&reply.request_id)
            .filter(|run| run.sid == reply.session_id)
            .map(|run| {
                let first = !*run.cancellation.borrow();
                let _ = run.cancellation.send(true);
                first
            })
            .unwrap_or(false);
        if first {
            self.bump_metric("cancelled");
        }
        self.pending.lock().unwrap().retain(|_, pending| {
            !(pending.sid == reply.session_id
                && pending.generation == reply.generation
                && s(&pending.reply, "request_id") == reply.request_id)
        });
    }

    /// Settle every result after the lease has detached its old reply. All
    /// critical revocation is synchronous and precedes pause's possible abort
    /// of this very monitor task; cleanup must not depend on its next await.
    pub(super) fn settle_claimed_barge_in_locked(
        &self,
        reply: &LoopReply,
        token: u64,
        adoption: Result<()>,
    ) -> Result<()> {
        let closed = self.close_barge_in_speech_locked(reply);
        self.cancel_barge_reply_request_locked(reply);
        let result = adoption.and(closed);
        if result.is_err() {
            self.pause_voice_loop_recording(token, "barge_in_failed");
        }
        result
    }

    async fn adopt_barge_candidate_locked(
        &self,
        tag: &LoopTag,
        reply: &LoopReply,
        token: u64,
        snapshot: &Lease,
    ) -> Result<()> {
        self.voice_loop_context(snapshot)?;
        ensure!(
            self.audio.barge_in_available() && self.audio.speech_epoch() == reply.audio_epoch,
            "被打断的播报已变化"
        );
        let learner = self.stores.lock().unwrap()[&snapshot.owner.namespace]
            .learner(&snapshot.owner.learner_id)?;
        ensure!(
            learner["speech_speed"] == "normal",
            "语音打断语速设置已变化"
        );
        self.voice_loop
            .lock()
            .unwrap()
            .live_mut(tag)?
            .claim_barge_candidate(tag, reply, token)?;
        let promoted = self
            .audio
            .promote_barge_in_capture(token, reply.audio_epoch)
            .await;
        let result = (|| -> Result<()> {
            let mut receipt = promoted?;
            ensure!(
                receipt["adopted"] == true
                    && receipt["input_continuous"] == true
                    && receipt["recording_generation"] == token
                    && receipt["sample_rate"] == 16000
                    && receipt["device_key"] == snapshot.input.device_key
                    && receipt["input_topology_revision"] == snapshot.input.topology_revision
                    && receipt["pre_roll_samples"].as_u64().is_some(),
                "打断录音接管回执无效"
            );
            let mut state = self.voice_loop.lock().unwrap();
            let lease = state.live_mut(tag)?;
            ensure!(
                lease.phase == Phase::Starting
                    && lease.reply.is_none()
                    && lease.recording == Some(token)
                    && lease.owner == snapshot.owner,
                "打断录音接管已撤销"
            );
            self.adopt_barge_in_recording(
                &reply.session_id,
                reply.generation,
                token,
                &snapshot.input,
                &mut receipt,
            )?;
            lease.owner.generation = reply.generation + 1;
            lease.adoption = Some(json!({"kind":"speech_barge_in_v1",
                "from_request_id":reply.request_id,"from_generation":reply.generation,
                "from_audio_epoch":reply.audio_epoch,"session_id":reply.session_id,
                "session_generation":receipt["session_generation"],"language":receipt["language"],
                "recording_generation":token,"device_key":snapshot.input.device_key,
                "input_topology_revision":snapshot.input.topology_revision,
                "pre_roll_samples":receipt["pre_roll_samples"],"sample_rate":16000}));
            lease.change(Phase::Listening);
            Ok(())
        })();
        self.settle_claimed_barge_in_locked(reply, token, result)
    }
}
