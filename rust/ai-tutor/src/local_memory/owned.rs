use super::{LocalMemoryPolicy, policy::{linux_absolute, windows_path_argument}, protocol::*};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{mpsc, watch},
    task::JoinHandle,
    time::Instant,
};

const PREPARE: Duration = Duration::from_secs(300);
const ENCODE: Duration = Duration::from_secs(5);
const STOP: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, PartialEq)]
enum Phase { Gated, Preparing, Ready, Encoding, Failed, Stopping, Stopped }

/// A failed owner must be retained until stop has an actual Linux cleanup and
/// bridge-exit receipt. There is no implicit retry or attachment to another helper.
pub struct OwnedEmbedding {
    child: Child,
    input: Option<ChildStdin>,
    events: mpsc::Receiver<Result<Value, String>>,
    reader: Option<JoinHandle<()>>,
    pub instance_id: String,
    phase: Phase,
    pending: Option<Pending>,
    terminal: Option<Value>,
    stop_result: Option<Value>,
}

fn wsl() -> Command {
    let mut command = Command::new("wsl.exe");
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    command
}

async fn converted_path(distribution: &str, path: &Path) -> Result<String> {
    let argument = windows_path_argument(path)?;
    let mut command = wsl();
    command.args(["--distribution",distribution,"--exec","wslpath","-u"])
        .arg(argument).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| anyhow!("local_memory_wslpath_spawn_failed"))?;
    let output = child.stdout.take().ok_or_else(|| anyhow!("local_memory_wslpath_pipe_missing"))?;
    let read = async {
        let mut bytes = Vec::new();
        output.take(4097).read_to_end(&mut bytes).await?;
        ensure!(bytes.len() <= 4096, "local_memory_wslpath_output_too_large");
        ensure!(child.wait().await?.success(), "local_memory_wslpath_failed");
        let path = std::str::from_utf8(&bytes).map_err(|_| anyhow!("local_memory_wslpath_encoding"))?
            .trim_end_matches(['\r','\n']).to_owned();
        ensure!(linux_absolute(&path), "local_memory_wslpath_invalid");
        Ok(path)
    };
    tokio::time::timeout(Duration::from_secs(15), read).await
        .map_err(|_| anyhow!("local_memory_wslpath_timeout"))?
}

async fn cancelled(receiver: &mut watch::Receiver<bool>) {
    loop {
        if *receiver.borrow() { return; }
        if receiver.changed().await.is_err() { return; }
    }
}

impl OwnedEmbedding {
    /// An exited bridge still needs stop's Linux cleanup receipt. This is only
    /// an idle liveness observation, never permission to claim group cleanup.
    pub fn exited(&mut self) -> Result<bool> {
        Ok(self.child.try_wait()?.is_some())
    }

    /// Starts only the supervisor's private stdin gate. No model is loaded until prepare.
    pub async fn spawn(root: &Path, policy: &LocalMemoryPolicy) -> Result<Self> {
        policy.validate()?;
        for name in ["managed_embedding.py","embedding_worker.py"] {
            ensure!(root.join("bin/local-memory").join(name).is_file(), "local_memory_bundle_missing");
        }
        let root = std::fs::canonicalize(root)?;
        let linux_root = converted_path(&policy.distribution, &root).await?;
        let linux_model = converted_path(&policy.distribution, &policy.model_directory).await?;
        let instance_id = uuid::Uuid::new_v4().simple().to_string();
        let mut command = wsl();
        command.args(["--distribution",&policy.distribution,"--exec",&policy.python_executable,"-u"])
            .arg(format!("{linux_root}/bin/local-memory/managed_embedding.py"))
            .args(["--instance",&instance_id,"--model-dir",&linux_model])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        // Closing stdin is the Linux ownership signal. Killing wsl.exe would
        // not prove that its Linux worker group has stopped.
        command.kill_on_drop(false);
        let mut child = command.spawn().map_err(|_| anyhow!("local_memory_supervisor_spawn_failed"))?;
        let input = child.stdin.take().ok_or_else(|| anyhow!("local_memory_control_pipe_missing"))?;
        let output = child.stdout.take().ok_or_else(|| anyhow!("local_memory_event_pipe_missing"))?;
        let (send, events) = mpsc::channel(8);
        let reader = tokio::spawn(read_events(BufReader::new(output),send));
        Ok(Self { child, input:Some(input), events, reader:Some(reader), instance_id,
            phase:Phase::Gated, pending:None, terminal:None, stop_result:None })
    }

    async fn send(&mut self, bytes: &[u8], deadline: Instant) -> Result<()> {
        let input = self.input.as_mut().ok_or_else(|| anyhow!("local_memory_control_closed"))?;
        tokio::time::timeout_at(deadline, async {
            input.write_all(bytes).await?;
            input.flush().await
        }).await.map_err(|_| anyhow!("local_memory_write_timeout"))??;
        Ok(())
    }

    async fn event(&mut self, deadline: Instant) -> Result<Value> {
        let event = tokio::time::timeout_at(deadline,self.events.recv()).await
            .map_err(|_| anyhow!("local_memory_response_timeout"))?
            .ok_or_else(|| anyhow!("local_memory_supervisor_eof"))?
            .map_err(anyhow::Error::msg)?;
        let state = common(&event,&self.instance_id)?;
        if state == "stopped" { self.terminal = Some(event.clone()); }
        Ok(event)
    }

