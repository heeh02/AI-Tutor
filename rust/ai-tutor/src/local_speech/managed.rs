//! The installed Linux wrapper owns its process group. Rust owns only this bridge.
use super::ManagedSpeechConfig;
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use std::{
    path::Path,
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::mpsc,
    task::JoinHandle,
};

const GPU_WAIT_BUDGET: Duration = Duration::from_secs(600);

pub(crate) fn engine_endpoint(engine: &str) -> Result<(&'static str, &'static str)> {
    match engine {
        "qwen" => Ok(("http://127.0.0.1:18868/v1/audio/speech", "tutor-qwen-tts")),
        "cosy" => Ok(("http://127.0.0.1:18869/v1/audio/speech", "tutor-cosy-tts")),
        _ => bail!("Unsupported managed voice"),
    }
}

fn event(bytes: &[u8], instance: &str, engine: &str) -> Result<Value> {
    ensure!(
        bytes.len() <= 4096,
        "Managed lifecycle message exceeds its bound"
    );
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| anyhow!("Invalid managed lifecycle JSON"))?;
    ensure!(
        value["protocol_version"] == 1 && value["instance_id"] == instance,
        "Managed lifecycle identity mismatch"
    );
    let state = value["state"].as_str().unwrap_or("");
    let mut result = json!({"protocol_version":1,"instance_id":instance,"state":state});
    match state {
        "waiting_gpu" => {
            ensure!(
                value["ready"] == false
                    && value["warmup_completed"] == false
                    && value.get("pid").is_none()
                    && value.get("process_group").is_none(),
                "Invalid managed GPU waiting receipt"
            );
            result["ready"] = json!(false);
            result["warmup_completed"] = json!(false);
        }
        "starting" => {
            let (endpoint, model) = engine_endpoint(engine)?;
            ensure!(
                value["engine"] == engine
                    && value["endpoint"] == endpoint
                    && value["model"] == model
                    && value["ready"] == false
                    && value["warm"] == false
                    && value["pid"].as_u64().is_some_and(|pid| pid > 0)
                    && value["process_group"] == value["pid"],
                "Invalid managed starting receipt"
            );
            for key in ["engine", "endpoint", "model", "pid", "process_group"] {
                result[key] = value[key].clone();
            }
            result["ready"] = json!(false);
            result["warm"] = json!(false);
        }
        "stopped" => {
            ensure!(
                [
                    "owner_stop",
                    "owner_eof",
                    "owner_signal",
                    "service_exited",
                    "control_error"
                ]
                .contains(&value["reason"].as_str().unwrap_or("")),
                "Invalid managed stop reason"
            );
            ensure!(
                value["forced_group_stop"].is_boolean() && value["service_exit_code"].is_i64(),
                "Incomplete managed stop receipt"
            );
            for key in ["reason", "forced_group_stop", "service_exit_code"] {
                result[key] = value[key].clone();
            }
        }
        "cleanup_failed" => {
            result["linux_cleanup_confirmed"] = json!(false);
        }
        "failed" => {
            ensure!(
                value["linux_cleanup_confirmed"] == true
                    && value["process_started"].is_boolean()
                    && value["ready"] == false
                    && value["warm"] == false,
                "Managed failure lacks explicit cleanup evidence"
            );
            result["error"] = json!("managed_start_failed");
            result["linux_cleanup_confirmed"] = json!(true);
            result["process_started"] = value["process_started"].clone();
        }
        _ => bail!("Unexpected managed lifecycle state"),
    }
    Ok(result)
}

fn command() -> Command {
    let mut command = Command::new("wsl.exe");
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    command.kill_on_drop(true);
    command
}

