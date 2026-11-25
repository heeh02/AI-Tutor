//! One owned Windows CPU service; never attach to the laboratory service.
use super::{AsrClient, AsrMode, LocalAsrPolicy, MODEL_DIRECTORY};
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::watch,
    time::Instant,
};

#[derive(Clone, Copy)]
enum AsrBackend {
    Zipformer,
    #[cfg(test)]
    Paraformer,
}
impl AsrBackend {
    fn argument(self) -> &'static str {
        match self {
            Self::Zipformer => "zipformer",
            #[cfg(test)]
            Self::Paraformer => "paraformer",
        }
    }
    fn model_directory(self) -> &'static str {
        match self {
            Self::Zipformer => MODEL_DIRECTORY,
            #[cfg(test)]
            Self::Paraformer => OwnedAsrService::PARAFORMER_DIAGNOSTIC_MODEL_DIRECTORY,
        }
    }
    fn model_files(self) -> &'static [&'static str] {
        match self {
            Self::Zipformer => &[
                "tokens.txt",
                "encoder-epoch-99-avg-1.int8.onnx",
                "decoder-epoch-99-avg-1.onnx",
                "joiner-epoch-99-avg-1.int8.onnx",
            ],
            #[cfg(test)]
            Self::Paraformer => &["tokens.txt", "encoder.int8.onnx", "decoder.int8.onnx"],
        }
    }
}

pub(crate) struct OwnedAsrService {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    job: AsrJob,
    interpreter: Option<InterpreterWait>,
    pub instance_id: String,
    token: String,
    released: bool,
    backend: AsrBackend,
}
impl OwnedAsrService {
    /// The returned process is waiting at its stdin gate. Call prepare next;
    /// on any preparation error retain this owner and await stop's receipt.
    pub fn spawn(root: &Path, policy: &LocalAsrPolicy) -> Result<Self> {
        Self::spawn_model(root, policy, AsrBackend::Zipformer)
    }

    #[cfg(test)]
    pub(crate) const PARAFORMER_DIAGNOSTIC_MODEL_DIRECTORY: &str =
        "models/speech/sherpa-onnx-streaming-paraformer-bilingual-zh-en-8e40c43";

    /// Only the explicitly invoked diagnostic can select this candidate.
    /// Reuse prepare/stop and retain failed owners exactly as for Zipformer.
    #[cfg(test)]
    pub(crate) fn spawn_paraformer_diagnostic(
        root: &Path,
        policy: &LocalAsrPolicy,
    ) -> Result<Self> {
        Self::spawn_model(root, policy, AsrBackend::Paraformer)
    }

