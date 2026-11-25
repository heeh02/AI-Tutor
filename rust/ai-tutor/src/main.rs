#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use ai_tutor::{
    kimi::KimiProvider,
    server::{Workstation, router},
    vision,
};
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::io::Write;
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

const HELP: &str = "AI Tutor — local Rust workstation\n\
Usage: ai-tutor [--root FOLDER] [--port 8765] [--no-browser | --browser EXE]\n\
       ai-tutor --stop [--root FOLDER]\n\
       ai-tutor --verify-kimi [--root FOLDER]\n\
       ai-tutor --prepare-demo [--root FOLDER]\n\
       ai-tutor --soak SECONDS [--root FOLDER]\n\
       ai-tutor --help\n\
--verify-kimi performs real remote no-tool verification using the existing Kimi login.\n\
--browser selects an absolute browser executable if the default browser cannot open.\n\
--no-browser starts the local service without opening a browser or showing dialogs.\n";

#[derive(Debug, PartialEq)]
enum Mode {
    Start,
    Stop,
    VerifyKimi,
    PrepareDemo,
    Soak(u64),
    Help,
    CameraDiscover,
    CameraWorker(PathBuf, String),
}
#[derive(Debug)]
struct Options {
    mode: Mode,
    root: Option<PathBuf>,
    port: u16,
    no_browser: bool,
    browser: Option<PathBuf>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut options = Self {
            mode: Mode::Start,
            root: None,
            port: 8765,
            no_browser: false,
            browser: None,
        };
        if args.len() == 2 && matches!(args[1].as_str(), "--help" | "-h") {
            options.mode = Mode::Help;
            return Ok(options);
        }
        if args.get(1).is_some_and(|v| v == "--camera-discover") {
            anyhow::ensure!(args.len() == 2, "Camera discovery takes no arguments");
            options.mode = Mode::CameraDiscover;
            return Ok(options);
        }
        if args.get(1).is_some_and(|v| v == "--camera-worker") {
            anyhow::ensure!(
                args.len() == 4,
                "Camera worker requires a root and device path"
            );
            options.mode = Mode::CameraWorker(PathBuf::from(&args[2]), args[3].clone());
            return Ok(options);
        }
        let mut seen = std::collections::HashSet::new();
        let mut index = 1;
        while index < args.len() {
            let flag = args[index].as_str();
            anyhow::ensure!(seen.insert(flag), "Duplicate option");
            let mut value = || -> Result<&str> {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| anyhow!("Missing option value"))?;
                anyhow::ensure!(!value.starts_with("--"), "Missing option value");
                Ok(value)
            };
            match flag {
                "--root" => options.root = Some(PathBuf::from(value()?)),
                "--port" => {
                    options.port = value()?
                        .parse()
                        .map_err(|_| anyhow!("Port must be 1–65535"))?;
                    anyhow::ensure!(options.port > 0, "Port must be 1–65535");
                }
                "--browser" => {
                    let path = PathBuf::from(value()?);
                    anyhow::ensure!(
                        path.is_absolute(),
                        "Browser executable must use an absolute path"
                    );
                    options.browser = Some(path);
                }
                "--no-browser" => options.no_browser = true,
                "--stop" | "--verify-kimi" | "--prepare-demo" | "--soak" => {
                    anyhow::ensure!(options.mode == Mode::Start, "Choose only one command");
                    options.mode = match flag {
                        "--stop" => Mode::Stop,
                        "--verify-kimi" => Mode::VerifyKimi,
                        "--prepare-demo" => Mode::PrepareDemo,
                        _ => {
                            let seconds = value()?
                                .parse::<u64>()
                                .map_err(|_| anyhow!("Soak duration must be 1–86400 seconds"))?;
                            anyhow::ensure!(
                                (1..=86400).contains(&seconds),
                                "Soak duration must be 1–86400 seconds"
                            );
                            Mode::Soak(seconds)
                        }
                    };
                }
                _ => return Err(anyhow!("Unknown option; use --help for supported commands")),
            }
            index += 1;
        }
        anyhow::ensure!(
            !(options.no_browser && options.browser.is_some()),
            "Choose either --no-browser or --browser"
        );
        anyhow::ensure!(
            options.mode == Mode::Start
                || (!seen.contains("--port") && !options.no_browser && options.browser.is_none()),
            "Browser and port options apply only when starting the workstation"
        );
        Ok(options)
    }
    fn interactive(&self) -> bool {
        matches!(self.mode, Mode::Start) && !self.no_browser
    }
}