fn wsl_path_argument(root: &Path) -> Result<std::path::PathBuf> {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        ensure!(
            root.is_absolute(),
            "Managed voice needs an absolute Windows project root"
        );
        let mut components = root.components();
        let Some(Component::Prefix(prefix)) = components.next() else {
            bail!("Managed voice needs a local Windows drive");
        };
        match prefix.kind() {
            Prefix::Disk(_) => Ok(root.to_owned()),
            Prefix::VerbatimDisk(drive) => {
                // canonicalize uses the Win32 extended prefix, which wslpath
                // misinterprets as a literal '?' directory despite exit code 0.
                let mut path = std::path::PathBuf::from(format!(
                    "{}:\\",
                    char::from(drive.to_ascii_uppercase())
                ));
                for component in components {
                    if component != Component::RootDir {
                        path.push(component.as_os_str());
                    }
                }
                Ok(path)
            }
            Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _) => {
                bail!("Managed voice does not support UNC project roots; use a local Windows drive")
            }
            _ => bail!("Unsupported Windows device prefix for managed voice"),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        bail!("The managed WSL voice bridge requires Windows")
    }
}

fn cleanup_receipt(exit_code: Option<i32>, receipt: &Value) -> Result<Value> {
    // The wrapper emits stopped only after its owned group has no live members.
    // A service that failed to start still exits the wrapper with code 1; that
    // failure must not erase its successful cleanup evidence or forbid retry.
    ensure!(
        receipt["state"] == "stopped"
            || (receipt["state"] == "failed"
                && receipt["linux_cleanup_confirmed"] == true
                && receipt["process_started"].is_boolean()),
        "linux_cleanup_unconfirmed: wrapper cleanup failed"
    );
    let normal_owner_stop = exit_code == Some(0)
        && ["owner_stop", "owner_eof", "owner_signal"]
            .contains(&receipt["reason"].as_str().unwrap_or(""));
    Ok(
        json!({"linux_cleanup_confirmed":true,"bridge_exit_code":exit_code,
        "normal_owner_stop":normal_owner_stop,"receipt":receipt}),
    )
}

pub(crate) struct OwnedManagedTts {
    child: Child,
    input: Option<ChildStdin>,
    events: mpsc::Receiver<Result<Value, String>>,
    readers: Vec<JoinHandle<()>>,
    instance: String,
    lifecycle: ManagedLifecycle,
    prepared_sent: bool,
}

#[derive(Default)]
struct ManagedLifecycle {
    waiting: Option<Value>,
    starting: Option<Value>,
    terminal: Option<Value>,
}
impl ManagedLifecycle {
    fn observe(&mut self, value: Value) -> Result<()> {
        ensure!(
            self.terminal.is_none(),
            "Duplicate managed terminal receipt"
        );
        match value["state"].as_str().unwrap_or("") {
            "waiting_gpu" => {
                ensure!(
                    self.waiting.is_none() && self.starting.is_none(),
                    "Managed GPU waiting is only valid once before starting"
                );
                self.waiting = Some(value);
            }
            "starting" => {
                ensure!(
                    self.starting.is_none(),
                    "Duplicate managed starting receipt"
                );
                self.starting = Some(value);
            }
            "stopped" | "failed" | "cleanup_failed" => self.terminal = Some(value),
            _ => bail!("Unexpected managed lifecycle state"),
        }
        Ok(())
    }
    fn prepared_control(&self, instance: &str) -> Result<Vec<u8>> {
        ensure!(
            self.starting.is_some() && self.terminal.is_none(),
            "Managed service has not started or already stopped"
        );
        let mut line = serde_json::to_vec(&json!({"op":"prepared","instance_id":instance}))?;
        line.push(b'\n');
        Ok(line)
    }
}