    fn spawn_model(root: &Path, policy: &LocalAsrPolicy, backend: AsrBackend) -> Result<Self> {
        ensure!(policy.mode == AsrMode::Managed, "本地识别托管未启用");
        let python = policy
            .python_executable
            .as_ref()
            .ok_or_else(|| anyhow!("缺少识别运行环境"))?;
        ensure!(
            python.is_absolute() && python.is_file(),
            "识别运行环境不可用"
        );
        let script = root.join("bin/local-asr/managed_asr.py");
        for name in ["managed_asr.py", "asr_service.py", "recognizer.py"] {
            ensure!(
                root.join("bin/local-asr").join(name).is_file(),
                "本地识别程序尚未安装"
            );
        }
        for name in backend.model_files() {
            ensure!(
                root.join(backend.model_directory()).join(name).is_file(),
                "本地识别模型尚未安装"
            );
        }
        let job = AsrJob::new()?;
        let instance_id = uuid::Uuid::new_v4().simple().to_string();
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let mut command = Command::new(python);
        command
            .arg("-u")
            .arg(script)
            .arg("--root")
            .arg(root)
            .arg("--instance")
            .arg(&instance_id)
            .arg("--backend")
            .arg(backend.argument())
            .env("PYTHONUTF8", "1")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        let mut child = command.spawn()?;
        let input = child.stdin.take();
        let output = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| anyhow!("识别启动回执通道缺失"))?,
        );
        Ok(Self {
            child,
            input,
            output,
            job,
            interpreter: None,
            instance_id,
            token,
            released: false,
            backend,
        })
    }

    pub async fn prepare(
        &mut self,
        cancelled: &mut watch::Receiver<bool>,
        mut progress: impl FnMut(Value),
    ) -> Result<Arc<AsrClient>> {
        // The model and HTTP listener cannot be created until after this attach
        // and the private bootstrap. No bare PID is used to terminate a process.
        let startup_deadline = Instant::now() + Duration::from_secs(60);
        self.job.attach(&self.child)?;
        let first = self
            .event(cancelled, Instant::now() + Duration::from_secs(5))
            .await?;
        ensure!(first["state"] == "awaiting_owner", "识别启动握手顺序无效");
        let pid = first["interpreter_pid"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| *v != 0)
            .ok_or_else(|| anyhow!("识别解释器回执无效"))?;
        self.interpreter = Some(InterpreterWait::open(pid)?);
        self.interpreter.as_mut().unwrap().ensure_owned(&self.job)?;
        let bootstrap =
            json!({"protocol_version":1,"instance_id":self.instance_id,"bearer_token":self.token});
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| anyhow!("识别控制通道已关闭"))?;
        let mut line = serde_json::to_vec(&bootstrap)?;
        line.push(b'\n');
        tokio::select! {
            _ = cancelled.changed() => bail!("本地识别准备已取消"),
            sent = tokio::time::timeout(Duration::from_secs(1), async {
                input.write_all(&line).await?; input.flush().await
            }) => { sent.map_err(|_| anyhow!("识别启动握手超时"))??; }
        }
        self.released = true;
        let loading = self.event(cancelled, startup_deadline).await?;
        ensure!(
            loading["state"] == "loading"
                && loading["ready"] == false
                && loading["warmup_completed"] == false,
            "识别加载回执无效"
        );
        let endpoint = loading["base_url"]
            .as_str()
            .ok_or_else(|| anyhow!("识别地址回执缺失"))?
            .to_owned();
        let client = Arc::new(AsrClient::new(
            self.instance_id.clone(),
            endpoint.clone(),
            self.token.clone(),
        )?);
        progress(loading);
        let warming = self.event(cancelled, startup_deadline).await?;
        ensure!(
            warming["state"] == "warming"
                && warming["base_url"] == endpoint
                && warming["ready"] == true
                && warming["warmup_completed"] == false,
            "识别预热回执无效"
        );
        progress(warming);
        let warm = self
            .event(cancelled, Instant::now() + Duration::from_secs(30))
            .await?;
        ensure!(
            warm["state"] == "warm_ready"
                && warm["base_url"] == endpoint
                && warm["ready"] == true
                && warm["warmup_completed"] == true,
            "识别预热未完成"
        );
        tokio::select! {
            _=cancelled.changed()=>bail!("本地识别准备已取消"),
            result=client.check_warm()=>result?,
        }
        ensure!(self.is_running()?, "识别进程已退出");
        progress(warm);
        Ok(client)
    }

    async fn event(
        &mut self,
        cancelled: &mut watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<Value> {
        ensure!(!*cancelled.borrow(), "本地识别准备已取消");
        let value = tokio::select! {
            _=cancelled.changed()=>bail!("本地识别准备已取消"),
            result=tokio::time::timeout_at(deadline, read_event(&mut self.output))=>
                result.map_err(|_| anyhow!("本地识别准备超时"))??,
        };
        ensure!(
            value["protocol_version"] == 1 && value["instance_id"] == self.instance_id,
            "识别进程实例回执不匹配"
        );
        ensure!(
            value["backend"] == self.backend.argument(),
            "识别进程模型回执不匹配"
        );
        ensure!(
            value["state"] != "failed" && value["state"] != "stopped",
            "本地识别准备失败或已停止"
        );
        Ok(value)
    }
    pub fn is_running(&mut self) -> Result<bool> {
        Ok(self.child.try_wait()?.is_none()
            && self.interpreter.as_ref().is_none_or(|p| !p.exited()))
    }
    pub async fn stop(&mut self) -> Value {
        if let Some(mut input) = self.input.take() {
            let line = format!("{}\n", json!({"op":"stop","instance_id":self.instance_id}));
            let _ = tokio::time::timeout(Duration::from_millis(250), async {
                input.write_all(line.as_bytes()).await?;
                input.flush().await
            })
            .await;
            // Dropping stdin is a second owned stop signal, never a port lookup.
        }
        let mut forced = false;
        let grace_deadline = Instant::now() + Duration::from_secs(5);
        let mut child_exited = matches!(
            tokio::time::timeout_at(grace_deadline, self.child.wait()).await,
            Ok(Ok(_))
        );
        // The launcher can finish before its redirected interpreter or the
        // kernel's Job accounting. Give both the remainder of this same grace
        // window, rather than treating that transient order as a forced stop.
        while child_exited
            && (!self.job.empty()
                || self
                    .interpreter
                    .as_ref()
                    .is_some_and(|p| !p.cleanup_complete()))
            && Instant::now() < grace_deadline
        {
            tokio::time::sleep_until(
                (Instant::now() + Duration::from_millis(20)).min(grace_deadline),
            )
            .await;
        }
        let graceful_complete = child_exited
            && self
                .interpreter
                .as_ref()
                .is_none_or(InterpreterWait::cleanup_complete)
            && self.job.empty();
        if !graceful_complete {
            forced = true;
            if let Some(interpreter) = &self.interpreter {
                interpreter.terminate_owned_job();
            }
            self.job.terminate();
            let _ = self.child.start_kill();
            child_exited = matches!(
                tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await,
                Ok(Ok(_))
            );
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline
            && (!self.job.empty()
                || self
                    .interpreter
                    .as_ref()
                    .is_some_and(|p| !p.cleanup_complete()))
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let interpreter_exited = self.interpreter.as_ref().map(InterpreterWait::exited);
        let interpreter_job_empty = self.interpreter.as_ref().map(InterpreterWait::job_empty);
        let confirmed = child_exited
            && self.job.empty()
            && self
                .interpreter
                .as_ref()
                .map(InterpreterWait::cleanup_complete)
                .unwrap_or(!self.released);
        json!({"instance_id":self.instance_id,"child_exited":child_exited,
            "interpreter_exited":interpreter_exited,"forced":forced,
            "launcher_job_empty":self.job.empty(),"interpreter_job_empty":interpreter_job_empty,
            "interpreter_job_owned":self.interpreter.as_ref().is_some_and(|p|p.owned),
            "cleanup_confirmed":confirmed,"model_release_sent":self.released})
    }
}
impl Drop for OwnedAsrService {
    fn drop(&mut self) {
        self.input.take();
        if let Some(interpreter) = &self.interpreter {
            interpreter.terminate_owned_job();
        }
        self.job.terminate();
    }
}

async fn read_event(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Value> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        ensure!(!available.is_empty(), "识别进程回执通道已关闭");
        let length = available
            .iter()
            .position(|b| *b == b'\n')
            .map(|n| n + 1)
            .unwrap_or(available.len());
        ensure!(line.len() + length <= 8192, "识别进程回执超限");
        let finished = available[length - 1] == b'\n';
        line.extend_from_slice(&available[..length]);
        reader.consume(length);
        if finished {
            break;
        }
    }
    serde_json::from_slice(&line).map_err(|_| anyhow!("识别进程回执格式无效"))
}

