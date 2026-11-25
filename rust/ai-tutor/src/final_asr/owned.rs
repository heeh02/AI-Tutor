//! The Linux owner retires its own process group; Rust owns only the WSL bridge.
use super::FinalAsrClient;
use anyhow::{Result, anyhow, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{mpsc, watch},
    task::JoinHandle,
    time::Instant,
};

const GPU_WAIT_BUDGET: Duration = Duration::from_secs(600);
const MODEL_PREPARATION_BUDGET: Duration = Duration::from_secs(180);

#[cfg(test)]
#[path = "owned_tests.rs"]
mod tests;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FinalAsrConfig {
    pub distribution: String,
    pub python_executable: String,
    pub cache_directory: String,
}

impl FinalAsrConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.distribution.is_empty()
                && self.distribution.len() <= 128
                && self.distribution.trim() == self.distribution
                && !self.distribution.starts_with('-')
                && !self.distribution.chars().any(char::is_control),
            "Invalid final ASR WSL distribution"
        );
        for path in [&self.python_executable, &self.cache_directory] {
            ensure!(
                path.starts_with('/')
                    && path.len() > 1
                    && path.len() <= 4096
                    && !path.chars().any(char::is_control),
                "Final ASR requires absolute Linux paths"
            );
        }
        Ok(())
    }
}

pub(crate) struct Preparation {
    /// Retain a created owner even when ready is Err, then await stop().
    pub owner: Option<OwnedFinalAsrService>,
    pub ready: Result<Arc<FinalAsrClient>>,
}

async fn wait_cancelled(cancel: &mut watch::Receiver<bool>) {
    loop {
        if *cancel.borrow() {
            return;
        }
        if cancel.changed().await.is_err() {
            return;
        }
    }
}

fn command() -> Command {
    let mut command = Command::new("wsl.exe");
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    command.kill_on_drop(true);
    command
}

fn wsl_path_argument(root: &Path) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        ensure!(
            root.is_absolute(),
            "Final ASR needs an absolute Windows project root"
        );
        let mut components = root.components();
        let Some(Component::Prefix(prefix)) = components.next() else {
            bail!("Final ASR needs a local Windows drive");
        };
        match prefix.kind() {
            Prefix::Disk(_) => Ok(root.to_owned()),
            Prefix::VerbatimDisk(drive) => {
                let mut path =
                    PathBuf::from(format!("{}:\\", char::from(drive.to_ascii_uppercase())));
                for component in components {
                    if component != Component::RootDir {
                        path.push(component.as_os_str());
                    }
                }
                Ok(path)
            }
            Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _) => {
                bail!("Final ASR does not support UNC project roots")
            }
            _ => bail!("Unsupported Windows project root"),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        bail!("The final ASR WSL bridge requires Windows")
    }
}