#[derive(Clone, Copy, Debug)]
enum FailurePhase {
    ProjectRoot,
    Bind,
    Instance,
    Initialize,
    Descriptor,
    Browser,
    Serve,
    Command,
}
impl FailurePhase {
    fn code(self) -> &'static str {
        match self {
            Self::ProjectRoot => "project_root",
            Self::Bind => "local_port_unavailable",
            Self::Instance => "project_already_running",
            Self::Initialize => "initialization",
            Self::Descriptor => "local_descriptor",
            Self::Browser => "browser_open",
            Self::Serve => "local_service",
            Self::Command => "command_failed",
        }
    }
    fn message(self) -> &'static str {
        match self {
            Self::ProjectRoot => "找不到 AI Tutor 项目目录。请从项目中的 Start AI Tutor.cmd 启动。",
            Self::Bind => {
                "本地端口无法使用。请确认其他程序没有占用端口；也可使用 --port 指定另一端口。"
            }
            Self::Instance => {
                "此项目已有服务正在启动或退出。请稍候再启动；更换端口不会创建第二个实例。"
            }
            Self::Initialize => {
                "工作台初始化失败。请检查项目文件与本地数据目录是否可读取、可写入。"
            }
            Self::Descriptor => "无法保存本地启动信息。请检查项目 runtime 目录的写入权限。",
            Self::Browser => "工作台已运行，但浏览器没有打开。",
            Self::Serve => "本地服务已停止。请重新使用 Start AI Tutor.cmd 启动。",
            Self::Command => {
                "操作未完成。请检查本地服务状态；Kimi 核验请使用 --verify-kimi 并保留正常登录。"
            }
        }
    }
}
impl std::fmt::Display for FailurePhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}
impl std::error::Error for FailurePhase {}

fn record_failure(root: &Path, phase: FailurePhase, port: u16) -> Result<()> {
    std::fs::create_dir_all(root.join("runtime"))?;
    // Fixed fields only: never persist error chains, arguments, auth URLs or model context.
    std::fs::write(
        root.join("runtime/startup-error.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version":"1.0","timestamp_utc":chrono::Utc::now().to_rfc3339(),
            "code":phase.code(),"message":phase.message(),"port":port
        }))?,
    )?;
    Ok(())
}

fn show_notice(message: &str, error: bool) {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn MessageBoxW(
                window: *mut std::ffi::c_void,
                text: *const u16,
                caption: *const u16,
                kind: u32,
            ) -> i32;
        }
        let text = message.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let title = "AI Tutor".encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                if error { 0x10 } else { 0x30 } | 0x10000,
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = error;
        eprintln!("{message}");
    }
}

fn spawn_browser(executable: &Path, url: &str) -> std::io::Result<()> {
    let mut command = std::process::Command::new(executable);
    command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command.spawn().map(|_| ())
}

fn open_workbench(root: &Path, port: u16, token: &str, browser: Option<&Path>) {
    let url = format!("http://127.0.0.1:{port}/?token={token}");
    let opened = if let Some(executable) = browser {
        spawn_browser(executable, &url).is_ok()
    } else {
        webbrowser::open(&url).is_ok()
            || installed_browser_paths()
                .iter()
                .any(|path| path.is_file() && spawn_browser(path, &url).is_ok())
    };
    if !opened {
        let _ = record_failure(root, FailurePhase::Browser, port);
        let message = format!(
            "工作台已运行，本地入口：http://127.0.0.1:{port}/\n\n自动打开浏览器失败。已有登录的浏览器可直接访问该地址。需要首次登录时，请重新运行启动器，或使用 --browser 指定已安装浏览器的完整路径（见 README）。\n\n安全诊断：runtime/startup-error.json"
        );
        eprintln!("{message}");
        show_notice(&message, false);
    }
}

fn installed_browser_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for name in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(name) {
            for suffix in [
                "Microsoft/Edge/Application/msedge.exe",
                "Google/Chrome/Application/chrome.exe",
                "Mozilla Firefox/firefox.exe",
            ] {
                paths.push(PathBuf::from(&base).join(suffix));
            }
        }
    }
    paths
}

fn root_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(std::fs::canonicalize(path)?);
    }
    let current = std::env::current_dir()?;
    for base in [
        current,
        std::env::current_exe()?.parent().unwrap().to_path_buf(),
    ] {
        for ancestor in base.ancestors() {
            if ancestor.join("implementation-package").is_dir() && ancestor.join("models").is_dir()
            {
                return Ok(ancestor.to_owned());
            }
        }
    }
    Err(anyhow!("Cannot find AI Tutor project root"))
}

