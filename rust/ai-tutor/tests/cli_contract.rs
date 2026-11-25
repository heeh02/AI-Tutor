//! Execute the actual Rust CLI against a bounded loopback HTTP fixture.
//! These are transport/command contracts, not real-model or device acceptance.
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::Command,
};

const TOKEN: &str = "SYNTHETIC_PRIVATE_CLI_TOKEN_DO_NOT_PRINT";
struct Fixture {
    root: tempfile::TempDir,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    task: tokio::task::JoinHandle<Result<()>>,
}
impl Fixture {
    async fn start(responses: Vec<(u16, Value)>) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("runtime"))?;
        std::fs::write(
            root.path().join("runtime/server.json"),
            serde_json::to_vec(
                &json!({"pid":std::process::id(),"port":port,"token":TOKEN,"implementation":"Rust"}),
            )?,
        )?;
        let requests = Arc::new(Mutex::new(vec![]));
        let captured = requests.clone();
        let task = tokio::spawn(async move {
            for (status, response) in responses {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(8), listener.accept()).await??;
                let mut bytes = Vec::new();
                let (header_end, content_length) = loop {
                    let mut chunk = [0u8; 4096];
                    let n = tokio::time::timeout(Duration::from_secs(4), socket.read(&mut chunk))
                        .await??;
                    ensure!(n != 0, "CLI closed before sending an HTTP request");
                    bytes.extend_from_slice(&chunk[..n]);
                    ensure!(bytes.len() <= 32_768, "unbounded CLI request");
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..end]);
                        ensure!(
                            header.lines().any(|line| line
                                .eq_ignore_ascii_case(&format!("cookie: ai_tutor_local={TOKEN}"))),
                            "CLI omitted local cookie"
                        );
                        let length = header
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(str::trim)
                                    .and_then(|v| v.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        break (end + 4, length);
                    }
                };
                while bytes.len() < header_end + content_length {
                    let mut chunk = [0u8; 4096];
                    let n = tokio::time::timeout(Duration::from_secs(4), socket.read(&mut chunk))
                        .await??;
                    ensure!(n != 0 && bytes.len() + n <= 32_768);
                    bytes.extend_from_slice(&chunk[..n]);
                }
                let route = String::from_utf8_lossy(&bytes[..header_end])
                    .lines()
                    .next()
                    .unwrap()
                    .to_owned();
                let body = serde_json::from_slice(&bytes[header_end..header_end + content_length])
                    .unwrap_or(Value::Null);
                captured.lock().unwrap().push((route, body));
                let body = serde_json::to_vec(&response)?;
                let header = format!(
                    "HTTP/1.1 {status} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    if status == 200 { "OK" } else { "Bad Request" },
                    body.len()
                );
                socket.write_all(header.as_bytes()).await?;
                socket.write_all(&body).await?;
                socket.shutdown().await?;
            }
            Ok(())
        });
        Ok(Self {
            root,
            requests,
            task,
        })
    }
}
fn state() -> Value {
    json!({"implementation":"Rust native + Rust WebAssembly","session":{"session_id":"synthetic-session","activity_id":"synthetic-activity","learner_id":"synthetic-learner","status":"idle","turns":[{"content":"PRIVATE_DIALOGUE_CANARY"}],"recalled_memories":[{"content":"PRIVATE_MEMORY_CANARY"}]},"kimi":{"available":true,"running":0},"camera":{"state":"closed"},"audio":{"recording":false},"metrics":{"requests":0},"learners":[]})
}
async fn cli(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tutorctl"));
    command
        .arg("--root")
        .arg(root)
        .args(args)
        .kill_on_drop(true);
    Ok(tokio::time::timeout(Duration::from_secs(10), command.output()).await??)
}