#[cfg(windows)]
struct AsrJob(isize);
#[cfg(windows)]
impl AsrJob {
    fn new() -> Result<Self> {
        use windows_sys::Win32::{Foundation::CloseHandle, System::JobObjects::*};
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        ensure!(!handle.is_null(), "无法建立识别进程停止保障");
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let success = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if success == 0 {
            unsafe { CloseHandle(handle) };
            bail!("识别进程停止保障配置失败");
        }
        Ok(Self(handle as isize))
    }
    fn attach(&self, child: &Child) -> Result<()> {
        let handle = child
            .raw_handle()
            .ok_or_else(|| anyhow!("识别子进程句柄缺失"))?;
        self.attach_handle(handle)
    }
    fn attach_handle(&self, handle: windows_sys::Win32::Foundation::HANDLE) -> Result<()> {
        ensure!(
            unsafe {
                windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                    self.0 as _,
                    handle,
                )
            } != 0,
            "无法隔离自有识别进程，未启动模型"
        );
        Ok(())
    }
    fn contains(&self, handle: windows_sys::Win32::Foundation::HANDLE) -> Result<bool> {
        let mut contains = 0;
        ensure!(
            unsafe {
                windows_sys::Win32::System::JobObjects::IsProcessInJob(
                    handle,
                    self.0 as _,
                    &mut contains,
                )
            } != 0,
            "无法确认识别解释器进程归属"
        );
        Ok(contains != 0)
    }
    fn empty(&self) -> bool {
        use windows_sys::Win32::System::JobObjects::*;
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        unsafe {
            QueryInformationJobObject(
                self.0 as _,
                JobObjectBasicAccountingInformation,
                (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                std::mem::size_of_val(&info) as u32,
                std::ptr::null_mut(),
            ) != 0
                && info.ActiveProcesses == 0
        }
    }
    fn terminate(&self) {
        unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0 as _, 1) };
    }
}
#[cfg(windows)]
impl Drop for AsrJob {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0 as _) };
    }
}
#[cfg(windows)]
struct InterpreterWait {
    handle: isize,
    /// A venv redirector may have created this interpreter before its own
    /// assignment. An empty job can be nested under that interpreter's existing
    /// job chain; the already populated launcher job cannot always be added.
    separate_job: Option<AsrJob>,
    owned: bool,
}
#[cfg(windows)]
impl InterpreterWait {
    fn open(pid: u32) -> Result<Self> {
        // PID comes only from our private bootstrap pipe. Keep this same handle
        // for assignment, ownership verification and exit confirmation. The
        // terminate right is required by AssignProcessToJobObject; termination
        // below still uses only owned jobs, never a later PID lookup.
        let handle = unsafe {
            windows_sys::Win32::System::Threading::OpenProcess(
                windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE
                    | windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION
                    | windows_sys::Win32::System::Threading::PROCESS_SET_QUOTA
                    | windows_sys::Win32::System::Threading::PROCESS_TERMINATE,
                0,
                pid,
            )
        };
        ensure!(!handle.is_null(), "无法确认自有识别解释器退出");
        Ok(Self {
            handle: handle as isize,
            separate_job: None,
            owned: false,
        })
    }
    fn ensure_owned(&mut self, launcher_job: &AsrJob) -> Result<()> {
        if launcher_job.contains(self.handle as _)? {
            self.owned = true;
            return Ok(());
        }
        // Keep the job even if a subsequent verification fails, so stop/Drop
        // still reclaim any process whose assignment already succeeded.
        self.separate_job = Some(AsrJob::new()?);
        let job = self.separate_job.as_ref().unwrap();
        job.attach_handle(self.handle as _)?;
        ensure!(
            job.contains(self.handle as _)?,
            "识别解释器未受自有进程停止保障管理"
        );
        self.owned = true;
        Ok(())
    }
    fn terminate_owned_job(&self) {
        if let Some(job) = &self.separate_job {
            job.terminate();
        }
    }
    fn job_empty(&self) -> bool {
        self.separate_job.as_ref().is_none_or(AsrJob::empty)
    }
    fn cleanup_complete(&self) -> bool {
        self.exited() && self.job_empty()
    }
    fn exited(&self) -> bool {
        unsafe {
            windows_sys::Win32::System::Threading::WaitForSingleObject(self.handle as _, 0)
                == windows_sys::Win32::Foundation::WAIT_OBJECT_0
        }
    }
}
#[cfg(windows)]
impl Drop for InterpreterWait {
    fn drop(&mut self) {
        self.terminate_owned_job();
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle as _) };
    }
}
#[cfg(not(windows))]
struct AsrJob;
#[cfg(not(windows))]
impl AsrJob {
    fn new() -> Result<Self> {
        bail!("托管本地识别目前仅支持 Windows")
    }
    fn attach(&self, _: &Child) -> Result<()> {
        bail!("不支持的系统")
    }
    fn empty(&self) -> bool {
        false
    }
    fn terminate(&self) {}
}
#[cfg(not(windows))]
struct InterpreterWait {
    owned: bool,
}
#[cfg(not(windows))]
impl InterpreterWait {
    fn open(_: u32) -> Result<Self> {
        bail!("不支持的系统")
    }
    fn exited(&self) -> bool {
        false
    }
    fn ensure_owned(&mut self, _: &AsrJob) -> Result<()> {
        bail!("不支持的系统")
    }
    fn terminate_owned_job(&self) {}
    fn job_empty(&self) -> bool {
        false
    }
    fn cleanup_complete(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paraformer_diagnostic_keeps_managed_mode_gate_before_any_launch() {
        let error = OwnedAsrService::spawn_paraformer_diagnostic(
            Path::new("not-an-installed-runtime"),
            &LocalAsrPolicy::default(),
        )
        .err()
        .expect("disabled policy must never launch a diagnostic service");
        assert_eq!(error.to_string(), "本地识别托管未启用");
    }

    #[cfg(windows)]
    const FIXTURE_TEST: &str =
        "local_asr::owned::tests::interpreter_created_before_launcher_assignment_has_owned_cleanup";
    #[cfg(windows)]
    const FIXTURE_ROLE: &str = "AI_TUTOR_ASR_JOB_FIXTURE_ROLE";

    #[cfg(windows)]
    fn fixture_command(role: &str) -> Result<Command> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--exact", FIXTURE_TEST, "--nocapture", "--test-threads=1"])
            .env(FIXTURE_ROLE, role)
            .creation_flags(0x08000000)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Ok(command)
    }