impl OwnedManagedTts {
    pub(crate) async fn launch(
        root: &Path,
        config: &ManagedSpeechConfig,
        token: &str,
        instance: &str,
        cancellation: &AtomicU64,
        expected: u64,
    ) -> Result<Self> {
        for name in ["managed_tts.py", "gpu_admission.py"] {
            ensure!(
                root.join("bin/local-speech").join(name).is_file(),
                "Installed local speech bundle is missing; run the project installer"
            );
        }
        let mut conversion = command();
        conversion
            .args(["-d", &config.distribution, "--exec", "wslpath", "-u"])
            .arg(wsl_path_argument(root)?)
            .stdin(Stdio::null());
        let converted = tokio::select! { biased;
            _=crate::audio::streaming_tts::wait_cancelled(cancellation,expected)=>bail!("Managed startup cancelled"),
            result=tokio::time::timeout(Duration::from_secs(15), conversion.output())=>
                result.map_err(|_|anyhow!("WSL project path conversion timed out"))??,
        };
        ensure!(
            converted.status.success(),
            "WSL project path conversion failed"
        );
        let linux_root = std::str::from_utf8(&converted.stdout)?.trim();
        ensure!(
            linux_root.starts_with('/') && !linux_root.contains(['\n', '\r']),
            "Invalid WSL project path"
        );
        let wrapper = format!(
            "{}/bin/local-speech/managed_tts.py",
            linux_root.trim_end_matches('/')
        );
        ensure!(
            cancellation.load(Ordering::SeqCst) == expected,
            "Managed startup cancelled"
        );
        let mut child = command()
            .args([
                "-d",
                &config.distribution,
                "--exec",
                "python3",
                &wrapper,
                "--root",
                linux_root,
                "--engine",
                &config.engine,
                "--instance",
                instance,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let (send, events) = mpsc::channel(8);
        let id = instance.to_owned();
        let engine = config.engine.clone();
        let reader = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut bytes = Vec::new();
                let count = match (&mut reader).take(4097).read_until(b'\n', &mut bytes).await {
                    Ok(count) => count,
                    Err(_) => {
                        let _ = send.send(Err("managed_stdout_failed".into())).await;
                        return;
                    }
                };
                if count == 0 {
                    return;
                }
                let value =
                    event(&bytes, &id, &engine).map_err(|_| "managed_protocol_failed".to_owned());
                let failed = value.is_err();
                if send.send(value).await.is_err() || failed {
                    return;
                }
            }
        });
        // vLLM logs never enter user state or an unbounded buffer. The private
        // bootstrap token is not persisted, even if a dependency prints it.
        let errors = tokio::spawn(async move {
            let mut bytes = [0u8; 8192];
            while stderr.read(&mut bytes).await.is_ok_and(|count| count > 0) {}
        });
        let mut owned = Self {
            child,
            input,
            events,
            readers: vec![reader, errors],
            instance: instance.into(),
            lifecycle: ManagedLifecycle::default(),
            prepared_sent: false,
        };
        let bootstrap = format!("{}\n", json!({"protocol_version":1,"bearer_token":token}));
        let sent = tokio::time::timeout(Duration::from_secs(2), async {
            let input = owned
                .input
                .as_mut()
                .ok_or_else(|| anyhow!("Managed control pipe unavailable"))?;
            input.write_all(bootstrap.as_bytes()).await?;
            input.flush().await?;
            Ok::<_, anyhow::Error>(())
        })
        .await;
        if !matches!(sent, Ok(Ok(()))) {
            let _ = owned.stop().await;
            bail!("Managed bootstrap failed; owned bridge cleanup attempted");
        }
        Ok(owned)
    }

    fn receive_events(&mut self) -> Result<()> {
        while let Ok(result) = self.events.try_recv() {
            let value = result.map_err(anyhow::Error::msg)?;
            self.lifecycle.observe(value)?;
        }
        Ok(())
    }
    pub(crate) fn poll_running(&mut self) -> Result<bool> {
        self.receive_events()?;
        ensure!(
            self.lifecycle.terminal.is_none(),
            "Owned local speech service stopped"
        );
        ensure!(
            self.child.try_wait()?.is_none(),
            "Owned local speech bridge exited"
        );
        Ok(self.lifecycle.starting.is_some())
    }
    pub(crate) fn starting_receipt(&self) -> Option<&Value> {
        self.lifecycle.starting.as_ref()
    }

    /// Wait only for GPU admission/child creation. The caller starts its
    /// existing HTTP/model-readiness budget after this returns, then warms.
    pub(crate) async fn wait_for_starting(
        &mut self,
        cancellation: &AtomicU64,
        expected: u64,
        mut progress: impl FnMut(Value),
    ) -> Result<Value> {
        let deadline = tokio::time::Instant::now() + GPU_WAIT_BUDGET;
        let mut waiting_reported = false;
        loop {
            ensure!(
                cancellation.load(Ordering::SeqCst) == expected,
                "Managed GPU wait cancelled"
            );
            self.receive_events()?;
            ensure!(
                self.lifecycle.terminal.is_none(),
                "Owned local speech service stopped before admission"
            );
            ensure!(
                self.child.try_wait()?.is_none(),
                "Owned local speech bridge exited before admission"
            );
            if !waiting_reported && let Some(value) = &self.lifecycle.waiting {
                progress(value.clone());
                waiting_reported = true;
            }
            if let Some(value) = &self.lifecycle.starting {
                progress(value.clone());
                return Ok(value.clone());
            }
            let result = tokio::select! { biased;
                _ = crate::audio::streaming_tts::wait_cancelled(cancellation, expected) =>
                    bail!("Managed GPU wait cancelled"),
                result = tokio::time::timeout_at(deadline, self.events.recv()) =>
                    result.map_err(|_| anyhow!("Managed GPU admission exceeded 600 seconds"))?
                        .ok_or_else(|| anyhow!("Managed lifecycle channel closed before admission"))?,
            };
            self.lifecycle
                .observe(result.map_err(anyhow::Error::msg)?)?;
        }
    }

    /// Call only after HTTP readiness and actual bilingual warmup succeeded.
    /// Notify the owner to release initialization admission; installation is separate.
    pub(crate) async fn finish_preparation(&mut self) -> Result<()> {
        ensure!(self.poll_running()?, "Managed service is not running");
        if self.prepared_sent {
            return Ok(());
        }
        let line = self.lifecycle.prepared_control(&self.instance)?;
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| anyhow!("Managed control pipe unavailable"))?;
        tokio::time::timeout(Duration::from_secs(2), async {
            input.write_all(&line).await?;
            input.flush().await
        })
        .await
        .map_err(|_| anyhow!("Managed prepared notification timed out"))??;
        self.prepared_sent = true;
        Ok(())
    }

    pub(crate) async fn stop(&mut self) -> Result<Value> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        if let Some(mut input) = self.input.take() {
            let message = format!("{}\n", json!({"op":"stop","instance_id":self.instance}));
            let _ =
                tokio::time::timeout(Duration::from_secs(2), input.write_all(message.as_bytes()))
                    .await;
            // EOF is an additional request to this wrapper only.
        }
        loop {
            let protocol = self.receive_events();
            let exit = self.child.try_wait()?;
            if let Some(exit) = exit {
                // Pipe readers can deliver the final line after process exit.
                for reader in &mut self.readers {
                    let _ = tokio::time::timeout_at(deadline, reader).await;
                }
                self.receive_events()?;
                protocol?;
                let receipt =
                    self.lifecycle.terminal.as_ref().ok_or_else(|| {
                        anyhow!("linux_cleanup_unconfirmed: missing wrapper receipt")
                    })?;
                return cleanup_receipt(exit.code(), receipt);
            }
            if tokio::time::Instant::now() >= deadline {
                // This handle belongs to this instance; never kill a discovered
                // Linux PID, another WSL bridge, or the WSL distribution.
                let _ = self.child.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
                bail!("linux_cleanup_unconfirmed: owned bridge stop timed out");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
impl Drop for OwnedManagedTts {
    fn drop(&mut self) {
        self.input.take();
        for reader in &self.readers {
            reader.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting(instance: &str) -> Value {
        json!({"protocol_version":1,"instance_id":instance,"state":"waiting_gpu",
            "ready":false,"warmup_completed":false})
    }
    fn starting(instance: &str) -> Value {
        json!({"protocol_version":1,"instance_id":instance,"state":"starting","engine":"qwen",
            "endpoint":"http://127.0.0.1:18868/v1/audio/speech","model":"tutor-qwen-tts",
            "pid":23,"process_group":23,"ready":false,"warm":false})
    }

    #[test]
    fn managed_gpu_wait_is_not_starting_and_cannot_send_prepared_early() -> Result<()> {
        let mut state = ManagedLifecycle::default();
        let queued = event(&serde_json::to_vec(&waiting("abc"))?, "abc", "qwen")?;
        state.observe(queued.clone())?;
        assert!(state.starting.is_none());
        assert!(state.prepared_control("abc").is_err());
        assert!(state.observe(queued.clone()).is_err());
        state.observe(event(
            &serde_json::to_vec(&starting("abc"))?,
            "abc",
            "qwen",
        )?)?;
        assert!(state.observe(queued).is_err());
        let control: Value = serde_json::from_slice(&state.prepared_control("abc")?)?;
        assert_eq!(control, json!({"op":"prepared","instance_id":"abc"}));
        assert_eq!(control.as_object().unwrap().len(), 2);
        Ok(())
    }

    #[test]
    fn managed_gpu_wait_rejects_fake_child_identity_or_ready_flags() -> Result<()> {
        for field in ["ready", "warmup_completed"] {
            let mut value = waiting("abc");
            value[field] = json!(true);
            assert!(event(&serde_json::to_vec(&value)?, "abc", "qwen").is_err());
        }
        for field in ["pid", "process_group"] {
            let mut value = waiting("abc");
            value[field] = json!(23);
            assert!(event(&serde_json::to_vec(&value)?, "abc", "qwen").is_err());
        }
        assert!(event(&serde_json::to_vec(&waiting("old"))?, "abc", "qwen").is_err());
        let mut value = waiting("abc");
        value["secret"] = json!("never-forward");
        assert!(
            event(&serde_json::to_vec(&value)?, "abc", "qwen")?
                .get("secret")
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn managed_wait_cancellation_preserves_no_child_cleanup_proof() -> Result<()> {
        let mut state = ManagedLifecycle::default();
        state.observe(event(&serde_json::to_vec(&waiting("abc"))?, "abc", "qwen")?)?;
        state.observe(event(
            &serde_json::to_vec(&json!({"protocol_version":1,"instance_id":"abc",
            "state":"failed","linux_cleanup_confirmed":true,"process_started":false,
            "ready":false,"warm":false}))?,
            "abc",
            "qwen",
        )?)?;
        assert!(state.starting.is_none());
        assert!(state.prepared_control("abc").is_err());
        let terminal = state.terminal.as_ref().unwrap();
        assert!(terminal.get("pid").is_none());
        assert!(terminal.get("service_exit_code").is_none());
        let proof = cleanup_receipt(Some(1), terminal)?;
        assert_eq!(proof["linux_cleanup_confirmed"], true);
        assert_eq!(proof["receipt"]["process_started"], false);
        assert_eq!(proof["normal_owner_stop"], false);
        Ok(())
    }

    #[test]
    fn managed_receipts_bind_instance_engine_endpoint_and_do_not_forward_unknown_fields()
    -> Result<()> {
        let mut value = json!({"protocol_version":1,"instance_id":"abc","state":"starting","engine":"cosy",
            "endpoint":"http://127.0.0.1:18869/v1/audio/speech","model":"tutor-cosy-tts","pid":23,"process_group":23,
            "ready":false,"warm":false,"secret":"never-forward"});
        let receipt = event(&serde_json::to_vec(&value)?, "abc", "cosy")?;
        assert!(receipt.get("secret").is_none());
        assert!(event(&serde_json::to_vec(&value)?, "old", "cosy").is_err());
        assert!(event(&serde_json::to_vec(&value)?, "abc", "qwen").is_err());
        value["warm"] = json!(true);
        assert!(event(&serde_json::to_vec(&value)?, "abc", "cosy").is_err());
        Ok(())
    }
    #[test]
    fn cleanup_failure_is_not_a_stopped_receipt() -> Result<()> {
        let value = event(
            br#"{"protocol_version":1,"instance_id":"abc","state":"cleanup_failed"}"#,
            "abc",
            "qwen",
        )?;
        assert_eq!(value["linux_cleanup_confirmed"], false);
        assert_ne!(value["state"], "stopped");
        assert!(event(&vec![b' '; 4097], "abc", "qwen").is_err());
        assert!(cleanup_receipt(Some(1), &value).is_err());
        Ok(())
    }
    #[test]
    fn service_failure_retains_confirmed_group_cleanup_without_claiming_normal_stop() -> Result<()>
    {
        let receipt = event(br#"{"protocol_version":1,"instance_id":"abc","state":"stopped","reason":"service_exited","forced_group_stop":false,"service_exit_code":1}"#, "abc", "qwen")?;
        let cleanup = cleanup_receipt(Some(1), &receipt)?;
        assert_eq!(cleanup["linux_cleanup_confirmed"], true);
        assert_eq!(cleanup["normal_owner_stop"], false);
        assert_eq!(cleanup["bridge_exit_code"], 1);
        Ok(())
    }

    #[test]
    fn preflight_failure_can_prove_no_child_cleanup_without_claiming_a_successful_run() -> Result<()>
    {
        for process_started in [false, true] {
            let value = json!({"protocol_version":1,"instance_id":"abc","state":"failed",
                "linux_cleanup_confirmed":true,"process_started":process_started,"ready":false,"warm":false});
            let receipt = event(&serde_json::to_vec(&value)?, "abc", "qwen")?;
            let cleanup = cleanup_receipt(Some(1), &receipt)?;
            assert_eq!(cleanup["linux_cleanup_confirmed"], true);
            assert_eq!(cleanup["normal_owner_stop"], false);
            assert_eq!(cleanup["receipt"]["process_started"], process_started);
        }
        Ok(())
    }

    #[test]
    fn generic_failure_or_incomplete_proof_cannot_confirm_cleanup() -> Result<()> {
        let mut value = json!({"protocol_version":1,"instance_id":"abc","state":"failed","ready":false,"warm":false});
        assert!(event(&serde_json::to_vec(&value)?, "abc", "qwen").is_err());
        assert!(cleanup_receipt(Some(1), &value).is_err());
        value["linux_cleanup_confirmed"] = json!(true);
        assert!(event(&serde_json::to_vec(&value)?, "abc", "qwen").is_err());
        value["process_started"] = json!(false);
        value["linux_cleanup_confirmed"] = json!(false);
        assert!(event(&serde_json::to_vec(&value)?, "abc", "qwen").is_err());
        assert!(cleanup_receipt(Some(1), &value).is_err());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn wsl_path_conversion_preserves_drive_paths_and_spaces() -> Result<()> {
        let ordinary = Path::new(r"C:\Users\RBMS\Desktop\AI TUTOR");
        assert_eq!(wsl_path_argument(ordinary)?, ordinary);
        assert_eq!(
            wsl_path_argument(Path::new(r"\\?\C:\Users\RBMS\Desktop\AI TUTOR"))?,
            ordinary
        );
        assert_eq!(
            wsl_path_argument(Path::new(r"\\?\d:\教学 项目\AI TUTOR"))?,
            Path::new(r"D:\教学 项目\AI TUTOR")
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn wsl_path_conversion_rejects_unc_and_device_roots_explicitly() {
        for path in [r"\\server\share\AI TUTOR", r"\\?\UNC\server\share\AI TUTOR"] {
            assert!(
                wsl_path_argument(Path::new(path))
                    .unwrap_err()
                    .to_string()
                    .contains("UNC")
            );
        }
        assert!(wsl_path_argument(Path::new(r"\\.\C:\AI TUTOR")).is_err());
        assert!(wsl_path_argument(Path::new(r"C:AI TUTOR")).is_err());
    }
}
