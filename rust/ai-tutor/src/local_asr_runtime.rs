//! One optional CPU ASR lifecycle. Lock order: lifecycle control -> ASR client
//! slot; capture only clones that slot and never takes lifecycle/teaching locks.
use super::*;
use crate::local_asr::{AsrClient, AsrMode, LocalAsrPolicy, OwnedAsrService};
#[cfg(test)]
#[path = "local_asr_runtime_tests.rs"]
mod tests;

struct AsrControl {
    epoch: u64,
    running_epoch: Option<u64>,
    stopping: bool,
    cancellation: Option<watch::Sender<bool>>,
    active_instance: Option<String>,
    status: Value,
}
pub(super) struct LocalAsrRuntime {
    control: Mutex<AsrControl>,
    task: Mutex<Option<(u64, JoinHandle<()>)>>,
}
impl Default for LocalAsrRuntime {
    fn default() -> Self {
        Self {
            control: Mutex::new(AsrControl {
                epoch: 0,
                running_epoch: None,
                stopping: false,
                cancellation: None,
                active_instance: None,
                status: json!({"mode":"disabled","phase":"disabled","ready":false,
                "warmup_completed":false,"active":false}),
            }),
            task: Mutex::new(None),
        }
    }
}
impl LocalAsrRuntime {
    fn status(&self) -> Value {
        let control = self.control.lock().unwrap();
        let mut value = control.status.clone();
        value["busy"] = json!(control.running_epoch.is_some() || control.stopping);
        value["retry_available"] = json!(
            control.running_epoch.is_none()
                && !control.stopping
                && ["stopped", "degraded", "configuration_error"].contains(&s(&value, "phase"))
        );
        value
    }
    pub(super) fn preempt(&self, audio: &AudioService) -> u64 {
        let mut control = self.control.lock().unwrap();
        control.stopping = true;
        control.epoch = control.epoch.wrapping_add(1);
        if let Some(cancel) = &control.cancellation {
            let _ = cancel.send(true);
        }
        if let Some(instance) = control.active_instance.take() {
            audio.clear_streaming_asr(&instance);
        }
        control.status["active"] = json!(false);
        if control.running_epoch.is_some() {
            control.status["phase"] = json!("stopping");
        }
        control.epoch
    }
    fn progress(&self, epoch: u64, event: Value) {
        let mut control = self.control.lock().unwrap();
        if control.epoch != epoch || control.running_epoch != Some(epoch) {
            return;
        }
        control.status["phase"] = json!(if event["state"] == "loading" {
            "loading"
        } else {
            "warming"
        });
        control.status["ready"] = event["ready"].clone();
        control.status["warmup_completed"] = event["warmup_completed"].clone();
        // Endpoint, token, paths and interpreter PID are never public UI state.
    }
    fn install(&self, epoch: u64, audio: &AudioService, client: Arc<AsrClient>) -> bool {
        let mut control = self.control.lock().unwrap();
        if control.epoch != epoch
            || control.running_epoch != Some(epoch)
            || control.stopping
            || client.unavailable()
        {
            return false;
        }
        // The cancellation check and client replacement are the single commit
        // point. No async owner queue or second preparation token is required.
        audio.install_streaming_asr(client.clone());
        control.active_instance = Some(client.instance_id.clone());
        control.status["phase"] = json!("active");
        control.status["active"] = json!(true);
        control.status["ready"] = json!(true);
        control.status["warmup_completed"] = json!(true);
        true
    }
    fn finish(&self, epoch: u64, audio: &AudioService, receipt: Option<Value>) {
        let mut control = self.control.lock().unwrap();
        if control.running_epoch != Some(epoch) {
            return;
        }
        if let Some(instance) = control.active_instance.take() {
            audio.clear_streaming_asr(&instance);
        }
        let confirmed = receipt
            .as_ref()
            .is_none_or(|r| r["cleanup_confirmed"] == true);
        let cancelled = control.epoch != epoch;
        control.status = json!({"mode":"managed","phase":if !confirmed {"cleanup_failed"}
            else if cancelled {"stopped"} else {"degraded"}, "ready":false,"warmup_completed":false,
            "active":false,"cleanup_confirmed":confirmed,"ownership_stop":receipt,
            "error":if !confirmed {Some("owned_asr_cleanup_unconfirmed")}
                else if !cancelled {Some("local_asr_service_unavailable")} else {None}});
        control.running_epoch = None;
        control.cancellation = None;
        // The HTTP waiter may have been dropped. Cleanup completion, rather
        // than the caller, owns returning this lifecycle to an idle state.
        control.stopping = false;
    }
    async fn drain(&self, audio: &AudioService) {
        let task = self.task.lock().unwrap().take();
        if let Some((task_epoch, mut task)) = task {
            let completed = match tokio::time::timeout(Duration::from_secs(10), &mut task).await {
                Ok(Ok(())) => true,
                Ok(Err(_)) => false,
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    false
                }
            };
            if !completed {
                let mut control = self.control.lock().unwrap();
                if control.running_epoch != Some(task_epoch) {
                    return;
                }
                if let Some(instance) = control.active_instance.take() {
                    audio.clear_streaming_asr(&instance);
                }
                control.running_epoch = None;
                control.cancellation = None;
                control.stopping = false;
                control.status = json!({"mode":"managed","phase":"cleanup_failed","ready":false,
                    "warmup_completed":false,"active":false,"cleanup_confirmed":false,
                    "error":"owned_asr_cleanup_unconfirmed"});
            }
        } else {
            // Another stop caller can own the JoinHandle; do not claim stopped
            // while its cleanup task still runs.
            let _ = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if self.control.lock().unwrap().running_epoch.is_none() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
        }
    }
}
impl Workstation {
    pub(super) fn local_asr_status(&self) -> Value {
        let mut status = self.local_asr.status();
        status["shutting_down"] = json!(self.shutting_down.load(Ordering::SeqCst));
        if status["shutting_down"] == true {
            status["retry_available"] = json!(false);
        }
        status
    }
    pub fn start_local_asr_runtime(self: &Arc<Self>) {
        if self.queue_local_asr().is_err() {
            self.notify("asr.status");
        }
    }
    fn queue_local_asr(self: &Arc<Self>) -> Result<Value> {
        let mut control = self.local_asr.control.lock().unwrap();
        ensure!(!self.shutting_down.load(Ordering::SeqCst), "工作台正在关闭");
        ensure!(
            control.running_epoch.is_none() && !control.stopping,
            "本地识别仍在运行或收尾"
        );
        ensure!(
            control.status["phase"] != "cleanup_failed",
            "识别进程退出尚未确认"
        );
        let policy = match LocalAsrPolicy::load(&self.root) {
            Ok(policy) => policy,
            Err(_) => {
                control.status = json!({"mode":"disabled","phase":"configuration_error",
                    "ready":false,"warmup_completed":false,"active":false,"error":"local_asr_policy_invalid"});
                bail!("请检查本地识别配置；可继续使用文字输入");
            }
        };
        if policy.mode == AsrMode::Disabled {
            control.status = json!({"mode":"disabled","phase":"disabled","ready":false,
                "warmup_completed":false,"active":false});
            return Ok(json!({"ok":true,"queued":false,"mode":"disabled"}));
        }
        control.epoch = control.epoch.wrapping_add(1);
        let epoch = control.epoch;
        control.running_epoch = Some(epoch);
        control.status = json!({"mode":"managed","phase":"starting","ready":false,
            "warmup_completed":false,"active":false});
        let (cancel, cancelled) = watch::channel(false);
        control.cancellation = Some(cancel);
        let (start, ready) = tokio::sync::oneshot::channel();
        let app = self.clone();
        let task = tokio::spawn(async move {
            if ready.await.is_ok() {
                app.run_local_asr(policy, epoch, cancelled).await;
            }
        });
        *self.local_asr.task.lock().unwrap() = Some((epoch, task));
        let _ = start.send(());
        drop(control);
        self.notify("asr.status");
        Ok(json!({"ok":true,"queued":true,"mode":"managed"}))
    }
    pub(super) async fn retry_local_asr(self: &Arc<Self>) -> Result<Value> {
        self.queue_local_asr()
    }
    pub(super) async fn stop_local_asr(&self) -> Result<Value> {
        let stopped_epoch = self.local_asr.preempt(&self.audio);
        self.local_asr.drain(&self.audio).await;
        {
            let mut control = self.local_asr.control.lock().unwrap();
            if control.epoch == stopped_epoch && control.running_epoch.is_none() {
                control.stopping = false;
                if control.status["phase"] != "cleanup_failed"
                    && control.status["phase"] != "disabled"
                {
                    control.status["phase"] = json!("stopped");
                    control.status["ready"] = json!(false);
                    control.status["warmup_completed"] = json!(false);
                }
            }
        }
        self.notify("asr.status");
        let status = self.local_asr_status();
        ensure!(
            status["phase"] != "cleanup_failed" && status["busy"] != true,
            "本地识别清理尚未确认"
        );
        Ok(status)
    }
    pub(super) async fn close_local_asr(&self) {
        self.local_asr.preempt(&self.audio);
        self.local_asr.drain(&self.audio).await;
    }
    async fn run_local_asr(
        self: Arc<Self>,
        policy: LocalAsrPolicy,
        epoch: u64,
        mut cancelled: watch::Receiver<bool>,
    ) {
        let mut owned = match OwnedAsrService::spawn(&self.root, &policy) {
            Ok(owned) => owned,
            Err(_) => {
                self.local_asr.finish(epoch, &self.audio, None);
                self.notify("asr.status");
                return;
            }
        };
        let prepared = owned
            .prepare(&mut cancelled, |event| {
                self.local_asr.progress(epoch, event);
                self.notify("asr.status");
            })
            .await;
        if let Ok(client) = prepared {
            if self.local_asr.install(epoch, &self.audio, client.clone()) {
                self.notify("asr.status");
                loop {
                    if *cancelled.borrow()
                        || client.unavailable()
                        || !owned.is_running().unwrap_or(false)
                    {
                        break;
                    }
                    // No health probe competes with the single inference worker.
                    tokio::select! { _=cancelled.changed()=>break,
                    _=tokio::time::sleep(Duration::from_millis(100))=>{} }
                }
            }
        }
        // This only detaches future captures. Any active capture retains its
        // immutable connection and full PCM for the existing Whisper fallback.
        {
            let mut control = self.local_asr.control.lock().unwrap();
            if control.running_epoch == Some(epoch) {
                if let Some(instance) = control.active_instance.take() {
                    self.audio.clear_streaming_asr(&instance);
                }
                control.status["active"] = json!(false);
                control.status["phase"] = json!("stopping");
            }
        }
        let receipt = owned.stop().await;
        self.local_asr.finish(epoch, &self.audio, Some(receipt));
        self.notify("asr.status");
    }
}