async fn linux_root(
    root: &Path,
    config: &FinalAsrConfig,
    cancel: &mut watch::Receiver<bool>,
    deadline: Instant,
) -> Result<String> {
    ensure!(!*cancel.borrow(), "Final ASR preparation cancelled");
    let mut child = command()
        .args(["-d", &config.distribution, "--exec", "wslpath", "-u"])
        .arg(wsl_path_argument(root)?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let output = child
        .stdout
        .take()
        .expect("configured WSL conversion stdout");
    let result = tokio::select! { biased;
        _ = wait_cancelled(cancel) => Err(anyhow!("Final ASR preparation cancelled")),
        result = tokio::time::timeout_at(deadline.min(Instant::now() + Duration::from_secs(15)), async {
            let mut bytes = Vec::new();
            output.take(4097).read_to_end(&mut bytes).await?;
            ensure!(bytes.len() <= 4096, "WSL root conversion exceeded its bound");
            ensure!(child.wait().await?.success(), "WSL root conversion failed");
            let path = std::str::from_utf8(&bytes)?.trim().to_owned();
            ensure!(path.starts_with('/') && !path.chars().any(char::is_control), "Invalid WSL project root");
            Ok(path)
        }) => result.map_err(|_| anyhow!("WSL root conversion timed out")).and_then(|value| value),
    };
    if result.is_err() {
        // No model owner exists yet. Retire only this conversion subprocess.
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    }
    result
}

fn local_endpoint(value: &Value) -> Result<String> {
    let text = value
        .as_str()
        .ok_or_else(|| anyhow!("Final ASR endpoint missing"))?;
    let url = reqwest::Url::parse(text).map_err(|_| anyhow!("Invalid final ASR endpoint"))?;
    ensure!(
        text.len() <= 128
            && url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.port().is_some_and(|port| port > 0)
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        "Final ASR endpoint must be an explicit loopback port"
    );
    Ok(text.to_owned())
}

fn lifecycle_event(bytes: &[u8], instance: &str) -> Result<Value> {
    ensure!(
        bytes.len() <= 4096 && bytes.ends_with(b"\n"),
        "Final ASR lifecycle line exceeds its bound or is incomplete"
    );
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| anyhow!("Invalid final ASR lifecycle JSON"))?;
    ensure!(
        value["protocol_version"] == 1 && value["instance_id"] == instance,
        "Final ASR lifecycle identity mismatch"
    );
    let state = value["state"]
        .as_str()
        .ok_or_else(|| anyhow!("Final ASR lifecycle state missing"))?;
    let mut clean = json!({"protocol_version":1,"instance_id":instance,"state":state});
    match state {
        "waiting_gpu" => {
            ensure!(
                value["ready"] == false
                    && value["warmup_completed"] == false
                    && value.get("pid").is_none()
                    && value.get("process_group").is_none(),
                "Invalid final ASR GPU waiting receipt"
            );
            clean["ready"] = json!(false);
            clean["warmup_completed"] = json!(false);
        }
        "starting" => {
            ensure!(
                value["pid"]
                    .as_u64()
                    .is_some_and(|pid| pid > 1 && pid <= u32::MAX.into())
                    && value["process_group"] == value["pid"]
                    && value["ready"] == false
                    && value["warmup_completed"] == false,
                "Invalid final ASR owned starting receipt"
            );
            for key in ["pid", "process_group", "ready", "warmup_completed"] {
                clean[key] = value[key].clone();
            }
        }
        "awaiting_owner" | "loading" | "warming" | "warm_ready" => {
            let warm = state == "warm_ready";
            ensure!(
                value["ready"] == warm && value["warmup_completed"] == warm,
                "Final ASR preparation flags mismatch"
            );
            clean["ready"] = json!(warm);
            clean["warmup_completed"] = json!(warm);
            if state != "awaiting_owner" {
                ensure!(
                    value["model_ready"] == (state != "loading")
                        && value["busy"] == false
                        && value["closing"] == false
                        && value["inference_mode"] == "offline_final",
                    "Invalid final ASR preparation state"
                );
                clean["base_url"] = json!(local_endpoint(&value["base_url"])?);
                for key in ["model_ready", "busy", "closing", "inference_mode"] {
                    clean[key] = value[key].clone();
                }
            }
        }
        "failed" | "cleanup_pending" | "stopped" => {
            ensure!(
                value["ready"] == false && value["warmup_completed"] == false,
                "Invalid child ASR terminal flags"
            );
            clean["ready"] = json!(false);
            clean["warmup_completed"] = json!(false);
            if state == "stopped" {
                ensure!(
                    value["http_closed"] == true
                        && value["worker_retired"] == true
                        && value["process_exit_still_required"] == true,
                    "Incomplete child ASR stop receipt"
                );
                for key in [
                    "http_closed",
                    "worker_retired",
                    "process_exit_still_required",
                ] {
                    clean[key] = value[key].clone();
                }
            } else if state == "cleanup_pending" {
                ensure!(
                    value["worker_retired"] == false,
                    "Invalid child ASR pending cleanup"
                );
                clean["worker_retired"] = json!(false);
            }
            // Child events deliberately cannot carry Linux cleanup evidence.
        }
        "owned_stopped" => {
            ensure!(
                value["linux_cleanup_confirmed"] == true
                    && value["forced_group_stop"].is_boolean()
                    && value["service_exit_code"].is_i64()
                    && value["process_started"].is_boolean()
                    && [
                        "owner_stop",
                        "owner_eof",
                        "owner_signal",
                        "service_exited",
                        "startup_failed",
                        "control_error",
                        "protocol_error",
                        "owner_error"
                    ]
                    .contains(&value["reason"].as_str().unwrap_or("")),
                "Incomplete final ASR Linux cleanup receipt"
            );
            for key in [
                "linux_cleanup_confirmed",
                "forced_group_stop",
                "service_exit_code",
                "process_started",
                "reason",
            ] {
                clean[key] = value[key].clone();
            }
        }
        "cleanup_failed" => {
            ensure!(
                value["linux_cleanup_confirmed"] == false,
                "Invalid final ASR cleanup failure"
            );
            clean["linux_cleanup_confirmed"] = json!(false);
        }
        _ => bail!("Unknown final ASR lifecycle state"),
    }
    Ok(clean)
}

