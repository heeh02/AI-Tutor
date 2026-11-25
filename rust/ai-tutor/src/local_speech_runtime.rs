//! One explicitly enabled voice lifecycle; no automatic restart loop.
use super::*;
use crate::{
    audio::streaming_tts::wait_cancelled,
    local_speech::{LocalSpeech, LocalSpeechMode, WarmupPolicy, managed::OwnedManagedTts},
};
use std::sync::atomic::AtomicU64;
use tokio::sync::oneshot;

pub(super) struct LocalSpeechRuntime {
    current: Mutex<Option<Arc<LocalSpeech>>>,
    fallback: Mutex<Value>,
    epoch: Arc<AtomicU64>,
    running: AtomicBool,
    task: Mutex<Option<JoinHandle<()>>>,
}
impl Default for LocalSpeechRuntime {
    fn default() -> Self {
        Self {
            current: Mutex::new(None),
            fallback: Mutex::new(json!({"mode":"disabled",
            "phase":"disabled","ready":false,"warmup_completed":false,"active":false})),
            epoch: Arc::new(AtomicU64::new(0)),
            running: AtomicBool::new(false),
            task: Mutex::new(None),
        }
    }
}
impl LocalSpeechRuntime {
    pub(super) fn status(&self) -> Value {
        // Do not combine an earlier preparation status with a later idle flag
        // if the owner retires (possibly with cleanup_failed) during this read.
        let was_running = self.running.load(Ordering::SeqCst);
        let mut status = self
            .current
            .lock()
            .unwrap()
            .as_ref()
            .map(|local| {
                let mut status = local.status();
                status["mode"] = json!(mode_name(local.policy().mode));
                status
            })
            .unwrap_or_else(|| self.fallback.lock().unwrap().clone());
        let busy = was_running || self.running.load(Ordering::SeqCst);
        status["busy"] = json!(busy);
        status["retry_available"] = json!(
            !busy
                && status["phase"] != "cleanup_failed"
                && (status["mode"] != "disabled" || status["phase"] == "configuration_error")
        );
        status
    }
    pub(super) fn preempt(&self) -> u64 {
        self.preempt_current().0
    }
    fn preempt_current(&self) -> (u64, Option<Arc<LocalSpeech>>) {
        // Retry publishes epoch/current/task under this same short lock.
        let _task = self.task.lock().unwrap();
        let stop_nonce = self.epoch.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
        let local = self.current.lock().unwrap().clone();
        if let Some(local) = &local {
            local.cancel_preparation();
            if local.status()["linux_cleanup_confirmed"] == false {
                local.update_lifecycle(json!({"phase":"cleanup_failed","retry_available":false}));
            } else if self.running.load(Ordering::SeqCst) {
                local.update_lifecycle(json!({"phase":"stopping","retry_available":false}));
            }
        }
        (stop_nonce, local)
    }
    fn matches_stop(&self, stop_nonce: u64, local: &Option<Arc<LocalSpeech>>) -> bool {
        let current = self.current.lock().unwrap();
        self.epoch.load(Ordering::SeqCst) == stop_nonce
            && match (current.as_ref(), local.as_ref()) {
                (Some(current), Some(local)) => Arc::ptr_eq(current, local),
                (None, None) => true,
                _ => false,
            }
    }
    // Caller holds task so a Retry cannot publish between this check and update.
    fn drain_failed(&self, stop_nonce: u64, local: &Option<Arc<LocalSpeech>>) {
        if !self.matches_stop(stop_nonce, local) {
            return;
        }
        let fields = json!({"phase":"cleanup_failed","client_deactivation_confirmed":false,
            "linux_cleanup_confirmed":false,"error":"owned lifecycle drain did not complete"});
        if let Some(local) = local {
            local.update_lifecycle(fields);
        } else {
            *self.fallback.lock().unwrap() = fields;
        }
    }
    async fn drain(&self, stop_nonce: u64) {
        let (task, local) = {
            let mut slot = self.task.lock().unwrap();
            if self.epoch.load(Ordering::SeqCst) != stop_nonce {
                return;
            }
            (slot.take(), self.current.lock().unwrap().clone())
        };
        if let Some(mut task) = task {
            match tokio::time::timeout(Duration::from_secs(30), &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    let _slot = self.task.lock().unwrap();
                    self.drain_failed(stop_nonce, &local);
                }
                Err(_) => {
                    let abort_owned = {
                        let _slot = self.task.lock().unwrap();
                        if self.matches_stop(stop_nonce, &local) {
                            // Publish unconfirmed cleanup before abort can clear running.
                            self.drain_failed(stop_nonce, &local);
                            task.abort();
                            true
                        } else {
                            false
                        }
                    };
                    if abort_owned {
                        let _ = task.await;
                    }
                    // A superseding Stop can still wait for this old task's own
                    // cleanup; dropping its handle does not abort another epoch.
                }
            }
        } else {
            // Another Stop may own this epoch's JoinHandle. Do not report an
            // idle lifecycle while that owner's cleanup is still running.
            let finished = tokio::time::timeout(Duration::from_secs(30), async {
                while self.epoch.load(Ordering::SeqCst) == stop_nonce
                    && self.running.load(Ordering::SeqCst)
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .is_ok();
            if !finished {
                let _slot = self.task.lock().unwrap();
                self.drain_failed(stop_nonce, &local);
            }
        }
    }
}
struct RunningVoice(Arc<LocalSpeechRuntime>);
impl Drop for RunningVoice {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::SeqCst);
    }
}

