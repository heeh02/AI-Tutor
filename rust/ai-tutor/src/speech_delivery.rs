//! Server playback ownership. Queueing is not evidence that speech has started.
use super::*;
use crate::audio::{SpeechEnd, SpeechOutcome, SpeechStarted};
use std::future::Future;
use tokio::sync::oneshot;
#[path = "segment_speech.rs"]
mod segment_speech;

#[derive(Clone)]
struct PlaybackIdentity {
    attempt_id: String,
    namespace: String,
    learner_id: String,
    session_id: String,
    turn_id: String,
    generation: i64,
    learner_epoch: i64,
    audio_epoch: u64,
}
struct PlaybackBinding {
    identity: PlaybackIdentity,
    started: Option<SpeechStarted>,
    task: Option<JoinHandle<()>>,
}
#[derive(Default)]
pub(super) struct SpeechDelivery {
    active: Option<PlaybackBinding>,
    stream: Option<segment_speech::AutoSpeechFlow>,
}
impl SpeechDelivery {
    fn matches(&self, id: &PlaybackIdentity) -> bool {
        self.active
            .as_ref()
            .is_some_and(|b| b.identity.attempt_id == id.attempt_id)
    }
    fn take(&mut self, id: &PlaybackIdentity) -> Option<PlaybackBinding> {
        if self.matches(id) {
            self.active.take()
        } else {
            None
        }
    }
}

fn identity_is_current(
    store: &Store,
    current: &Option<(String, String)>,
    audio_epoch: u64,
    id: &PlaybackIdentity,
) -> bool {
    if current.as_ref() != Some(&(id.namespace.clone(), id.session_id.clone()))
        || store.namespace != id.namespace
        || audio_epoch != id.audio_epoch
    {
        return false;
    }
    let Ok(session) = store.session(&id.session_id) else {
        return false;
    };
    let Ok(learner) = store.learner(&id.learner_id) else {
        return false;
    };
    s(&session, "learner_id") == id.learner_id
        && session["generation"] == id.generation
        && learner["epoch"] == id.learner_epoch
}

fn accept_started(binding: &mut PlaybackBinding, receipt: SpeechStarted) -> bool {
    if binding.started.is_some() || receipt.epoch != binding.identity.audio_epoch {
        return false;
    }
    binding.started = Some(receipt);
    true
}

fn terminal_status(
    binding: &PlaybackBinding,
    result: &Result<SpeechOutcome>,
) -> Option<&'static str> {
    let started = binding.started.as_ref()?;
    match result {
        Ok(outcome)
            if outcome.playback_id == started.playback_id
                && outcome.epoch == binding.identity.audio_epoch
                && outcome.engine_kind == started.engine_kind =>
        {
            Some(match outcome.state {
                SpeechEnd::Completed => "speech_completed",
                SpeechEnd::Interrupted => "speech_interrupted",
            })
        }
        Ok(_) => None,
        Err(_) => Some("speech_interrupted"),
    }
}

// Poll synthesis while awaiting its start receipt. An immediately ready outcome
// may have sent both receipts during this same poll; consume that start first.
async fn observe_speech<F, H, HF>(
    outcome: F,
    mut started: oneshot::Receiver<SpeechStarted>,
    mut on_start: H,
) -> Result<SpeechOutcome>
where
    F: Future<Output = Result<SpeechOutcome>>,
    H: FnMut(SpeechStarted) -> HF,
    HF: Future<Output = ()>,
{
    tokio::pin!(outcome);
    tokio::select! {
        biased;
        receipt = &mut started => {
            if let Ok(receipt) = receipt { on_start(receipt).await; }
            outcome.await
        }
        result = &mut outcome => {
            if let Ok(receipt) = started.try_recv() { on_start(receipt).await; }
            result
        }
    }
}

