use super::*;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use reqwest::header::{HeaderValue, AUTHORIZATION};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::sync::watch;
use tokio::task::JoinHandle;

const ENDPOINT: &str = "http://127.0.0.1:18870";
const MODEL_CHECKPOINT: &str = "Qwen/Qwen2.5-Omni-7B-GPTQ-Int4";
pub(crate) const MODEL: &str = OMNI_MODEL;

fn complete_official_manifest(manifest: &Value) -> bool {
    manifest["repository"] == MODEL_CHECKPOINT && manifest["metadata_only"] == false
}

pub(crate) struct OmniRuntime {
    client: reqwest::Client,
    token: String,
    state: watch::Sender<Value>,
    stop: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<()>>>,
}
impl OmniRuntime {
    pub(crate) fn prepare(root: &Path) -> Result<Arc<Self>> {
        for relative in [
            "runtime/omni-runtime/Scripts/python.exe",
            "scripts/omni-service/server.py",
            "models/qwen-omni/download-manifest.json",
        ] {
            ensure!(root.join(relative).is_file(), "omni_runtime_missing");
        }
        let manifest = fs::read(root.join("models/qwen-omni/download-manifest.json"))?;
        let manifest: Value = serde_json::from_slice(&manifest)?;
        ensure!(complete_official_manifest(&manifest), "omni_model_manifest");
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(300))
            .build()?;
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let (state, _) = watch::channel(json!({"phase":"starting","ready":false}));
        let (stop, _) = watch::channel(false);
        Ok(Arc::new(Self {
            client,
            token,
            state,
            stop,
            task: Mutex::new(None),
        }))
    }
    pub(crate) fn start(self: &Arc<Self>, root: &Path) -> Result<()> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow!("omni_async_runtime_required"))?;
        let mut command =
            tokio::process::Command::new(root.join("runtime/omni-runtime/Scripts/python.exe"));
        command
            .arg(root.join("scripts/omni-service/server.py"))
            .arg("--port")
            .arg("18870")
            .current_dir(root)
            .env("AI_TUTOR_OMNI_TOKEN", &self.token)
            .env("AI_TUTOR_OMNI_ATTENTION", "sdpa")
            .env("HF_HUB_OFFLINE", "1")
            .env("TRANSFORMERS_OFFLINE", "1")
            .env("PYTHONUTF8", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(CREATION_NO_WINDOW);
        let child = command
            .spawn()
            .map_err(|_| anyhow!("omni_process_spawn_failed"))?;
        *self.task.lock().unwrap() = Some(handle.spawn(self.clone().run(
            child,
            self.stop.subscribe(),
            root.join("runtime/omni-service.log"),
        )));
        Ok(())
    }
    async fn run(
        self: Arc<Self>,
        mut child: tokio::process::Child,
        mut stop: watch::Receiver<bool>,
        log_path: PathBuf,
    ) {
        let log_task = child
            .stderr
            .take()
            .map(|stderr| tokio::spawn(drain_log(stderr, log_path)));
        let deadline = Instant::now() + Duration::from_secs(600);
        let result = loop {
            if *stop.borrow() {
                break Ok(());
            }
            match child.try_wait() {
                Ok(Some(status)) => break Err(anyhow!("omni_service_exited:{status}")),
                Ok(None) => {}
                Err(_) => break Err(anyhow!("omni_process_observe_failed")),
            }
            if self.health().await {
                self.state
                    .send_replace(json!({"phase":"ready","ready":true}));
                let stopped = tokio::select! {
                    _ = stop.changed() => true,
                    status = child.wait() => {
                        let detail = status.map(|status| status.to_string())
                            .unwrap_or_else(|_| "observe_failed".into());
                        self.state.send_replace(json!({"phase":"error","ready":false,
                            "error":format!("omni_service_exited:{detail}")}));
                        false
                    }
                };
                if stopped {
                    self.state.send_replace(json!({"phase":"stopped","ready":false}));
                }
                let _ = child.kill().await;
                let _ = child.wait().await;
                if let Some(task) = log_task {
                    let _ = task.await;
                }
                return;
            }
            if Instant::now() >= deadline {
                break Err(anyhow!("omni_readiness_deadline"));
            }
            if tokio::time::timeout(Duration::from_secs(1), stop.changed())
                .await
                .is_ok()
            {
                break Ok(());
            }
        };
        if let Err(error) = result {
            self.state
                .send_replace(json!({"phase":"error","ready":false,"error":error.to_string()}));
            let _ = child.kill().await;
        } else {
            self.state
                .send_replace(json!({"phase":"stopped","ready":false}));
            let _ = child.kill().await;
        }
        let _ = child.wait().await;
        if let Some(task) = log_task {
            let _ = task.await;
        }
    }
    async fn health(&self) -> bool {
        let Ok(response) = self.client.get(format!("{ENDPOINT}/health")).send().await else {
            return false;
        };
        let Ok(value) = response.json::<Value>().await else {
            return false;
        };
        value["ready"] == true && value["model"] == MODEL && value["local"] == true
    }
    pub(crate) fn status(&self) -> Value {
        let mut status = self.state.borrow().clone();
        status["model"] = json!(MODEL);
        status["local"] = json!(true);
        status
    }
    pub(crate) fn ready(&self) -> bool {
        self.state.borrow()["ready"] == true
    }
    pub(crate) async fn stop(&self) {
        let _ = self.stop.send(true);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            if tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .is_err()
            {
                self.state
                    .send_replace(json!({"phase":"stop_timeout","ready":false}));
            }
        }
    }
    pub(crate) async fn infer(&self, mut payload: Value) -> Result<Value> {
        ensure!(self.ready(), "omni_not_ready");
        ensure!(
            payload["messages"]
                .as_array()
                .is_some_and(|messages| !messages.is_empty() && messages.len() <= 16),
            "omni_messages_invalid"
        );
        payload["model"] = json!(MODEL);
        payload["stream"] = json!(false);
        let body = serde_json::to_vec(&payload)?;
        ensure!(body.len() <= 16 * 1024 * 1024, "omni_request_too_large");
        let mut authorization = HeaderValue::from_str(&format!("Bearer {}", self.token))?;
        authorization.set_sensitive(true);
        let response = self
            .client
            .post(format!("{ENDPOINT}/v1/chat/completions"))
            .header(AUTHORIZATION, authorization)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| anyhow!("omni_transport_failed"))?;
        ensure!(response.status().is_success(), "omni_request_rejected");
        ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= 32 * 1024 * 1024),
            "omni_response_too_large"
        );
        let bytes = response
            .bytes()
            .await
            .map_err(|_| anyhow!("omni_response_failed"))?;
        ensure!(bytes.len() <= 32 * 1024 * 1024, "omni_response_too_large");
        let value: Value = serde_json::from_slice(&bytes)?;
        ensure!(
            value["model"] == MODEL && value["x_ai_tutor"]["local_inference"] == true,
            "omni_response_model_mismatch"
        );
        let choices = value["choices"]
            .as_array()
            .ok_or_else(|| anyhow!("omni_choices_invalid"))?;
        ensure!(
            choices.len() == 1 && choices[0]["finish_reason"] == "stop",
            "omni_incomplete_response"
        );
        ensure!(
            choices[0]["message"]["content"]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty()),
            "omni_empty_response"
        );
        if let Some(audio) = choices[0]["message"]["audio"].get("data") {
            let data = audio
                .as_str()
                .ok_or_else(|| anyhow!("omni_audio_invalid"))?;
            ensure!(data.len() <= 24 * 1024 * 1024, "omni_audio_too_large");
            let bytes = STANDARD
                .decode(data)
                .map_err(|_| anyhow!("omni_audio_invalid"))?;
            ensure!(
                bytes.len() >= 44 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
                "omni_audio_invalid"
            );
            ensure!(
                choices[0]["message"]["audio"]["format"] == "wav"
                    && choices[0]["message"]["audio"]["sample_rate"] == 24000,
                "omni_audio_format"
            );
            ensure!(
                choices[0]["message"]["audio"]["transcript"] == choices[0]["message"]["content"],
                "omni_audio_text_mismatch"
            );
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_requires_the_complete_official_qwen_repository() {
        assert!(complete_official_manifest(&json!({
            "repository":MODEL_CHECKPOINT,"metadata_only":false
        })));
        for manifest in [
            json!({"repository":"Qwen2.5-Omni-7B-GPTQ-Int4","metadata_only":false}),
            json!({"repository":"unofficial/fork","metadata_only":false}),
            json!({"repository":MODEL_CHECKPOINT,"metadata_only":true}),
        ] {
            assert!(!complete_official_manifest(&manifest));
        }
    }
}

async fn drain_log(mut reader: tokio::process::ChildStderr, path: PathBuf) {
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(size) => {
                output.extend_from_slice(&buffer[..size]);
                if output.len() > 64 * 1024 {
                    let excess = output.len() - 64 * 1024;
                    output.drain(..excess);
                }
            }
        }
    }
    let _ = tokio::fs::write(path, output).await;
}