fn mode_name(mode: LocalSpeechMode) -> &'static str {
    match mode {
        LocalSpeechMode::Disabled => "disabled",
        LocalSpeechMode::External => "external",
        LocalSpeechMode::Managed => "managed",
    }
}

fn terminal_phase(
    cancelled: bool,
    deactivation_confirmed: bool,
    cleanup: &Option<Result<Value>>,
) -> &'static str {
    if !deactivation_confirmed || cleanup.as_ref().is_some_and(Result::is_err) {
        "cleanup_failed"
    } else if cancelled {
        "stopped"
    } else {
        "degraded"
    }
}

async fn disable_voice_client(audio: &AudioService) -> Result<()> {
    // A concurrent physical Stop may advance the epoch while the owner is
    // processing this command. Recheck that specific rejection, never a model.
    for _ in 0..3 {
        let receipt = audio.configure_tts(None, audio.speech_epoch()).await?;
        if receipt.applied {
            return Ok(());
        }
        ensure!(
            receipt.reason == "stale_epoch",
            "Audio owner did not confirm voice deactivation"
        );
    }
    bail!("Audio generation kept changing; voice deactivation was not confirmed")
}

impl Workstation {
    pub(super) fn local_speech_status(&self) -> Value {
        let mut status = self.local_speech.status();
        let shutting_down = self.shutting_down.load(Ordering::SeqCst);
        status["shutting_down"] = json!(shutting_down);
        if shutting_down {
            status["retry_available"] = json!(false);
        }
        status
    }
    /// main calls this only after its exclusive project lock, listener and
    /// descriptor exist. Preparation never delays opening the SAPI workbench.
    pub fn start_local_speech_runtime(self: &Arc<Self>) {
        if let Err(error) = self.queue_local_speech() {
            self.error(format!("本地语音暂未启用：{error}"));
        }
    }
    fn queue_local_speech(self: &Arc<Self>) -> Result<Value> {
        // Read both runtimes before taking this role's task lock. The pin lock
        // covers admission through publication, but never model work or notify.
        let mut gpu_queue = self.gpu_queue.lock().unwrap();
        let speech_state = self.local_speech.status();
        let asr_state = self.final_asr.status();
        gpu_queue.refresh_if_idle(&speech_state, &asr_state);
        let mut task_slot = self.local_speech.task.lock().unwrap();
        ensure!(!self.shutting_down.load(Ordering::SeqCst), "工作台正在关闭");
        ensure!(
            !self.local_speech.running.load(Ordering::SeqCst),
            "本地语音任务仍在运行或收尾"
        );
        let previous = self.local_speech.status();
        ensure!(
            previous["phase"] != "cleanup_failed" && previous["linux_cleanup_confirmed"] != false,
            "本地语音进程退出尚未确认"
        );
        // Publish all outcomes (including a disabled/invalid new policy) as a
        // new epoch before exposing a replacement LocalSpeech to old Stop calls.
        let epoch = self
            .local_speech
            .epoch
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        let local = match LocalSpeech::new(&self.root) {
            Ok(local) => Arc::new(local),
            Err(_) => {
                *self.local_speech.current.lock().unwrap() = None;
                *self.local_speech.fallback.lock().unwrap() = json!({"mode":"disabled","phase":"configuration_error",
                    "ready":false,"warmup_completed":false,"active":false,"error":"local speech policy could not be loaded"});
                bail!("请检查本地语音策略；系统语音仍可使用");
            }
        };
        let mode = local.policy().mode;
        if mode == LocalSpeechMode::Managed {
            let config = local.policy().managed.as_ref().unwrap();
            gpu_queue.admit(&config.distribution, &speech_state, &asr_state)?;
        }
        local.update_lifecycle(json!({"mode":mode_name(mode),"active":false}));
        *self.local_speech.current.lock().unwrap() = Some(local.clone());
        if mode == LocalSpeechMode::Disabled {
            return Ok(json!({"ok":true,"queued":false,"mode":"disabled"}));
        }
        ensure!(
            local.policy().warmup == WarmupPolicy::SyntheticBilingual,
            "本地语音需要明确启用固定中英双语准备"
        );
        ensure!(
            self.local_speech
                .running
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok(),
            "本地语音任务仍在运行或收尾"
        );
        let app = self.clone();
        let (send, receive) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _running = RunningVoice(app.local_speech.clone());
            if receive.await.is_ok() {
                app.run_local_speech(local, epoch).await;
            }
        });
        task_slot.replace(task);
        let _ = send.send(());
        drop(task_slot);
        drop(gpu_queue);
        self.notify("tts.status");
        Ok(json!({"ok":true,"queued":true,"mode":mode_name(mode)}))
    }
    pub(super) async fn retry_local_speech(self: &Arc<Self>) -> Result<Value> {
        let _control = self.control.lock().await;
        self.queue_local_speech()
    }
    pub(super) async fn stop_local_speech(&self) -> Result<Value> {
        let (stop_nonce, local) = self.local_speech.preempt_current();
        {
            let _task = self.local_speech.task.lock().unwrap();
            if self.local_speech.matches_stop(stop_nonce, &local) {
                self.audio.reserve_speech_epoch();
            }
        }
        let stopped = {
            let _control = self.control.lock().await;
            if self.local_speech.matches_stop(stop_nonce, &local) {
                self.stop_playback_locked_reason("local_voice_stopped")
                    .await
            } else {
                Ok(())
            }
        };
        self.local_speech.drain(stop_nonce).await;
        {
            let _control = self.control.lock().await;
            // Retry also takes control before publication, so a stale Stop
            // cannot clear a newly installed client while awaiting its ACK.
            if self.local_speech.matches_stop(stop_nonce, &local)
                && !self.local_speech.running.load(Ordering::SeqCst)
            {
                if !self.audio.status()["tts_configured_engine"].is_null() {
                    disable_voice_client(&self.audio).await?;
                }
                let _task = self.local_speech.task.lock().unwrap();
                if self.local_speech.matches_stop(stop_nonce, &local)
                    && let Some(local) = &local
                {
                    local.update_lifecycle(
                        json!({"active":false,"client_deactivation_confirmed":true}),
                    );
                    if local.status()["linux_cleanup_confirmed"] == false {
                        local.update_lifecycle(json!({"phase":"cleanup_failed"}));
                    } else {
                        local.update_lifecycle(json!({"phase":"stopped","error":null}));
                    }
                }
            }
        }
        stopped?;
        self.notify("tts.status");
        let status = self.local_speech_status();
        if self.local_speech.matches_stop(stop_nonce, &local) {
            ensure!(
                status["phase"] != "cleanup_failed"
                    && status["linux_cleanup_confirmed"] != false
                    && status["busy"] == false,
                "本地语音清理尚未确认；请检查状态"
            );
        }
        Ok(status)
    }
    pub(super) async fn close_local_speech(&self) {
        let stop_nonce = self.local_speech.preempt();
        self.local_speech.drain(stop_nonce).await;
    }

    async fn run_local_speech(self: Arc<Self>, local: Arc<LocalSpeech>, epoch: u64) {
        let mut owned = None;
        let mut installed = false;
        let result = self
            .prepare_and_install_voice(&local, epoch, &mut owned, &mut installed)
            .await;
        let cancelled = self.local_speech.epoch.load(Ordering::SeqCst) != epoch
            || self.shutting_down.load(Ordering::SeqCst);
        // Cancel the physical epoch before waiting for a teaching lock. A failed
        // background preparation that never installed cannot interrupt SAPI.
        let mut deactivation_confirmed = !installed;
        if installed {
            self.audio.reserve_speech_epoch();
            let _control = self.control.lock().await;
            let _ = self
                .stop_playback_locked_reason("local_voice_stopped")
                .await;
            // No database or status lock crosses this bounded owner confirmation.
            // Holding control prevents a new reply from starting on a dead client.
            match disable_voice_client(&self.audio).await {
                Ok(()) => deactivation_confirmed = true,
                Err(error) => self.error(format!("本地语音停用确认失败：{error}")),
            }
        }
        let cleanup = if let Some(owned) = &mut owned {
            Some(owned.stop().await)
        } else {
            None
        };
        let phase = terminal_phase(cancelled, deactivation_confirmed, &cleanup);
        let cleanup_failed = phase == "cleanup_failed";
        local.update_lifecycle(json!({"mode":mode_name(local.policy().mode),"active":!deactivation_confirmed,"ready":false,
            "client_deactivation_confirmed":deactivation_confirmed,
            "phase":phase,
            "retry_available":!self.shutting_down.load(Ordering::SeqCst)}));
        if let Some(cleanup) = cleanup {
            match cleanup {
                Ok(receipt) => local.update_lifecycle(json!({"ownership_stop":receipt,"linux_cleanup_confirmed":true})),
                Err(_) => local.update_lifecycle(json!({"linux_cleanup_confirmed":false,"error":"owned Linux cleanup was not confirmed"})),
            }
        }
        if !cancelled && result.is_err() && !cleanup_failed {
            // Fixed diagnostic text avoids leaking subprocess credentials/logs.
            local.update_lifecycle(json!({"error":"local voice preparation or service failed; explicit retry required"}));
        }
        self.notify("tts.status");
    }

    async fn prepare_and_install_voice(
        self: &Arc<Self>,
        local: &Arc<LocalSpeech>,
        epoch: u64,
        owned: &mut Option<OwnedManagedTts>,
        installed: &mut bool,
    ) -> Result<()> {
        let prepared = match local.policy().mode {
            LocalSpeechMode::Managed => {
                crate::local_speech::managed_prepare::prepare_owned_managed(
                    &self.root,
                    local,
                    owned,
                    &self.local_speech.epoch,
                    epoch,
                    || self.notify("tts.status"),
                )
                .await?
            }
            LocalSpeechMode::External => tokio::select! { biased;
                _=wait_cancelled(&self.local_speech.epoch,epoch)=>bail!("External voice preparation cancelled"),
                result=local.prepare_external_tts()=>result?,
            },
            LocalSpeechMode::Disabled => bail!("Local voice is disabled"),
        };
        local.update_lifecycle(json!({"mode":mode_name(local.policy().mode),"active":false,
            "phase":"warm_ready_pending_install"}));
        self.notify("tts.status");
        loop {
            ensure!(
                self.local_speech.epoch.load(Ordering::SeqCst) == epoch
                    && !self.shutting_down.load(Ordering::SeqCst),
                "Local voice installation cancelled"
            );
            if let Some(service) = owned.as_mut() {
                service.poll_running()?;
            }
            let audio_epoch = {
                let _control = self.control.lock().await;
                self.local_voice_installation_epoch_locked()?
            };
            if let Some(audio_epoch) = audio_epoch {
                let result = local
                    .install_prepared(&self.audio, &prepared, audio_epoch)
                    .await;
                if result.is_err() && local.status()["phase"] == "active" {
                    *installed = true;
                }
                let receipt = result?;
                if receipt.applied {
                    *installed = true;
                    local.update_lifecycle(json!({"mode":mode_name(local.policy().mode),"phase":"active","active":true}));
                    self.notify("tts.status");
                    break;
                }
                ensure!(
                    ["busy", "stale_epoch"].contains(&receipt.reason),
                    "Voice owner rejected installation"
                );
            }
            tokio::select! { biased;
                _=wait_cancelled(&self.local_speech.epoch,epoch)=>bail!("Local voice installation cancelled"),
                _=tokio::time::sleep(Duration::from_millis(250))=>{},
            }
        }
        loop {
            tokio::select! { biased;
                _=wait_cancelled(&self.local_speech.epoch,epoch)=>return Ok(()),
                _=tokio::time::sleep(Duration::from_millis(500))=>{
                    if let Some(service) = owned.as_mut() { service.poll_running()?; }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_speech::{ManagedSpeechConfig, managed_prepare::managed_client};

    #[tokio::test(flavor = "current_thread")]
    async fn workstation_queue_rejects_changed_disk_distribution_while_captured_peer_runs()
    -> Result<()> {
        let root = tempfile::tempdir()?;
        let runtime = root.path().join("runtime");
        std::fs::create_dir_all(&runtime)?;
        std::fs::write(
            runtime.join("gpu-resource-policy.json"),
            r#"{"protocol_version":1,"mode":"qwen_pair_12gb_v1"}"#,
        )?;
        let write_policies = |distribution: &str| -> Result<()> {
            std::fs::write(
                runtime.join("local-speech-policy.json"),
                serde_json::to_vec(&json!({
                "protocol_version":1,"mode":"managed","warmup":"synthetic_bilingual",
                "managed":{"engine":"qwen","distribution":distribution}}))?,
            )?;
            std::fs::write(
                runtime.join("local-final-asr-policy.json"),
                serde_json::to_vec(&json!({
                "protocol_version":1,"enabled":true,"managed":{"distribution":distribution,
                    "python_executable":"/not-started/python","cache_directory":"/not-started/cache"}}))?,
            )?;
            Ok(())
        };
        write_policies("Ubuntu")?;
        let peer = Arc::new(LocalSpeech::new(root.path())?);
        let captured = peer.policy().managed.as_ref().unwrap().distribution.clone();
        let mut app = Workstation::new(root.path().into(), "queue-pin-fixture".into())?;
        // Retire the standard fixture's idle owners, then use no device owner.
        app.audio.close().await;
        let (audio, _owner) = AudioService::configuration_test_channel(0);
        Arc::get_mut(&mut app).unwrap().audio = Arc::new(audio);
        let idle = json!({"phase":"disabled","busy":false,"active":false});
        app.gpu_queue
            .lock()
            .unwrap()
            .admit(&captured, &idle, &idle)?;
        *app.local_speech.current.lock().unwrap() = Some(peer.clone());
        app.local_speech.running.store(true, Ordering::SeqCst);
        write_policies("devbox")?;
        assert!(crate::gpu_resource_policy::load(root.path()).is_ok());
        // Even a regression in the pin cannot launch WSL: prepare checks the
        // missing installed bundle before conversion/spawn. No weights exist.
        assert!(!root.path().join("bin/final-asr").exists());
        assert!(!root.path().join("models").exists());
        for phase in ["loading", "active"] {
            peer.update_lifecycle(json!({"phase":phase,"active":phase=="active"}));
            let result = app.retry_final_asr().await;
            if result.is_ok() {
                let _ = app.stop_final_asr().await;
            }
            assert!(result.is_err(), "new distribution must not be queued");
            assert!(result.unwrap_err().to_string().contains("WSL"));
            assert_eq!(app.final_asr_status()["busy"], false);
            assert_eq!(
                peer.policy().managed.as_ref().unwrap().distribution,
                "Ubuntu"
            );
        }
        app.local_speech.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    #[tokio::test]
    async fn old_voice_stop_cannot_take_or_abort_a_new_retry_task() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let runtime = LocalSpeechRuntime::default();
        let old = Arc::new(LocalSpeech::new(dir.path())?);
        *runtime.current.lock().unwrap() = Some(old);
        let (old_stop, old_local) = runtime.preempt_current();
        let newer = Arc::new(LocalSpeech::new(dir.path())?);
        newer.update_lifecycle(json!({"phase":"active","active":true,"ready":true}));
        let (finish, pending) = oneshot::channel::<()>();
        {
            let mut slot = runtime.task.lock().unwrap();
            runtime
                .epoch
                .store(old_stop.wrapping_add(1), Ordering::SeqCst);
            *runtime.current.lock().unwrap() = Some(newer.clone());
            runtime.running.store(true, Ordering::SeqCst);
            *slot = Some(tokio::spawn(async move {
                let _ = pending.await;
            }));
        }
        tokio::time::timeout(Duration::from_secs(1), runtime.drain(old_stop)).await?;
        assert!(!runtime.matches_stop(old_stop, &old_local));
        assert!(runtime.running.load(Ordering::SeqCst));
        assert_eq!(newer.status()["phase"], "active");
        let task = runtime
            .task
            .lock()
            .unwrap()
            .take()
            .expect("old drain must not take new task");
        assert!(!task.is_finished());
        let _ = finish.send(());
        task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn voice_drain_without_handle_waits_for_the_same_epoch_owner() -> Result<()> {
        let runtime = LocalSpeechRuntime::default();
        runtime.epoch.store(7, Ordering::SeqCst);
        runtime.running.store(true, Ordering::SeqCst);
        let waiting = runtime.drain(7);
        tokio::pin!(waiting);
        assert!(futures_util::poll!(waiting.as_mut()).is_pending());
        runtime.running.store(false, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(1), waiting).await?;

        runtime.running.store(true, Ordering::SeqCst);
        let superseded = runtime.drain(7);
        tokio::pin!(superseded);
        assert!(futures_util::poll!(superseded.as_mut()).is_pending());
        runtime.epoch.store(8, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(1), superseded).await?;
        assert!(runtime.running.load(Ordering::SeqCst));
        Ok(())
    }

    #[test]
    fn voice_stop_failure_update_requires_nonce_and_original_local_arc() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let runtime = LocalSpeechRuntime::default();
        let old = Arc::new(LocalSpeech::new(dir.path())?);
        let newer = Arc::new(LocalSpeech::new(dir.path())?);
        newer.update_lifecycle(
            json!({"phase":"active","active":true,"linux_cleanup_confirmed":true}),
        );
        runtime.epoch.store(4, Ordering::SeqCst);
        *runtime.current.lock().unwrap() = Some(newer.clone());
        let _slot = runtime.task.lock().unwrap();
        runtime.drain_failed(4, &Some(old));
        assert_eq!(newer.status()["phase"], "active");
        runtime.drain_failed(3, &Some(newer.clone()));
        assert_eq!(newer.status()["linux_cleanup_confirmed"], true);
        runtime.drain_failed(4, &Some(newer.clone()));
        assert_eq!(newer.status()["phase"], "cleanup_failed");
        assert_eq!(newer.status()["linux_cleanup_confirmed"], false);
        Ok(())
    }

    #[test]
    fn disabled_runtime_does_not_create_a_task_or_claim_a_ready_voice() {
        let runtime = LocalSpeechRuntime::default();
        assert_eq!(runtime.status()["phase"], "disabled");
        assert_eq!(runtime.status()["active"], false);
        assert!(runtime.task.lock().unwrap().is_none());
        assert!(!runtime.running.load(Ordering::SeqCst));
        assert_eq!(runtime.status()["busy"], false);
        assert_eq!(runtime.status()["retry_available"], false);
    }
    #[test]
    fn managed_qwen_configuration_stays_in_memory_and_has_a_fixed_endpoint() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let config = ManagedSpeechConfig {
            engine: "qwen".into(),
            distribution: "Ubuntu".into(),
            references_file: None,
        };
        let client = managed_client(
            dir.path(),
            &config,
            "only-a-synthetic-test-credential-123456",
        )?;
        assert_eq!(client.engine_kind(), "qwen_pcm");
        assert!(!dir.path().join("runtime/tts-service.json").exists());
        assert_eq!(std::fs::read_dir(dir.path())?.count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn deactivation_rechecks_a_stale_epoch_instead_of_claiming_the_client_was_removed()
    -> Result<()> {
        let dir = tempfile::tempdir()?;
        let config = ManagedSpeechConfig {
            engine: "qwen".into(),
            distribution: "Ubuntu".into(),
            references_file: None,
        };
        let original = managed_client(
            dir.path(),
            &config,
            "only-a-synthetic-test-credential-123456",
        )?;
        let mut slot = Some(original.clone());
        let (audio, owner) = AudioService::configuration_test_channel(7);
        let deactivation = disable_voice_client(&audio);
        tokio::pin!(deactivation);
        assert!(futures_util::poll!(deactivation.as_mut()).is_pending());
        let pending = owner.take()?;
        let newer = audio.reserve_speech_epoch();
        let rejected = pending.commit(&mut slot, newer);
        assert!(!rejected.applied);
        assert!(Arc::ptr_eq(slot.as_ref().unwrap(), &original));
        assert!(futures_util::poll!(deactivation.as_mut()).is_pending());
        assert!(owner.take()?.commit(&mut slot, newer).applied);
        deactivation.await?;
        assert!(slot.is_none());
        Ok(())
    }

    #[test]
    fn retry_status_excludes_live_tasks_and_unconfirmed_linux_cleanup() {
        let runtime = LocalSpeechRuntime::default();
        *runtime.fallback.lock().unwrap() =
            json!({"phase":"configuration_error","mode":"disabled"});
        assert_eq!(runtime.status()["retry_available"], true);
        runtime.running.store(true, Ordering::SeqCst);
        assert_eq!(runtime.status()["retry_available"], false);
        runtime.running.store(false, Ordering::SeqCst);
        *runtime.fallback.lock().unwrap() = json!({"phase":"cleanup_failed","mode":"managed"});
        assert_eq!(runtime.status()["retry_available"], false);
    }

    #[test]
    fn preflight_cleanup_proof_allows_explicit_retry_but_cleanup_failure_does_not() {
        let runtime = LocalSpeechRuntime::default();
        let confirmed = Some(Ok(
            json!({"linux_cleanup_confirmed":true,"normal_owner_stop":false,
            "receipt":{"state":"failed","process_started":false,"linux_cleanup_confirmed":true}}),
        ));
        let phase = terminal_phase(false, true, &confirmed);
        *runtime.fallback.lock().unwrap() = json!({"mode":"managed","phase":phase,"active":false});
        assert_eq!(runtime.status()["phase"], "degraded");
        assert_eq!(runtime.status()["retry_available"], true);
        let unconfirmed = Some(Err(anyhow!("owned cleanup was not confirmed")));
        let phase = terminal_phase(false, true, &unconfirmed);
        *runtime.fallback.lock().unwrap() = json!({"mode":"managed","phase":phase,"active":false});
        assert_eq!(runtime.status()["phase"], "cleanup_failed");
        assert_eq!(runtime.status()["retry_available"], false);
    }
}