    #[cfg(windows)]
    fn fixture_role() -> Result<bool> {
        use std::{
            io::Write,
            os::windows::{io::AsRawHandle, process::CommandExt},
        };
        let Ok(role) = std::env::var(FIXTURE_ROLE) else {
            return Ok(false);
        };
        if matches!(
            role.as_str(),
            "interpreter" | "exit_on_input" | "delayed_exit"
        ) {
            println!("\nASR_JOB_FIXTURE {}", std::process::id());
            std::io::stdout().flush()?;
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            if role != "interpreter" {
                if role == "delayed_exit" {
                    std::thread::sleep(Duration::from_millis(200));
                }
                return Ok(true);
            }
        } else {
            ensure!(role == "redirector", "unknown fixture role");
            // Simulate a venv redirector's private, already populated job. The
            // parent sees this marker only after the interpreter is created.
            let redirector_job = AsrJob::new()?;
            let mut child = std::process::Command::new(std::env::current_exe()?)
                .args(["--exact", FIXTURE_TEST, "--nocapture", "--test-threads=1"])
                .env(FIXTURE_ROLE, "interpreter")
                .creation_flags(0x08000000)
                .stdin(Stdio::inherit())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            if let Err(error) = redirector_job.attach_handle(child.as_raw_handle()) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            println!("\nASR_JOB_FIXTURE {}", child.id());
            std::io::stdout().flush()?;
            // Hold the redirector and its job alive even after interpreter exit:
            // the test must prove our interpreter job performs the termination.
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    #[cfg(windows)]
    async fn fixture_pid(child: &mut Child) -> Result<(u32, BufReader<ChildStdout>)> {
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut line = String::new();
                ensure!(
                    reader.read_line(&mut line).await? != 0,
                    "fixture handshake missing"
                );
                if let Some((_, pid)) = line.split_once("ASR_JOB_FIXTURE ") {
                    return Ok((pid.trim().parse::<u32>()?, reader));
                }
            }
        })
        .await?
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn interpreter_created_before_launcher_assignment_has_owned_cleanup() -> Result<()> {
        if fixture_role()? {
            return Ok(());
        }
        let launcher_job = AsrJob::new()?;
        let mut launcher = fixture_command("redirector")?.spawn()?;
        let (pid, _) = fixture_pid(&mut launcher).await?;
        // This order deterministically forces the real race: assignment occurs
        // only after the separate interpreter and its original job both exist.
        launcher_job.attach(&launcher)?;
        let mut interpreter = InterpreterWait::open(pid)?;
        assert!(!launcher_job.contains(interpreter.handle as _)?);
        interpreter.ensure_owned(&launcher_job)?;
        assert!(interpreter.owned && interpreter.separate_job.is_some());
        assert!(
            interpreter
                .separate_job
                .as_ref()
                .unwrap()
                .contains(interpreter.handle as _)?
        );
        interpreter.terminate_owned_job();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !interpreter.cleanup_complete() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(interpreter.cleanup_complete());
        assert!(
            launcher.try_wait()?.is_none(),
            "interpreter exit must not depend on killing redirector"
        );
        launcher_job.terminate();
        tokio::time::timeout(Duration::from_secs(3), launcher.wait()).await??;
        assert!(launcher_job.empty());
        Ok(())
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn interpreter_already_in_launcher_job_reuses_owned_job() -> Result<()> {
        let launcher_job = AsrJob::new()?;
        let mut child = fixture_command("interpreter")?.spawn()?;
        let (pid, _) = fixture_pid(&mut child).await?;
        launcher_job.attach(&child)?;
        let mut interpreter = InterpreterWait::open(pid)?;
        interpreter.ensure_owned(&launcher_job)?;
        assert!(interpreter.owned && interpreter.separate_job.is_none());
        launcher_job.terminate();
        tokio::time::timeout(Duration::from_secs(3), child.wait()).await??;
        assert!(interpreter.cleanup_complete() && launcher_job.empty());
        Ok(())
    }

    #[cfg(windows)]
    async fn independent_interpreter_fixture(
        role: &str,
    ) -> Result<(OwnedAsrService, Child, BufReader<ChildStdout>)> {
        let job = AsrJob::new()?;
        let mut child = fixture_command("exit_on_input")?.spawn()?;
        let (_, output) = fixture_pid(&mut child).await?;
        job.attach(&child)?;
        let mut actual = fixture_command(role)?.spawn()?;
        let (pid, actual_output) = fixture_pid(&mut actual).await?;
        let mut interpreter = InterpreterWait::open(pid)?;
        interpreter.ensure_owned(&job)?;
        assert!(interpreter.separate_job.is_some());
        let input = child.stdin.take();
        Ok((
            OwnedAsrService {
                child,
                input,
                output,
                job,
                interpreter: Some(interpreter),
                instance_id: uuid::Uuid::new_v4().simple().to_string(),
                token: "synthetic".into(),
                released: true,
                backend: AsrBackend::Zipformer,
            },
            actual,
            actual_output,
        ))
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn stop_waits_remaining_grace_after_launcher_exits_without_forcing() -> Result<()> {
        let (mut owned, mut actual, _actual_output) =
            independent_interpreter_fixture("delayed_exit").await?;
        let launcher = InterpreterWait::open(owned.child.id().unwrap())?;
        let mut input = actual.stdin.take().unwrap();
        // A real process-handle barrier proves interpreter exit occurs after
        // launcher exit. The interpreter then takes another 200 ms naturally.
        let release = async {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !launcher.exited() && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            ensure!(launcher.exited(), "fixture launcher did not stop");
            input.write_all(b"finish\n").await?;
            input.flush().await?;
            Ok::<_, anyhow::Error>(())
        };
        let (receipt, released) = tokio::join!(owned.stop(), release);
        released?;
        assert_eq!(receipt["forced"], false);
        assert_eq!(receipt["child_exited"], true);
        assert_eq!(receipt["interpreter_exited"], true);
        assert_eq!(receipt["launcher_job_empty"], true);
        assert_eq!(receipt["interpreter_job_empty"], true);
        assert_eq!(receipt["cleanup_confirmed"], true);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), actual.wait())
                .await??
                .success()
        );
        Ok(())
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn stop_still_forces_stuck_interpreter_after_shared_grace_deadline() -> Result<()> {
        let (mut owned, mut actual, _actual_output) =
            independent_interpreter_fixture("interpreter").await?;
        let started = Instant::now();
        let receipt = owned.stop().await;
        assert!(started.elapsed() >= Duration::from_secs(5));
        assert_eq!(receipt["forced"], true);
        assert_eq!(receipt["interpreter_exited"], true);
        assert_eq!(receipt["launcher_job_empty"], true);
        assert_eq!(receipt["interpreter_job_empty"], true);
        assert_eq!(receipt["cleanup_confirmed"], true);
        tokio::time::timeout(Duration::from_secs(1), actual.wait()).await??;
        Ok(())
    }

    #[tokio::test]
    async fn owner_event_reader_is_bounded_and_requires_complete_json_line() {
        for bytes in [
            vec![b'x'; 8193],
            b"{\"state\":\"loading\"}".to_vec(),
            b"not-json\n".to_vec(),
        ] {
            assert!(
                read_event(&mut BufReader::new(bytes.as_slice()))
                    .await
                    .is_err()
            );
        }
        let bytes = b"{\"state\":\"loading\"}\n";
        assert_eq!(
            read_event(&mut BufReader::new(bytes.as_slice()))
                .await
                .unwrap()["state"],
            "loading"
        );
    }
}