#[derive(Default)]
struct Lifecycle {
    phase: u8,
    waiting_gpu: bool,
    endpoint: Option<String>,
    terminal: Option<Value>,
    child_stopped: bool,
    failed: bool,
    protocol_failed: bool,
}
impl Lifecycle {
    fn observe(&mut self, value: &Value) -> Result<()> {
        ensure!(
            self.terminal.is_none(),
            "Duplicate final ASR owner terminal receipt"
        );
        let phase = match value["state"].as_str().unwrap_or("") {
            "waiting_gpu" => {
                ensure!(
                    self.phase == 0 && !self.waiting_gpu && !self.failed && !self.child_stopped,
                    "Final ASR GPU waiting is only valid once before starting"
                );
                self.waiting_gpu = true;
                return Ok(());
            }
            "starting" => 1,
            "awaiting_owner" => 2,
            "loading" => 3,
            "warming" => 4,
            "warm_ready" => 5,
            "failed" | "cleanup_pending" => {
                self.failed = true;
                return Ok(());
            }
            "stopped" => {
                self.child_stopped = true;
                return Ok(());
            }
            "owned_stopped" | "cleanup_failed" => {
                self.terminal = Some(value.clone());
                return Ok(());
            }
            _ => bail!("Unexpected final ASR lifecycle event"),
        };
        ensure!(
            !self.failed && !self.child_stopped && phase == self.phase + 1,
            "Final ASR lifecycle order mismatch"
        );
        if phase >= 3 {
            let endpoint = value["base_url"]
                .as_str()
                .ok_or_else(|| anyhow!("Final ASR endpoint missing"))?;
            ensure!(
                self.endpoint
                    .as_deref()
                    .is_none_or(|previous| previous == endpoint),
                "Final ASR endpoint changed during preparation"
            );
            self.endpoint = Some(endpoint.into());
        }
        self.phase = phase;
        Ok(())
    }
    fn running(&self) -> bool {
        self.phase == 5
            && self.terminal.is_none()
            && !self.failed
            && !self.child_stopped
            && !self.protocol_failed
    }
}

async fn read_line(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    let count = reader.take(4097).read_until(b'\n', &mut bytes).await?;
    if count == 0 {
        return Ok(None);
    }
    ensure!(
        count <= 4096 && bytes.ends_with(b"\n"),
        "Final ASR lifecycle line too long or incomplete"
    );
    Ok(Some(bytes))
}

fn cleanup_receipt(
    instance: &str,
    bridge_exit_code: Option<i32>,
    terminal: &Value,
    protocol_failed: bool,
) -> Result<Value> {
    ensure!(
        terminal["state"] == "owned_stopped" && terminal["linux_cleanup_confirmed"] == true,
        "linux_cleanup_unconfirmed: no final ASR owned receipt"
    );
    let normal = bridge_exit_code == Some(0)
        && terminal["forced_group_stop"] == false
        && ["owner_stop", "owner_eof", "owner_signal"]
            .contains(&terminal["reason"].as_str().unwrap_or(""));
    Ok(
        json!({"instance_id":instance,"cleanup_confirmed":true,"linux_cleanup_confirmed":true,
        "bridge_exit_code":bridge_exit_code,"normal_owner_stop":normal,"protocol_failed":protocol_failed,
        "receipt":terminal}),
    )
}

pub(crate) struct OwnedFinalAsrService {
    child: Child,
    input: Option<ChildStdin>,
    events: mpsc::Receiver<Result<Value, String>>,
    readers: Vec<JoinHandle<()>>,
    lifecycle: Lifecycle,
    token: String,
    cleanup: Option<Value>,
    pub instance_id: String,
}