fn local_client(timeout: Duration) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(timeout)
        .build()?)
}

// The OS releases this lock even after a crash. Never remove the file: another
// launcher may already have it open, and replacing it would split ownership.
fn lock_project(root: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(root.join("runtime"))?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("runtime/server.lock"))?;
    file.try_lock()?;
    Ok(file)
}

fn descriptor_values(info: &Value) -> Result<(u16, &str)> {
    let port = info["port"]
        .as_u64()
        .and_then(|v| u16::try_from(v).ok())
        .filter(|v| *v != 0)
        .ok_or_else(|| anyhow!("Invalid local descriptor"))?;
    let token = info["token"]
        .as_str()
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 256
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .ok_or_else(|| anyhow!("Invalid local descriptor"))?;
    Ok((port, token))
}
#[tokio::main]
async fn main() -> ExitCode {
    let args = std::env::args().collect::<Vec<_>>();
    let options = match Options::parse(&args) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("Invalid command arguments.\n{HELP}");
            if !args.iter().any(|v| v == "--no-browser")
                && !args.get(1).is_some_and(|v| v.starts_with("--camera-"))
            {
                show_notice(
                    "启动参数无效。请使用 --help 查看可用命令；无工具核验命令是 --verify-kimi。",
                    true,
                );
            }
            return ExitCode::from(2);
        }
    };
    if let Err(error) = run(&options).await {
        let phase = error
            .downcast_ref::<FailurePhase>()
            .copied()
            .unwrap_or(FailurePhase::Command);
        let mut message = phase.message().to_owned();
        if !matches!(
            options.mode,
            Mode::CameraDiscover | Mode::CameraWorker(..) | Mode::Help
        ) {
            if let Ok(root) = root_path(options.root.as_deref()) {
                if record_failure(&root, phase, options.port).is_ok() {
                    message.push_str("\n安全诊断已保存到 runtime/startup-error.json。");
                }
            }
        }
        eprintln!("{message}");
        if options.interactive() {
            show_notice(&message, true);
        }
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

async fn run(options: &Options) -> Result<()> {
    if options.mode == Mode::Help {
        print!("{HELP}");
        return Ok(());
    }
    if options.mode == Mode::CameraDiscover {
        let devices = vision::discovery_worker().map_err(anyhow::Error::msg)?;
        println!("{}", json!(devices));
        return Ok(());
    }
    if let Mode::CameraWorker(root, device) = &options.mode {
        return vision::camera_worker(root.clone(), device.clone()).map_err(anyhow::Error::msg);
    }
    let root = root_path(options.root.as_deref()).context(FailurePhase::ProjectRoot)?;
    if let Mode::Soak(seconds) = options.mode {
        return soak(&root, seconds).await;
    }
    if options.mode == Mode::PrepareDemo {
        return prepare_demo(&root).await;
    }
    if options.mode == Mode::VerifyKimi {
        let provider = KimiProvider::new(&root)?;
        let result = provider.verify_real().await?;
        println!("{}", result);
        return Ok(());
    }
    if options.mode == Mode::Stop {
        if !root.join("runtime/server.json").exists() {
            println!("AI Tutor is already stopped");
            return Ok(());
        }
        let descriptor: Value =
            serde_json::from_slice(&std::fs::read(root.join("runtime/server.json"))?)?;
        let (port, token) = descriptor_values(&descriptor)?;
        let response = local_client(Duration::from_secs(5))?
            .post(format!("http://127.0.0.1:{port}/api/shutdown"))
            .header("Cookie", format!("ai_tutor_local={token}"))
            .json(&json!({}))
            .send()
            .await?
            .error_for_status()?;
        anyhow::ensure!(
            response.status().is_success(),
            "Local shutdown was not accepted"
        );
        println!("AI Tutor shutdown accepted; owned services are being released");
        return Ok(());
    }
    let port = options.port;
    let descriptor_path = root.join("runtime/server.json");
    if descriptor_path.exists() {
        if let Ok(info) = std::fs::read(&descriptor_path)
            .and_then(|v| serde_json::from_slice::<Value>(&v).map_err(std::io::Error::other))
        {
            if let Ok((port, token)) = descriptor_values(&info) {
                let client = local_client(Duration::from_secs(2))?;
                if client
                    .get(format!("http://127.0.0.1:{port}/api/state"))
                    .header("Cookie", format!("ai_tutor_local={token}"))
                    .send()
                    .await
                    .is_ok_and(|r| r.status().is_success())
                {
                    if !options.no_browser {
                        open_workbench(&root, port, token, options.browser.as_deref());
                    }
                    return Ok(());
                }
            }
        }
    }
    let _instance_lock = lock_project(&root).context(FailurePhase::Instance)?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .context(FailurePhase::Bind)?;
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let app = Workstation::new(root.clone(), token.clone()).context(FailurePhase::Initialize)?;
    app.provider
        .probe()
        .await
        .context(FailurePhase::Initialize)?;
    let memory_task = app
        .start_memory_schedule()
        .context(FailurePhase::Initialize)?;
    std::fs::create_dir_all(root.join("runtime")).context(FailurePhase::Descriptor)?;
    std::fs::write(
        &descriptor_path,
        serde_json::to_vec(
            &json!({"pid":std::process::id(),"port":port,"token":token,"implementation":"Rust"}),
        )?,
    )
    .context(FailurePhase::Descriptor)?;
    app.start_local_speech_runtime();
    app.start_local_asr_runtime();
    app.start_final_asr_runtime();
    app.start_local_memory_runtime();
    if !options.no_browser {
        let browser_root = root.clone();
        let browser_token = token.clone();
        let browser = options.browser.clone();
        // A browser error dialog must not hold up the local service or its shutdown.
        std::thread::spawn(move || {
            open_workbench(&browser_root, port, &browser_token, browser.as_deref());
        });
    }
    println!("Rust AI Tutor ready on127.0.0.1:{port}");
    let shutdown_app = app.clone();
    let serve_result = axum::serve(listener, router(app.clone()))
        .with_graceful_shutdown(async move {
            tokio::select! {_=shutdown_app.shutdown.notified()=>{},_=tokio::signal::ctrl_c()=>{}}
            shutdown_app.begin_shutdown().await;
        })
        .await;
    app.close().await;
    if let Some(mut task) = memory_task {
        if tokio::time::timeout(Duration::from_secs(3), &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
    serve_result.context(FailurePhase::Serve)?;
    if descriptor_path.exists() {
        let current: Value = serde_json::from_slice(&std::fs::read(&descriptor_path)?)?;
        if current["pid"] == std::process::id() {
            std::fs::remove_file(descriptor_path)?;
        }
    }
    Ok(())
}

fn descriptor(root: &std::path::Path) -> Result<(String, String)> {
    let info: Value = serde_json::from_slice(&std::fs::read(root.join("runtime/server.json"))?)?;
    let (port, token) = descriptor_values(&info)?;
    Ok((
        format!("http://127.0.0.1:{port}"),
        format!("ai_tutor_local={token}"),
    ))
}
async fn soak(root: &std::path::Path, seconds: u64) -> Result<()> {
    let (base, cookie) = descriptor(root)?;
    let client = local_client(Duration::from_secs(12))?;
    let initial: Value = client
        .get(format!("{base}/api/state"))
        .header("Cookie", &cookie)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    anyhow::ensure!(
        initial["implementation"]
            .as_str()
            .is_some_and(|v| v.starts_with("Rust")),
        "Soak requires native Rust service"
    );
    if initial["camera"]["state"] != "running" {
        client
            .post(format!("{base}/api/camera/open"))
            .header("Cookie", &cookie)
            .json(&json!({}))
            .send()
            .await?
            .error_for_status()?;
    }
    let start = std::time::Instant::now();
    let mut output = std::fs::File::create(root.join("runtime/soak-rust.jsonl"))?;
    let mut failures = 0;
    while start.elapsed() < Duration::from_secs(seconds) {
        let result=async{let state:Value=client.get(format!("{base}/api/state")).header("Cookie",&cookie).send().await?.error_for_status()?.json().await?;Ok::<Value,anyhow::Error>(json!({"elapsed":start.elapsed().as_secs_f64(),"camera":state["camera"],"memory_mb":state["metrics"]["memory_mb"],"requests":state["metrics"]["requests"],"implementation":state["implementation"]}))}.await;
        let row = match result {
            Ok(v) => v,
            Err(_) => {
                failures += 1;
                json!({"elapsed":start.elapsed().as_secs_f64(),"status":"request_failed"})
            }
        };
        writeln!(output, "{row}")?;
        output.flush()?;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    println!(
        "{}",
        json!({"native_rust_soak_seconds":start.elapsed().as_secs(),"request_failures":failures})
    );
    Ok(())
}
async fn prepare_demo(root: &std::path::Path) -> Result<()> {
    let (base, cookie) = descriptor(root)?;
    let client = local_client(Duration::from_secs(12))?;
    let state: Value = client
        .get(format!("{base}/api/state"))
        .header("Cookie", &cookie)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let learner = state["learners"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["name"] == "成人试用")
        .cloned();
    let learner = if let Some(value) = learner {
        value
    } else {
        client.post(format!("{base}/api/learners")).header("Cookie",&cookie).json(&json!({"name":"成人试用","namespace":"real","language":"zh","help_style":"one_step"})).send().await?.error_for_status()?.json::<Value>().await?
    };
    client
        .post(format!("{base}/api/sessions"))
        .header("Cookie", &cookie)
        .json(&json!({"learner_id":learner["learner_id"],"resume":true}))
        .send()
        .await?
        .error_for_status()?;
    if state["camera"]["state"] != "running" {
        client
            .post(format!("{base}/api/camera/open"))
            .header("Cookie", &cookie)
            .json(&json!({}))
            .send()
            .await?
            .error_for_status()?;
    }
    let token = cookie.strip_prefix("ai_tutor_local=").unwrap();
    let port = base
        .rsplit(':')
        .next()
        .and_then(|v| v.parse::<u16>().ok())
        .ok_or_else(|| anyhow!("Invalid server port"))?;
    open_workbench(root, port, token, None);
    println!("Native Rust adult test profile ready");
    Ok(())
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    fn parse(values: &[&str]) -> Result<Options> {
        Options::parse(
            &std::iter::once("ai-tutor")
                .chain(values.iter().copied())
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        )
    }
    #[test]
    fn help_needs_no_project_or_service() {
        assert_eq!(parse(&["--help"]).unwrap().mode, Mode::Help);
        assert_eq!(parse(&["-h"]).unwrap().mode, Mode::Help);
    }
    #[test]
    fn rejects_unknown_and_old_verification_flag() {
        for args in [
            &["--typo"][..],
            &["--probe-kimi"][..],
            &["--root", ".", "private-child-input"][..],
        ] {
            assert!(parse(args).is_err());
        }
    }
    #[test]
    fn rejects_missing_duplicate_and_invalid_values() {
        for args in [
            &["--root"][..],
            &["--root", "--stop"][..],
            &["--port", "0"][..],
            &["--port", "65536"][..],
            &["--port", "no"][..],
            &["--port", "8765", "--port", "8766"][..],
            &["--soak", "0"][..],
            &["--soak", "86401"][..],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }
    #[test]
    fn rejects_conflicting_commands_and_ignored_options() {
        for args in [
            &["--stop", "--verify-kimi"][..],
            &["--verify-kimi", "--port", "8765"][..],
            &["--stop", "--no-browser"][..],
            &["--no-browser", "--browser", "C:\\Browser\\browser.exe"][..],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }
    #[test]
    fn verification_command_and_headless_start_parse() {
        assert_eq!(
            parse(&["--root", ".", "--verify-kimi"]).unwrap().mode,
            Mode::VerifyKimi
        );
        let options = parse(&["--root", ".", "--no-browser", "--port", "8766"]).unwrap();
        assert!(!options.interactive());
        assert_eq!(options.port, 8766);
    }
    #[test]
    fn camera_internal_arguments_remain_strict() {
        assert_eq!(
            parse(&["--camera-discover"]).unwrap().mode,
            Mode::CameraDiscover
        );
        assert!(parse(&["--camera-discover", "extra"]).is_err());
        assert!(parse(&["--camera-worker", "."]).is_err());
        assert!(matches!(
            parse(&["--camera-worker", ".", "device"]).unwrap().mode,
            Mode::CameraWorker(..)
        ));
    }
    #[test]
    fn diagnostic_has_only_safe_fields() {
        let directory = tempfile::tempdir().unwrap();
        let private_error =
            anyhow!("token=secret-token private learner prompt").context(FailurePhase::Bind);
        let phase = private_error
            .downcast_ref::<FailurePhase>()
            .copied()
            .unwrap();
        record_failure(directory.path(), phase, 8765).unwrap();
        let body =
            std::fs::read_to_string(directory.path().join("runtime/startup-error.json")).unwrap();
        assert!(!body.contains("secret-token"));
        assert!(!body.contains("learner prompt"));
        assert!(!body.contains("?token="));
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["code"], "local_port_unavailable");
        assert_eq!(value.as_object().unwrap().len(), 5);
    }
}