    pub async fn prepare(&mut self, cancellation: &mut watch::Receiver<bool>, mut progress: impl FnMut(Value)) -> Result<Value> {
        ensure!(self.phase == Phase::Gated, "local_memory_prepare_already_attempted");
        self.phase = Phase::Preparing;
        let deadline = Instant::now() + PREPARE;
        let result = tokio::select! { biased;
            _=cancelled(cancellation)=>Err(anyhow!("local_memory_prepare_cancelled")),
            result=self.prepare_inner(deadline,&mut progress)=>result,
        };
        self.phase = if result.is_ok() { Phase::Ready } else { Phase::Failed };
        result
    }

    async fn prepare_inner(&mut self, deadline: Instant, progress: &mut impl FnMut(Value)) -> Result<Value> {
        let awaiting = self.event(deadline.min(Instant::now()+Duration::from_secs(10))).await?;
        ensure!(common(&awaiting,&self.instance_id)? == "awaiting_owner", "local_memory_gate_order_invalid");
        validate_awaiting(&awaiting)?;
        let mut start = control(&self.instance_id,"start"); start["profile"] = PROFILE.metadata();
        self.send(&frame(&start)?,deadline.min(Instant::now()+Duration::from_secs(2))).await?;
        let starting = self.event(deadline).await?;
        ensure!(common(&starting,&self.instance_id)? == "starting", "local_memory_start_failed");
        validate_starting(&starting)?;
        progress(json!({"phase":"starting","instance_id":self.instance_id,"ready":false}));
        let loading = self.event(deadline).await?;
        ensure!(common(&loading,&self.instance_id)? == "loading", "local_memory_loading_failed");
        validate_loading(&loading)?;
        progress(json!({"phase":"loading","instance_id":self.instance_id,"ready":false}));
        let value = self.event(deadline).await?;
        ensure!(common(&value,&self.instance_id)? == "ready", "local_memory_prepare_failed");
        let mut metadata = ready(&value)?;
        metadata["instance_id"] = json!(self.instance_id);
        Ok(metadata)
    }

    /// The caller must await this operation, even when the query becomes stale.
    /// A timeout/error poisons the owner; retain it and call stop, never retry here.
    pub async fn encode(&mut self, request_id: &str, role: EmbeddingRole, text: &str) -> Result<EncodeResult> {
        ensure!(self.phase == Phase::Ready && self.pending.is_none(), "local_memory_not_ready");
        let (pending, bytes) = encode_request(&self.instance_id,request_id,role,text)?;
        self.pending = Some(pending.clone());
        self.phase = Phase::Encoding;
        let deadline = Instant::now() + ENCODE;
        let result = async {
            self.send(&bytes,deadline).await?;
            let value = self.event(deadline).await?;
            encoded(&value,&self.instance_id,&pending)
        }.await;
        if result.is_ok() {
            self.pending = None;
            self.phase = Phase::Ready;
        } else {
            self.phase = Phase::Failed;
        }
        result
    }

    /// Returning Ok requires both the Linux supervisor's verified group-reap
    /// receipt and an observed bridge exit. On timeout retain this owner and retry stop.
    pub async fn stop(&mut self) -> Result<Value> {
        if let Some(result) = &self.stop_result { return Ok(result.clone()); }
        self.phase = Phase::Stopping;
        let deadline = Instant::now() + STOP;
        if self.input.is_some() {
            let bytes = frame(&control(&self.instance_id,"stop"))?;
            let _ = self.send(&bytes,deadline.min(Instant::now()+Duration::from_secs(2))).await;
            self.input.take(); // EOF is also supervised; a broken pipe does not prove cleanup.
        }
        let mut protocol_error = false;
        let mut skipped = 0usize;
        while self.terminal.is_none() {
            let value = self.event(deadline).await?;
            match common(&value,&self.instance_id)? {
                "stopped" => {},
                "encoded" | "encode_error" => {
                    if let Some(pending) = self.pending.take() {
                        protocol_error |= encoded(&value,&self.instance_id,&pending).is_err();
                    } else { protocol_error = true; }
                }
                "awaiting_owner" => { protocol_error |= validate_awaiting(&value).is_err(); }
                "starting" => { protocol_error |= validate_starting(&value).is_err(); }
                "loading" => { protocol_error |= validate_loading(&value).is_err(); }
                "ready" => { protocol_error |= ready(&value).is_err(); }
                "failed" => {},
                _ => { protocol_error = true; }
            }
            skipped += 1;
            ensure!(skipped <= 16, "local_memory_stop_event_limit");
        }
        let receipt = stopped(self.terminal.as_ref().unwrap(),&self.instance_id)?;
        let exit = tokio::time::timeout_at(deadline,self.child.wait()).await
            .map_err(|_| anyhow!("local_memory_bridge_exit_timeout"))??;
        let result = json!({"linux_cleanup_confirmed":true,"group_reaped":true,
            "bridge_exited":true,"bridge_exit_code":exit.code(),"normal_bridge_exit":exit.success(),
            "protocol_error_while_stopping":protocol_error,"receipt":receipt});
        if let Some(reader) = self.reader.take() {
            // The bridge has exited and the supervisor terminal was consumed.
            reader.abort();
            let _ = reader.await;
        }
        self.phase = Phase::Stopped;
        self.pending = None;
        self.stop_result = Some(result.clone());
        Ok(result)
    }
}

impl Drop for OwnedEmbedding {
    fn drop(&mut self) {
        // Best-effort owner EOF only; Drop never claims a Linux cleanup receipt.
        self.input.take();
    }
}