#[tokio::test]
async fn recording_stop_binds_the_observed_session_and_recording_generation() -> Result<()> {
    let mut snapshot = state();
    snapshot["audio"]["recording"] = json!(true);
    snapshot["audio"]["recording_generation"] = json!(42);
    let fixture = Fixture::start(vec![
        (200, snapshot),
        (200, json!({"text":"模拟转写","requires_confirmation":true})),
    ])
    .await?;
    let output = cli(
        fixture.root.path(),
        &["audio", "record", "stop", "--session", "synthetic-session"],
    )
    .await?;
    ensure!(
        output.status.success(),
        "record stop command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.task.await??;
    let requests = fixture.requests.lock().unwrap();
    ensure!(requests.len() == 2);
    ensure!(requests[1].0.starts_with("POST /api/audio/record/stop "));
    ensure!(requests[1].1 == json!({"session_id":"synthetic-session","recording_generation":42}));
    Ok(())
}

#[tokio::test]
async fn recording_stop_without_generation_never_sends_an_unbound_stop() -> Result<()> {
    let fixture = Fixture::start(vec![(200, state())]).await?;
    let output = cli(
        fixture.root.path(),
        &["audio", "record", "stop", "--session", "synthetic-session"],
    )
    .await?;
    ensure!(!output.status.success());
    fixture.task.await??;
    ensure!(fixture.requests.lock().unwrap().len() == 1);
    Ok(())
}

#[tokio::test]
async fn status_uses_authenticated_loopback_and_omits_private_dialogue_by_default() -> Result<()> {
    let fixture = Fixture::start(vec![(200, state())]).await?;
    let output = cli(fixture.root.path(), &["status"]).await?;
    ensure!(output.status.success(), "status command failed");
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    let _: Value = serde_json::from_str(&stdout)?;
    ensure!(!stdout.contains(TOKEN) && !stderr.contains(TOKEN));
    ensure!(
        !stdout.contains("PRIVATE_DIALOGUE_CANARY") && !stdout.contains("PRIVATE_MEMORY_CANARY")
    );
    fixture.task.await??;
    ensure!(
        fixture.requests.lock().unwrap()[0]
            .0
            .starts_with("GET /api/state ")
    );
    Ok(())
}

#[tokio::test]
async fn report_only_keeps_modules_unspecified_and_treats_text_as_json_data() -> Result<()> {
    let fixture = Fixture::start(vec![
        (200, state()),
        (
            200,
            json!({"event_id":"synthetic-observation","hardware_verified":false}),
        ),
    ])
    .await?;
    let report = "第二次间隙更大；引号\"、换行\n、$(literal)、`literal`与🟢均是报告文本";
    let output = cli(
        fixture.root.path(),
        &[
            "activity",
            "report",
            "--source",
            "operator_confirmed",
            "--report",
            report,
        ],
    )
    .await?;
    ensure!(output.status.success(), "report command failed");
    fixture.task.await??;
    let requests = fixture.requests.lock().unwrap();
    ensure!(requests.len() == 2);
    ensure!(
        requests[1]
            .0
            .starts_with("POST /api/activities/synthetic-activity/events ")
    );
    ensure!(
        requests[1].1["report"] == report && requests[1].1["source_type"] == "operator_confirmed"
    );
    ensure!(
        requests[1].1.get("modules").is_none() || requests[1].1["modules"].is_null(),
        "observation-only request would overwrite inventory"
    );
    Ok(())
}

#[tokio::test]
async fn server_error_cannot_echo_the_local_authentication_token() -> Result<()> {
    let fixture = Fixture::start(vec![(
        400,
        json!({"detail":format!("synthetic failure contains {TOKEN}")}),
    )])
    .await?;
    let output = cli(fixture.root.path(), &["status"]).await?;
    ensure!(!output.status.success());
    ensure!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN));
    ensure!(!String::from_utf8_lossy(&output.stderr).contains(TOKEN));
    fixture.task.await??;
    Ok(())
}

#[tokio::test]
async fn unknown_command_never_silently_launches_the_application() -> Result<()> {
    let root = tempfile::tempdir()?;
    let output = cli(root.path(), &["unsupported-command"]).await?;
    ensure!(!output.status.success());
    ensure!(!root.path().join("runtime/server.json").exists());
    Ok(())
}