impl OwnedFinalAsrService {
    pub async fn prepare(
        root: &Path,
        config: &FinalAsrConfig,
        mut cancellation: watch::Receiver<bool>,
        progress: mpsc::Sender<Value>,
    ) -> Preparation {
        let deadline = Instant::now() + MODEL_PREPARATION_BUDGET;
        let preflight = async {
            config.validate()?;
            for name in [
                "managed_final_asr.py",
                "gpu_admission.py",
                "qwen_final_service.py",
                "qwen_final_engine.py",
                "qwen-model.json",
                "qwen-sdk.json",
            ] {
                ensure!(
                    root.join("bin/final-asr").join(name).is_file(),
                    "Installed final ASR bundle is missing"
                );
            }
            linux_root(root, config, &mut cancellation, deadline).await
        }
        .await;
        let linux_root = match preflight {
            Ok(path) => path,
            Err(error) => {
                return Preparation {
                    owner: None,
                    ready: Err(error),
                };
            }
        };
        if *cancellation.borrow() {
            return Preparation {
                owner: None,
                ready: Err(anyhow!("Final ASR preparation cancelled")),
            };
        }
        let mut owner = match Self::spawn(&linux_root, config) {
            Ok(owner) => owner,
            Err(error) => {
                return Preparation {
                    owner: None,
                    ready: Err(error),
                };
            }
        };
        let ready = owner
            .prepare_created(&mut cancellation, progress, deadline)
            .await;
        Preparation {
            owner: Some(owner),
            ready,
        }
    }