impl Workstation {
    pub(super) async fn playback(self: &Arc<Self>, sid: &str, tid: &str) -> Result<Value> {
        let foreground = self.memory_worker.foreground();
        let _control = self.control.lock().await;
        self.pause_voice_loop(None, "manual_playback");
        ensure!(!self.shutting_down.load(Ordering::SeqCst), "工作台正在关闭");
        let (namespace, session) = self.writable(sid)?;
        ensure!(s(&session, "status") != "generating", "请先完成当前生成");
        let (text, language, speed, learner_epoch) = {
            let stores = self.stores.lock().unwrap();
            let store = &stores[&namespace];
            let turn = session["turns"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| s(t, "turn_id") == tid && s(t, "role") == "assistant")
                .ok_or_else(|| anyhow!("回复不存在"))?;
            ensure!(
                turn["delivery_mode"] != "segments" || turn["stream_phase"] == "completed",
                "分句回复尚未完整验证和显示"
            );
            ensure!(
                ["shown", "speech_completed", "speech_interrupted"].contains(&s(turn, "status")),
                "回复尚未显示或已过期"
            );
            ensure!(turn["generation"] == session["generation"], "回复已过期");
            let learner = store.learner(s(&session, "learner_id"))?;
            (
                s(turn, "content").to_owned(),
                s(&learner, "language").to_owned(),
                learner["speech_speed"]
                    .as_str()
                    .unwrap_or("normal")
                    .to_owned(),
                learner["epoch"].as_i64().unwrap_or(0),
            )
        };
        ensure!(
            !self
                .speech_delivery
                .lock()
                .unwrap()
                .active
                .as_ref()
                .is_some_and(|b| b.identity.namespace == namespace
                    && b.identity.session_id == sid
                    && b.identity.turn_id == tid),
            "该回复正在等待或进行播报"
        );
        // Unique epochs also reject an old spawned task that reaches the audio
        // queue after its replacement. No device or database lock crosses await.
        self.stop_segment_speech_locked("manual_playback").await?;
        let audio_epoch = self.audio.reserve_speech_epoch();
        let _ = self.invalidate_playback_locked();
        let identity = PlaybackIdentity {
            attempt_id: uid("playback"),
            namespace,
            learner_id: s(&session, "learner_id").to_owned(),
            session_id: sid.to_owned(),
            turn_id: tid.to_owned(),
            generation: session["generation"].as_i64().unwrap_or(-1),
            learner_epoch,
            audio_epoch,
        };
        let response = json!({"ok":true,"playback_attempt_id":identity.attempt_id,"playback_epoch":audio_epoch,"state":"queued"});
        self.speech_delivery.lock().unwrap().active = Some(PlaybackBinding {
            identity: identity.clone(),
            started: None,
            task: None,
        });
        let app = self.clone();
        let task_identity = identity.clone();
        let task = tokio::spawn(async move {
            let _foreground = foreground;
            let (started, received) = oneshot::channel();
            let result = observe_speech(
                app.audio
                    .speak_with_receipts(text, language, &speed, audio_epoch, started),
                received,
                |receipt| app.playback_started(&task_identity, receipt),
            )
            .await;
            app.playback_finished(&task_identity, result).await;
        });
        // The task cannot consume receipts under control until this reservation
        // contains its abort handle, including on a multi-threaded runtime.
        self.speech_delivery
            .lock()
            .unwrap()
            .active
            .as_mut()
            .unwrap()
            .task = Some(task);
        Ok(response)
    }

    async fn playback_started(&self, identity: &PlaybackIdentity, receipt: SpeechStarted) {
        let _control = self.control.lock().await;
        let mut delivery = self.speech_delivery.lock().unwrap();
        if !delivery.matches(identity) {
            return;
        }
        let stores = self.stores.lock().unwrap();
        let store = &stores[&identity.namespace];
        if !identity_is_current(
            store,
            &self.current.lock().unwrap(),
            self.audio.speech_epoch(),
            identity,
        ) {
            return;
        }
        let binding = delivery.active.as_mut().unwrap();
        if accept_started(binding, receipt) {
            if let Err(error) =
                store.acknowledge_speech(&identity.session_id, &identity.turn_id, "speech_started")
            {
                self.error(format!("播报开始状态未保存：{error}"));
            }
            self.metrics.lock().unwrap()["last_speech_started"] = json!(binding.started);
            self.notify("tts.status");
        }
    }

    async fn playback_finished(&self, identity: &PlaybackIdentity, result: Result<SpeechOutcome>) {
        let _control = self.control.lock().await;
        let Some(binding) = self.speech_delivery.lock().unwrap().take(identity) else {
            return;
        };
        let stores = self.stores.lock().unwrap();
        let store = &stores[&identity.namespace];
        if !identity_is_current(
            store,
            &self.current.lock().unwrap(),
            self.audio.speech_epoch(),
            identity,
        ) {
            return;
        }
        if let Some(status) = terminal_status(&binding, &result) {
            if let Err(error) =
                store.acknowledge_speech(&identity.session_id, &identity.turn_id, status)
            {
                self.error(format!("播报结束状态未保存：{error}"));
            }
        }
        match result {
            Ok(outcome) => self.metrics.lock().unwrap()["last_speech_outcome"] = json!(outcome),
            Err(error) => self.error(format!("本地播报未完成：{error}")),
        }
        self.notify("tts.status");
    }

    /// Caller holds control and invalidates the audio epoch before starting a
    /// replacement. Aborting this waiter releases its foreground guard; audio
    /// ownership remains in the speech owner and observes the invalidated epoch.
    pub(super) fn invalidate_playback_locked(&self) -> Option<JoinHandle<()>> {
        let binding = self.speech_delivery.lock().unwrap().active.take()?;
        if let Some(task) = &binding.task {
            task.abort();
        }
        if binding.started.is_some() {
            let stores = self.stores.lock().unwrap();
            let store = &stores[&binding.identity.namespace];
            // Cancellation owns this exact binding. Generation validation in
            // Store prevents an old interruption from modifying a newer turn.
            if let Err(error) = store.acknowledge_speech(
                &binding.identity.session_id,
                &binding.identity.turn_id,
                "speech_interrupted",
            ) {
                self.error(format!("播报打断状态未保存：{error}"));
            }
        }
        self.notify("tts.status");
        binding.task
    }

    pub(super) async fn stop_playback(&self) -> Result<()> {
        // Physical cancellation cannot wait behind provider/teaching work.
        self.audio.reserve_speech_epoch();
        let _control = self.control.lock().await;
        self.stop_playback_locked().await
    }

    pub(super) async fn stop_playback_locked(&self) -> Result<()> {
        self.stop_playback_locked_reason("user").await
    }
    pub(super) async fn stop_playback_locked_reason(&self, reason: &str) -> Result<()> {
        self.audio.reserve_speech_epoch();
        let task = self.invalidate_playback_locked();
        let stream_result = self.stop_segment_speech_locked(reason).await;
        let result = self.audio.stop_speech().await;
        if let Some(task) = task {
            let _ = task.await;
        }
        stream_result.and(result)
    }
}

#[cfg(test)]
#[path = "speech_delivery_tests.rs"]
mod tests;
