//! Optional GPU final lifecycle, independent of the CPU partial stream.
use super::*;
use crate::final_asr::{
    FinalAsrClient, FinalAsrConfig, OwnedFinalAsrService, load_policy as policy,
};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_policy_does_not_adopt_an_external_service() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("runtime"))?;
        std::fs::write(root.path().join("runtime/speech-service.json"), "external")?;
        assert!(policy(root.path())?.is_none());
        std::fs::write(
            root.path().join("runtime/local-final-asr-policy.json"),
            r#"{"protocol_version":1,"enabled":false}"#,
        )?;
        assert!(policy(root.path())?.is_none());
        std::fs::write(
            root.path().join("runtime/local-final-asr-policy.json"),
            r#"{"protocol_version":1,"enabled":true}"#,
        )?;
        assert!(policy(root.path()).is_err());
        Ok(())
    }
    #[test]
    fn canceled_preparation_cannot_install_and_old_finish_cannot_clear_new_epoch() -> Result<()> {
        let runtime = FinalAsrRuntime::default();
        let (audio, _) = AudioService::configuration_test_channel(0);
        {
            let mut c = runtime.control.lock().unwrap();
            c.epoch = 1;
            c.running = Some(1);
        }
        let _ = runtime.preempt(&audio);
        let client = Arc::new(FinalAsrClient::new(
            "a".repeat(32),
            "http://127.0.0.1:49152".into(),
            "b".repeat(32),
        )?);
        assert!(!runtime.install(1, &audio, client.clone()));
        {
            let mut c = runtime.control.lock().unwrap();
            c.epoch = 3;
            c.running = Some(3);
        }
        assert!(runtime.install(3, &audio, client.clone()));
        runtime.finish(1, &audio, Some(json!({"linux_cleanup_confirmed":false})));
        assert_eq!(runtime.status()["phase"], "active");
        assert!(!client.unavailable());
        let _ = runtime.preempt(&audio);
        assert!(client.unavailable());
        runtime.finish(3, &audio, Some(json!({"linux_cleanup_confirmed":false})));
        assert_eq!(runtime.status()["phase"], "cleanup_failed");
        assert_eq!(runtime.status()["retry_available"], false);
        Ok(())
    }
    #[tokio::test]
    async fn old_stop_cannot_take_a_new_preparation_task() {
        let runtime = FinalAsrRuntime::default();
        let (send, receive) = tokio::sync::oneshot::channel::<()>();
        runtime.control.lock().unwrap().running = Some(2);
        *runtime.task.lock().unwrap() = Some((
            2,
            tokio::spawn(async move {
                let _ = receive.await;
            }),
        ));
        runtime.drain(Some(1)).await;
        assert_eq!(runtime.control.lock().unwrap().running, Some(2));
        let (epoch, task) = runtime.task.lock().unwrap().take().unwrap();
        assert_eq!(epoch, 2);
        assert!(!task.is_finished());
        let _ = send.send(());
        task.await.unwrap();
    }
    #[test]
    fn old_stop_response_does_not_mislabel_a_new_retry_as_failed_cleanup() -> Result<()> {
        let runtime = FinalAsrRuntime::default();
        let (audio, _) = AudioService::configuration_test_channel(0);
        {
            let mut control = runtime.control.lock().unwrap();
            control.epoch = 1;
            control.running = Some(1);
        }
        let (target, stop_nonce) = runtime.preempt_with_nonce(&audio);
        assert_eq!(target, Some(1));
        runtime.finish(1, &audio, Some(json!({"linux_cleanup_confirmed":true})));
        assert_eq!(
            runtime.stopped_status(stop_nonce)?["stop_superseded"],
            false
        );
        {
            let mut control = runtime.control.lock().unwrap();
            control.epoch = stop_nonce.wrapping_add(1);
            control.running = Some(control.epoch);
            control.status =
                json!({"phase":"starting","active":false,"ready":false,"warmup_completed":false});
        }
        let current = runtime.stopped_status(stop_nonce)?;
        assert_eq!(current["phase"], "starting");
        assert_eq!(current["busy"], true);
        assert_eq!(current["stop_superseded"], true);
        assert!(current.get("ownership_stop").is_none());
        assert!(runtime.stopped_status(stop_nonce.wrapping_add(1)).is_err());
        Ok(())
    }
}

struct Control {
    epoch: u64,
    running: Option<u64>,
    cancel: Option<watch::Sender<bool>>,
    instance: Option<String>,
    status: Value,
}
pub(super) struct FinalAsrRuntime {
    control: Mutex<Control>,
    task: Mutex<Option<(u64, JoinHandle<()>)>>,
}
impl Default for FinalAsrRuntime {
    fn default() -> Self {
        Self {
            control: Mutex::new(Control {
                epoch: 0,
                running: None,
                cancel: None,
                instance: None,
                status: json!({"phase":"disabled","active":false,"ready":false,"warmup_completed":false}),
            }),
            task: Mutex::new(None),
        }
    }
}
impl FinalAsrRuntime {
    #[cfg(test)]
    pub(super) fn diagnostic_instance_id(&self) -> Option<String> {
        self.control.lock().unwrap().instance.clone()
    }