    fn spawn(linux_root: &str, config: &FinalAsrConfig) -> Result<Self> {
        let instance_id = uuid::Uuid::new_v4().simple().to_string();
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let wrapper = format!(
            "{}/bin/final-asr/managed_final_asr.py",
            linux_root.trim_end_matches('/')
        );
        let mut child = command()
            .args([
                "-d",
                &config.distribution,
                "--exec",
                &config.python_executable,
                "-B",
                &wrapper,
                "--root",
                linux_root,
                "--instance",
                &instance_id,
                "--python",
                &config.python_executable,
                "--cache-dir",
                &config.cache_directory,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let input = child.stdin.take();
        let output = child.stdout.take().expect("configured final ASR stdout");
        let mut errors = child.stderr.take().expect("configured final ASR stderr");
        let (send, events) = mpsc::channel(16);
        let instance = instance_id.clone();
        let reader = tokio::spawn(async move {
            let mut reader = BufReader::new(output);
            for _ in 0..16 {
                let result = match read_line(&mut reader).await {
                    Ok(Some(bytes)) => lifecycle_event(&bytes, &instance)
                        .map_err(|_| "final_asr_protocol_failed".to_owned()),
                    Ok(None) => return,
                    Err(_) => Err("final_asr_stdout_failed".into()),
                };
                let failed = result.is_err();
                if send.send(result).await.is_err() || failed {
                    return;
                }
            }
            let _ = send
                .send(Err("final_asr_lifecycle_event_limit".into()))
                .await;
        });
        let drain = tokio::spawn(async move {
            // No model log, token, path or arbitrary dependency text enters UI state.
            let mut bytes = [0u8; 8192];
            while errors.read(&mut bytes).await.is_ok_and(|count| count > 0) {}
        });
        Ok(Self {
            child,
            input,
            events,
            readers: vec![reader, drain],
            lifecycle: Lifecycle::default(),
            token,
            cleanup: None,
            instance_id,
        })
    }

    fn observe(&mut self, result: Result<Value, String>) -> Result<Value> {
        let value = result.map_err(|error| {
            self.lifecycle.protocol_failed = true;
            anyhow!(error)
        })?;
        if let Err(error) = self.lifecycle.observe(&value) {
            self.lifecycle.protocol_failed = true;
            return Err(error);
        }
        Ok(value)
    }

    async fn next(
        &mut self,
        cancel: &mut watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<Value> {
        let result = tokio::select! { biased;
            _ = wait_cancelled(cancel) => bail!("Final ASR preparation cancelled"),
            result = tokio::time::timeout_at(deadline, self.events.recv()) =>
                result.map_err(|_| anyhow!("Final ASR preparation timed out"))?
                    .ok_or_else(|| anyhow!("Final ASR lifecycle channel closed"))?,
        };
        self.observe(result)
    }

    async fn prepare_created(
        &mut self,
        cancel: &mut watch::Receiver<bool>,
        progress: mpsc::Sender<Value>,
        deadline: Instant,
    ) -> Result<Arc<FinalAsrClient>> {
        let bootstrap = format!(
            "{}\n",
            json!({"protocol_version":1,"instance_id":self.instance_id,"bearer_token":self.token})
        );
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| anyhow!("Final ASR control pipe missing"))?;
        tokio::select! { biased;
            _ = wait_cancelled(cancel) => bail!("Final ASR preparation cancelled"),
            result = tokio::time::timeout_at(deadline.min(Instant::now() + Duration::from_secs(2)), async {
                input.write_all(bootstrap.as_bytes()).await?;
                input.flush().await
            }) => result.map_err(|_| anyhow!("Final ASR bootstrap timed out"))??,
        };
        let mut starting = self.next(cancel, deadline).await?;
        if starting["state"] == "waiting_gpu" {
            let _ = progress.try_send(starting);
            starting = self.next(cancel, Instant::now() + GPU_WAIT_BUDGET).await?;
        }
        ensure!(
            starting["state"] == "starting",
            "Final ASR stopped or failed before model preparation"
        );
        // Queueing for another model's initialization is not model preparation.
        let deadline = Instant::now() + MODEL_PREPARATION_BUDGET;
        let _ = progress.try_send(starting);
        for expected in ["awaiting_owner", "loading", "warming", "warm_ready"] {
            let value = self.next(cancel, deadline).await?;
            ensure!(
                value["state"] == expected,
                "Final ASR stopped or failed during preparation"
            );
            let _ = progress.try_send(value);
        }
        let client = Arc::new(FinalAsrClient::new(
            self.instance_id.clone(),
            self.lifecycle
                .endpoint
                .clone()
                .ok_or_else(|| anyhow!("Final ASR warm endpoint missing"))?,
            self.token.clone(),
        )?);
        tokio::select! { biased;
            _ = wait_cancelled(cancel) => bail!("Final ASR preparation cancelled"),
            result = tokio::time::timeout_at(deadline, client.check_warm()) =>
                result.map_err(|_| anyhow!("Final ASR warm health timed out"))??,
        };
        ensure!(
            !*cancel.borrow() && self.is_running()?,
            "Final ASR preparation cancelled or service exited"
        );
        Ok(client)
    }

    fn drain_events(&mut self) {
        while let Ok(value) = self.events.try_recv() {
            let _ = self.observe(value);
        }
    }

    pub fn is_running(&mut self) -> Result<bool> {
        self.drain_events();
        Ok(
            self.lifecycle.running()
                && !self.events.is_closed()
                && self.child.try_wait()?.is_none(),
        )
    }

    pub async fn monitor(&mut self, mut cancellation: watch::Receiver<bool>) -> Result<()> {
        loop {
            if *cancellation.borrow() {
                return Ok(());
            }
            ensure!(self.is_running()?, "Owned final ASR service stopped");
            tokio::select! { biased;
                _ = wait_cancelled(&mut cancellation) => return Ok(()),
                value = self.events.recv() => { self.observe(value.ok_or_else(|| anyhow!("Final ASR lifecycle channel closed"))?)?; }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
    }

    pub async fn stop(&mut self) -> Result<Value> {
        if let Some(receipt) = &self.cleanup {
            return Ok(receipt.clone());
        }
        let deadline = Instant::now() + Duration::from_secs(35);
        if let Some(mut input) = self.input.take() {
            let line = format!("{}\n", json!({"op":"stop","instance_id":self.instance_id}));
            let _ = tokio::time::timeout(Duration::from_secs(2), async {
                input.write_all(line.as_bytes()).await?;
                input.flush().await
            })
            .await;
            // EOF requests cleanup from this exact Linux owner as well.
        }
        loop {
            self.drain_events();
            if let Some(exit) = self.child.try_wait()? {
                for mut reader in std::mem::take(&mut self.readers) {
                    if tokio::time::timeout_at(deadline, &mut reader)
                        .await
                        .is_err()
                    {
                        reader.abort();
                    }
                }
                self.drain_events();
                let receipt = cleanup_receipt(
                    &self.instance_id,
                    exit.code(),
                    self.lifecycle.terminal.as_ref().ok_or_else(|| {
                        anyhow!("linux_cleanup_unconfirmed: final ASR owner receipt missing")
                    })?,
                    self.lifecycle.protocol_failed,
                )?;
                self.cleanup = Some(receipt.clone());
                return Ok(receipt);
            }
            if Instant::now() >= deadline {
                // Killing this Windows handle is not proof that Linux GPU work stopped.
                let _ = self.child.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
                bail!("linux_cleanup_unconfirmed: final ASR owned bridge stop timed out");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for OwnedFinalAsrService {
    fn drop(&mut self) {
        self.input.take();
        for reader in &self.readers {
            reader.abort();
        }
        // kill_on_drop covers this bridge only; callers must await stop() for proof.
    }
}