    pub(super) fn status(&self) -> Value {
        self.status_for_stop(None)
    }
    fn status_for_stop(&self, stop_nonce: Option<u64>) -> Value {
        let c = self.control.lock().unwrap();
        let mut value = c.status.clone();
        value["mode"] = json!(if value["phase"] == "disabled" {
            "disabled"
        } else {
            "managed"
        });
        value["busy"] = json!(c.running.is_some());
        value["retry_available"] = json!(
            c.running.is_none()
                && ["stopped", "degraded", "configuration_error"].contains(&s(&value, "phase"))
        );
        if let Some(stop_nonce) = stop_nonce {
            value["stop_superseded"] = json!(c.epoch != stop_nonce);
        }
        value
    }
    pub(super) fn preempt(&self, audio: &AudioService) -> Option<u64> {
        self.preempt_with_nonce(audio).0
    }
    fn preempt_with_nonce(&self, audio: &AudioService) -> (Option<u64>, u64) {
        let mut c = self.control.lock().unwrap();
        c.epoch = c.epoch.wrapping_add(1);
        if let Some(cancel) = &c.cancel {
            let _ = cancel.send(true);
        }
        if let Some(instance) = c.instance.take() {
            audio.clear_final_asr(&instance);
        }
        c.status["active"] = json!(false);
        c.status["ready"] = json!(false);
        c.status["warmup_completed"] = json!(false);
        if c.running.is_some() {
            c.status["phase"] = json!("stopping");
        }
        (c.running, c.epoch)
    }
    fn stopped_status(&self, stop_nonce: u64) -> Result<Value> {
        let status = self.status_for_stop(Some(stop_nonce));
        if status["stop_superseded"] != true {
            ensure!(
                status["phase"] != "cleanup_failed" && status["busy"] != true,
                "精确识别清理尚未确认"
            );
        }
        Ok(status)
    }
    fn progress(&self, epoch: u64, event: Value) {
        let mut c = self.control.lock().unwrap();
        if c.epoch != epoch || c.running != Some(epoch) {
            return;
        }
        let phase = s(&event, "state");
        if ["waiting_gpu", "loading", "warming"].contains(&phase) {
            c.status["phase"] = json!(phase);
        }
        // Paths, endpoints, PIDs and credentials never enter public state.
    }
    fn install(&self, epoch: u64, audio: &AudioService, client: Arc<FinalAsrClient>) -> bool {
        let mut c = self.control.lock().unwrap();
        if c.epoch != epoch || c.running != Some(epoch) || client.unavailable() {
            return false;
        }
        audio.install_final_asr(client.clone());
        c.instance = Some(client.instance_id.clone());
        c.status = json!({"phase":"active","active":true,"ready":true,"warmup_completed":true});
        true
    }
    fn finish(&self, epoch: u64, audio: &AudioService, receipt: Option<Value>) {
        let mut c = self.control.lock().unwrap();
        if c.running != Some(epoch) {
            return;
        }
        if let Some(instance) = c.instance.take() {
            audio.clear_final_asr(&instance);
        }
        let confirmed = receipt
            .as_ref()
            .is_none_or(|r| r["linux_cleanup_confirmed"] == true);
        let phase = if !confirmed {
            "cleanup_failed"
        } else if c.epoch != epoch {
            "stopped"
        } else {
            "degraded"
        };
        c.status = json!({"phase":phase,"active":false,"ready":false,"warmup_completed":false,
            "cleanup_confirmed":confirmed,"ownership_stop":receipt,
            "error":if phase=="degraded" {Some("精确识别暂不可用，仍可使用现有本地识别")}
                else if !confirmed {Some("精确识别进程清理尚未确认")} else {None}});
        c.running = None;
        c.cancel = None;
    }
    async fn drain(&self, target: Option<u64>) {
        let Some(target) = target else {
            return;
        };
        let task = {
            let mut slot = self.task.lock().unwrap();
            if slot.as_ref().is_some_and(|(epoch, _)| *epoch == target) {
                slot.take()
            } else {
                None
            }
        };
        if let Some((epoch, mut task)) = task {
            if !matches!(
                tokio::time::timeout(Duration::from_secs(45), &mut task).await,
                Ok(Ok(()))
            ) {
                task.abort();
                let _ = task.await;
                let mut c = self.control.lock().unwrap();
                if c.running != Some(epoch) {
                    return;
                }
                c.running = None;
                c.cancel = None;
                c.status = json!({"phase":"cleanup_failed","active":false,"ready":false,
                    "warmup_completed":false,"cleanup_confirmed":false});
            }
        } else {
            let _ = tokio::time::timeout(Duration::from_secs(45), async {
                while self.control.lock().unwrap().running == Some(target) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
        }
    }
}

impl Workstation {
    pub(super) fn final_asr_status(&self) -> Value {
        let mut status = self.final_asr.status();
        status["shutting_down"] = json!(self.shutting_down.load(Ordering::SeqCst));
        if status["shutting_down"] == true {
            status["retry_available"] = json!(false);
        }
        status
    }
    pub fn start_final_asr_runtime(self: &Arc<Self>) {
        if self.queue_final_asr().is_err() {
            self.notify("asr.status");
        }
    }
    fn queue_final_asr(self: &Arc<Self>) -> Result<Value> {
        let mut gpu_queue = self.gpu_queue.lock().unwrap();
        let speech_state = self.local_speech.status();
        let asr_state = self.final_asr.status();
        gpu_queue.refresh_if_idle(&speech_state, &asr_state);
        let mut c = self.final_asr.control.lock().unwrap();
        ensure!(!self.shutting_down.load(Ordering::SeqCst), "工作台正在关闭");
        ensure!(
            c.running.is_none() && c.status["phase"] != "cleanup_failed",
            "精确识别仍在运行或清理未确认"
        );
        let config = match policy(&self.root).and_then(|config| {
            if let Some(config) = &config {
                config.validate()?;
            }
            Ok(config)
        }) {
            Ok(Some(config)) => config,
            Ok(None) => {
                c.status = json!({"phase":"disabled","active":false,"ready":false,"warmup_completed":false});
                return Ok(json!({"ok":true,"queued":false}));
            }
            Err(_) => {
                c.status = json!({"phase":"configuration_error","active":false,"ready":false,
                    "warmup_completed":false,"error":"请检查精确识别配置"});
                bail!("请检查精确识别配置");
            }
        };
        gpu_queue.admit(&config.distribution, &speech_state, &asr_state)?;
        c.epoch = c.epoch.wrapping_add(1);
        let epoch = c.epoch;
        let (cancel, cancelled) = watch::channel(false);
        c.running = Some(epoch);
        c.cancel = Some(cancel);
        c.status =
            json!({"phase":"starting","active":false,"ready":false,"warmup_completed":false});
        let (start, ready) = tokio::sync::oneshot::channel();
        let app = self.clone();
        *self.final_asr.task.lock().unwrap() = Some((
            epoch,
            tokio::spawn(async move {
                if ready.await.is_ok() {
                    app.run_final_asr(config, epoch, cancelled).await;
                }
            }),
        ));
        let _ = start.send(());
        drop(c);
        drop(gpu_queue);
        self.notify("asr.status");
        Ok(json!({"ok":true,"queued":true}))
    }
    pub(super) async fn retry_final_asr(self: &Arc<Self>) -> Result<Value> {
        self.queue_final_asr()
    }
    pub(super) async fn stop_final_asr(&self) -> Result<Value> {
        let (target, stop_nonce) = self.final_asr.preempt_with_nonce(&self.audio);
        self.notify("asr.status");
        self.final_asr.drain(target).await;
        self.notify("asr.status");
        // The status and superseded marker share one locked snapshot. A newer
        // Retry's busy flag is not evidence that this old Stop failed cleanup.
        let mut status = self.final_asr.stopped_status(stop_nonce)?;
        status["shutting_down"] = json!(self.shutting_down.load(Ordering::SeqCst));
        if status["shutting_down"] == true {
            status["retry_available"] = json!(false);
        }
        Ok(status)
    }
    pub(super) async fn close_final_asr(&self) {
        let target = self.final_asr.preempt(&self.audio);
        self.final_asr.drain(target).await;
    }
    async fn run_final_asr(
        self: Arc<Self>,
        config: FinalAsrConfig,
        epoch: u64,
        cancelled: watch::Receiver<bool>,
    ) {
        let (progress, mut events) = tokio::sync::mpsc::channel(32);
        let preparing =
            OwnedFinalAsrService::prepare(&self.root, &config, cancelled.clone(), progress);
        tokio::pin!(preparing);
        let prepared = loop {
            tokio::select! {
                result = &mut preparing => break result,
                Some(event) = events.recv() => {
                    self.final_asr.progress(epoch, event);
                    self.notify("asr.status");
                }
            }
        };
        let Some(mut owner) = prepared.owner else {
            self.final_asr.finish(epoch, &self.audio, None);
            self.notify("asr.status");
            return;
        };
        if let Ok(client) = prepared.ready {
            if self.final_asr.install(epoch, &self.audio, client.clone()) {
                self.notify("asr.status");
                tokio::select! {
                    _ = owner.monitor(cancelled) => {},
                    _ = async {
                        while !client.unavailable() { tokio::time::sleep(Duration::from_millis(100)).await; }
                    } => {},
                }
            }
        }
        {
            let mut c = self.final_asr.control.lock().unwrap();
            if c.running == Some(epoch) {
                if let Some(instance) = c.instance.take() {
                    self.audio.clear_final_asr(&instance);
                }
                c.status["active"] = json!(false);
                c.status["phase"] = json!("stopping");
            }
        }
        let receipt = owner
            .stop()
            .await
            .unwrap_or_else(|_| json!({"linux_cleanup_confirmed":false}));
        self.final_asr.finish(epoch, &self.audio, Some(receipt));
        self.notify("asr.status");
    }
}
