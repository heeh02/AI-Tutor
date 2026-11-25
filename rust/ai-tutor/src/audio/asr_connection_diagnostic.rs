//! Explicit real-reqwest/real-Python-handler test, with no acoustic model.
//! Header observation uses AsrClient::post/send, check_instance and validate;
//! it does not invoke response_observed or replace its production error policy.
use super::voice_model_chain_diagnostic::{hash, protected, write_new};
use super::*;
use crate::local_asr::{AsrMode, LocalAsrPolicy, MODEL_DIRECTORY, OwnedAsrService};
use anyhow::{Result, anyhow, ensure};
use std::{fs, path::PathBuf};
use tokio::sync::watch;

const SERVICE: &[u8] = include_bytes!("../../../../scripts/speech-service/asr_service.py");
const MANAGED: &[u8] = include_bytes!("../../../../scripts/speech-service/managed_asr.py");
const SELF_SOURCE: &[u8] = include_bytes!("asr_connection_diagnostic.rs");
const CLIENT_SOURCE: &[u8] = include_bytes!("../local_asr.rs");
const OWNER_SOURCE: &[u8] = include_bytes!("../local_asr/owned.rs");
const STREAMING_SOURCE: &[u8] = include_bytes!("streaming_asr.rs");
const TEST: &str = "audio::streaming_asr::asr_connection_diagnostic::real_reqwest_owned_python_fake_connection_reuse";
const PLACEHOLDERS: [&str; 4] = [
    "tokens.txt",
    "encoder-epoch-99-avg-1.int8.onnx",
    "decoder-epoch-99-avg-1.onnx",
    "joiner-epoch-99-avg-1.int8.onnx",
];
const FAKE_RECOGNIZER: &str = r#"# Explicit fixture only. Never loads NumPy, sherpa, Torch or model assets.
import json
from pathlib import Path
import sys

class Zipformer:
    provider = "fake-only / independent stream counter / no acoustic model"
    runtime_version = "stdlib-only"

    def __init__(self, model, threads=2, warmup=True):
        model = Path(model)
        assert (model / "FAKE_ONLY").read_text(encoding="utf-8") == "NO_ACOUSTIC_MODEL"
        assert threads == 2 and warmup is False
        names = ("tokens.txt", "encoder-epoch-99-avg-1.int8.onnx",
                 "decoder-epoch-99-avg-1.onnx", "joiner-epoch-99-avg-1.int8.onnx")
        assert all((model / name).stat().st_size == 0 for name in names)
        self.audit = model / "fake-engine-events.jsonl"
        self.next_stream = 0
        native = [name for name in ("numpy", "sherpa_onnx", "torch") if name in sys.modules]
        assert not native
        self.event("init", native_modules=native, placeholder_assets=True)

    def event(self, event, **fields):
        with self.audit.open("a", encoding="utf-8") as output:
            output.write(json.dumps({"event":event, **fields}) + "\n")

    def warm(self):
        self.event("warm")

    def create(self):
        self.next_stream += 1
        stream = {"stream":self.next_stream, "chunks":0, "samples":0}
        self.event("create", **stream)
        return stream

    def text(self, stream):
        return "fake:{stream}:{chunks}:{samples}".format(**stream)

    def chunk(self, stream, pcm):
        assert len(pcm) == 640 and pcm == b"\0" * 640
        stream["chunks"] += 1
        stream["samples"] += len(pcm) // 2
        self.event("chunk", **stream)
        return self.text(stream)

    def finish(self, stream):
        self.event("final", **stream)
        return self.text(stream)

Paraformer = Zipformer  # Import-only fixture alias; probe always uses default Zipformer.
"#;

fn source_binding(root: &Path) -> Result<Value> {
    let mut sources = serde_json::Map::new();
    for (name, compiled) in [
        ("scripts/speech-service/asr_service.py", SERVICE),
        ("scripts/speech-service/managed_asr.py", MANAGED),
        (
            "rust/ai-tutor/src/audio/asr_connection_diagnostic.rs",
            SELF_SOURCE,
        ),
        ("rust/ai-tutor/src/local_asr.rs", CLIENT_SOURCE),
        ("rust/ai-tutor/src/local_asr/owned.rs", OWNER_SOURCE),
        ("rust/ai-tutor/src/audio/streaming_asr.rs", STREAMING_SOURCE),
    ] {
        ensure!(
            fs::read(root.join(name))? == compiled,
            "compiled source changed"
        );
        sources.insert(name.into(), json!(hash(&root.join(name))?));
    }
    let exe = std::env::current_exe()?.canonicalize()?;
    Ok(json!({"source_files_sha256":sources,"executable":exe,"executable_sha256":hash(&exe)?}))
}

fn stage_fixture(out: &Path) -> Result<Value> {
    let scripts = out.join("bin/local-asr");
    let models = out.join(MODEL_DIRECTORY);
    fs::create_dir_all(&scripts)?;
    fs::create_dir_all(&models)?;
    let mut files = vec![];
    for (name, bytes) in [
        ("managed_asr.py", MANAGED),
        ("asr_service.py", SERVICE),
        ("recognizer.py", FAKE_RECOGNIZER.as_bytes()),
    ] {
        let path = scripts.join(name);
        fs::write(&path, bytes)?;
        files.push(json!({"path":format!("bin/local-asr/{name}"),"sha256":hash(&path)?,"bytes":bytes.len()}));
    }
    for name in PLACEHOLDERS {
        let path = models.join(name);
        fs::write(&path, b"")?;
        files.push(json!({"path":format!("{MODEL_DIRECTORY}/{name}"),"bytes":0,"sha256":hash(&path)?,"empty_fake_placeholder":true}));
    }
    let sentinel = models.join("FAKE_ONLY");
    let sentinel_text = "NO_ACOUSTIC_MODEL";
    fs::write(&sentinel, sentinel_text)?;
    files.push(json!({"path":format!("{MODEL_DIRECTORY}/FAKE_ONLY"),
        "sha256":hash(&sentinel)?,"bytes":sentinel_text.len()}));
    Ok(json!({"fake_only":true,"real_model_calls_allowed":0,"files":files}))
}

struct Wire {
    value: Value,
    status: u16,
    connection: u64,
    request: u64,
    close: bool,
}

async fn wire(
    client: &AsrClient,
    request: RequestBuilder,
    label: &str,
    events: &mut Vec<Value>,
    limit: usize,
) -> Result<Wire> {
    ensure!(events.len() < limit, "fixed fake POST budget exceeded");
    let index = events.len();
    events.push(json!({"label":label,"attempted":true}));
    let began = Instant::now();
    let mut response = request.send().await.map_err(|error| {
        events[index]["send_error"] = send_error_metadata(&error);
        anyhow!("fake_request_send_failed")
    })?;
    let status = response.status().as_u16();
    let integer = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
    };
    let connection = integer("x-asr-connection-id");
    let request = integer("x-asr-request-id");
    let policy = response
        .headers()
        .get("connection")
        .and_then(|value| value.to_str().ok())
        .filter(|value| ["close", "keep-alive"].contains(value))
        .map(str::to_owned);
    events[index]["status"] = json!(status);
    events[index]["connection_id"] = json!(connection);
    events[index]["request_id"] = json!(request);
    events[index]["connection_policy"] = json!(policy);
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("fake_body_read_failed"))?
    {
        ensure!(
            bytes.len() + chunk.len() <= MAX_RESPONSE,
            "fake_body_limit_exceeded"
        );
        bytes.extend_from_slice(&chunk);
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| anyhow!("fake_body_json_invalid"))?;
    events[index]["value"] = value.clone();
    events[index]["elapsed_ms"] = json!(began.elapsed().as_secs_f64() * 1000.0);
    if (200..300).contains(&status) {
        client.check_instance(&value)?;
        events[index]["instance_validation_scope"] = json!("protocol_and_instance");
    } else {
        // Production error bodies have error + instance_id, not the success
        // protocol schema. Preserve that distinction for the planned stale409.
        ensure!(
            value["instance_id"] == client.instance_id,
            "fake_error_instance_mismatch"
        );
        events[index]["instance_validation_scope"] = json!("error_instance_id_only");
    }
    ensure!(
        policy.is_some(),
        "connection policy header missing or invalid"
    );
    Ok(Wire {
        value,
        status,
        connection: connection
            .ok_or_else(|| anyhow!("connection integer header missing or invalid"))?,
        request: request.ok_or_else(|| anyhow!("request integer header missing or invalid"))?,
        close: policy.as_deref() == Some("close"),
    })
}

struct Flow {
    sid: String,
    generation: u64,
    revision: u64,
    text: String,
}
impl Flow {
    fn new(generation: u64) -> Self {
        Self {
            sid: uuid::Uuid::new_v4().to_string(),
            generation,
            revision: 0,
            text: String::new(),
        }
    }
    fn validate(&mut self, response: &Wire, sequence: Option<u64>, final_text: bool) -> Result<()> {
        ensure!(response.status == 200, "unexpected fake success status");
        validate(
            &response.value,
            &self.sid,
            self.generation,
            sequence,
            final_text,
            &mut self.revision,
            &mut self.text,
        )
        .map_err(anyhow::Error::msg)
    }
    async fn start(
        &mut self,
        client: &AsrClient,
        events: &mut Vec<Value>,
        limit: usize,
    ) -> Result<Wire> {
        let response = wire(client, client.post("/v1/asr/start").json(&json!({
            "protocol_version":1,"session_id":self.sid,"recording_generation":self.generation,
            "sample_rate":16000,"channels":1,"encoding":"pcm_s16le","language":"auto","max_duration_ms":1000
        })), "start", events, limit).await?;
        self.validate(&response, None, false)?;
        ensure!(
            self.revision == 0 && self.text.is_empty(),
            "new stream inherited text or revision"
        );
        ensure!(!response.close, "healthy start closed transport");
        Ok(response)
    }
    async fn chunk(
        &mut self,
        client: &AsrClient,
        sequence: u64,
        events: &mut Vec<Value>,
        limit: usize,
    ) -> Result<Wire> {
        let response = wire(
            client,
            client
                .post(&format!("/v1/asr/{}/chunk?sequence={sequence}", self.sid))
                .header("X-Recording-Generation", self.generation)
                .body(vec![0u8; 640]),
            "chunk",
            events,
            limit,
        )
        .await?;
        self.validate(&response, Some(sequence), false)?;
        ensure!(!response.close, "healthy chunk closed transport");
        Ok(response)
    }
    async fn finish(
        &mut self,
        client: &AsrClient,
        sequence: u64,
        events: &mut Vec<Value>,
        limit: usize,
    ) -> Result<Wire> {
        let response = wire(
            client,
            client
                .post(&format!("/v1/asr/{}/final", self.sid))
                .timeout(Duration::from_secs(5))
                .json(&json!({"recording_generation":self.generation,"sequence":sequence})),
            "final",
            events,
            limit,
        )
        .await?;
        self.validate(&response, Some(sequence), true)?;
        ensure!(!response.close, "healthy final closed transport");
        Ok(response)
    }
}

fn same_connection(previous: &Wire, next: &Wire) -> Result<()> {
    ensure!(
        next.connection == previous.connection,
        "connection was not reused"
    );
    ensure!(
        next.request == previous.request + 1,
        "connection request order changed"
    );
    Ok(())
}

async fn completed_case(client: &AsrClient, events: &mut Vec<Value>) -> Result<Value> {
    let mut first = Flow::new(101);
    let mut second = Flow::new(202);
    ensure!(first.sid != second.sid, "fixture UUID collision");
    let first_start = first.start(client, events, 12).await?;
    let chunk = first.chunk(client, 0, events, 12).await?;
    same_connection(&first_start, &chunk)?;
    let next = first.chunk(client, 1, events, 12).await?;
    same_connection(&chunk, &next)?;
    let first_final = first.finish(client, 2, events, 12).await?;
    same_connection(&next, &first_final)?;
    ensure!(first.text == "fake:1:2:640", "first decoder count mismatch");

    let second_start = second.start(client, events, 12).await?;
    same_connection(&first_final, &second_start)?;
    let chunk = second.chunk(client, 0, events, 12).await?;
    same_connection(&second_start, &chunk)?;
    let next = second.chunk(client, 1, events, 12).await?;
    same_connection(&chunk, &next)?;
    let second_final = second.finish(client, 2, events, 12).await?;
    same_connection(&next, &second_final)?;
    ensure!(
        second.text == "fake:2:2:640",
        "second decoder inherited first stream"
    );

    let idle = Instant::now();
    tokio::time::sleep(Duration::from_millis(3200)).await;
    let idle_ms = idle.elapsed().as_secs_f64() * 1000.0;
    let mut third = Flow::new(303);
    let third_start = third.start(client, events, 12).await?;
    ensure!(
        third_start.connection == second_final.connection + 1 && third_start.request == 1,
        "idle close did not produce one fresh accepted connection"
    );
    let stale = wire(
        client,
        client
            .post(&format!("/v1/asr/{}/chunk?sequence=2", second.sid))
            .header("X-Recording-Generation", second.generation)
            .body(vec![0u8; 640]),
        "old_uuid_generation_chunk",
        events,
        12,
    )
    .await?;
    same_connection(&third_start, &stale)?;
    ensure!(
        stale.status == 409 && stale.close,
        "old identity was not rejected and closed"
    );
    let chunk = third.chunk(client, 0, events, 12).await?;
    ensure!(
        chunk.connection == stale.connection + 1 && chunk.request == 1,
        "error close did not produce one fresh accepted connection"
    );
    let third_final = third.finish(client, 1, events, 12).await?;
    same_connection(&chunk, &third_final)?;
    ensure!(
        third.text == "fake:3:1:320",
        "old identity altered current decoder"
    );
    ensure!(
        !client.unavailable(),
        "diagnostic changed managed client availability"
    );
    Ok(
        json!({"healthy_recordings_shared_connection":true,"independent_decoder_streams":true,
        "first_session_id":first.sid,"first_generation":101,"second_session_id":second.sid,"second_generation":202,
        "shared_connection_id":first_start.connection,"idle_wait_ms":idle_ms,
        "after_idle_connection_id":third_start.connection,"after_error_connection_id":third_final.connection,
        "old_identity_rejected":true,"stop_state":"after_healthy_final_keepalive"}),
    )
}

async fn unfinished_case(client: &AsrClient, events: &mut Vec<Value>) -> Result<Value> {
    let mut flow = Flow::new(404);
    let start = flow.start(client, events, 2).await?;
    let chunk = flow.chunk(client, 0, events, 2).await?;
    same_connection(&start, &chunk)?;
    ensure!(flow.text == "fake:1:1:320", "unfinished decoder mismatch");
    Ok(
        json!({"session_id":flow.sid,"generation":404,"connection_id":start.connection,
        "final_sent":false,"stop_state":"unfinished_utterance_idle_keepalive"}),
    )
}

fn read_audit(path: &Path) -> Result<Vec<Value>> {
    let bytes = fs::read(path)?;
    ensure!(bytes.len() <= 65536, "fake audit exceeds limit");
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).map_err(anyhow::Error::from))
        .collect()
}

fn audit_matches(audit: &[Value], unfinished: bool) -> bool {
    let names: Vec<_> = audit
        .iter()
        .filter_map(|event| event["event"].as_str())
        .collect();
    let expected: &[&str] = if unfinished {
        &["init", "warm", "create", "chunk"]
    } else {
        &[
            "init", "warm", "create", "chunk", "chunk", "final", "create", "chunk", "chunk",
            "final", "create", "chunk", "final",
        ]
    };
    let streams: Vec<_> = audit
        .iter()
        .filter(|event| event["event"] == "create")
        .filter_map(|event| event["stream"].as_u64())
        .collect();
    let expected_streams = if unfinished {
        vec![1u64]
    } else {
        vec![1, 2, 3]
    };
    names.as_slice() == expected
        && streams == expected_streams
        && audit.first().is_some_and(|event| {
            event["native_modules"] == json!([]) && event["placeholder_assets"] == true
        })
}

async fn run_case(root: &Path, out: &Path, unfinished: bool) -> Result<Value> {
    fs::create_dir(out)?;
    let fixture = out.join("fixture");
    let python = root
        .join("runtime/speech-lab/.venv/Scripts/python.exe")
        .canonicalize()?;
    let mut report = json!({"passed":false,"fake_only":true,"unfinished":unfinished,
        "real_model_calls":0,"devices":false,"request_retry_added":false,
        "post_budget":if unfinished{2}else{12},"startup_health_get_budget":1,
        "response_observed_called":false,"real_asrclient_reqwest":true,"real_python_handler":true});
    let mut owner = None;
    let mut retained_client = None;
    let mut events = vec![];
    let operation: Result<()> = async {
        report["fixture"] = stage_fixture(&fixture)?;
        write_new(&out.join("setup.json"), &report)?;
        let policy = LocalAsrPolicy {
            protocol_version: 1,
            mode: AsrMode::Managed,
            python_executable: Some(python),
        };
        owner = Some(OwnedAsrService::spawn(&fixture, &policy)?);
        let (_cancel, mut cancelled) = watch::channel(false);
        let mut preparation = vec![];
        let prepared = owner
            .as_mut()
            .unwrap()
            .prepare(&mut cancelled, |event| preparation.push(event))
            .await;
        report["preparation_events"] = json!(preparation);
        retained_client = Some(prepared?);
        let client = retained_client.as_ref().unwrap();
        report["instance_id"] = json!(client.instance_id);
        report["checks"] = if unfinished {
            unfinished_case(client, &mut events).await?
        } else {
            completed_case(client, &mut events).await?
        };
        Ok(())
    }
    .await;
    report["requests"] = json!(events);
    report["operation_passed"] = json!(operation.is_ok());
    report["error"] = operation.err().map_or(Value::Null, |error| {
        json!(error.to_string().chars().take(240).collect::<String>())
    });
    // Retain the actual reqwest pool until stop has finished. Dropping it first
    // would close the idle socket and falsely certify the owner-stop behavior.
    report["client_retained_until_owner_stop"] = json!(retained_client.is_some());
    let stop_began = Instant::now();
    report["owned_stop"] = match owner.as_mut() {
        Some(owner) => owner.stop().await,
        None => Value::Null,
    };
    let stop_ms = stop_began.elapsed().as_secs_f64() * 1000.0;
    report["owner_stop_ms"] = json!(stop_ms);
    drop(retained_client);
    let audit = read_audit(
        &fixture
            .join(MODEL_DIRECTORY)
            .join("fake-engine-events.jsonl"),
    );
    report["fake_engine_audit"] = audit.as_ref().map_or(Value::Null, |rows| json!(rows));
    report["fake_engine_audit_passed"] = json!(
        audit
            .as_ref()
            .is_ok_and(|rows| audit_matches(rows, unfinished))
    );
    report["passed"] = json!(
        report["operation_passed"] == true
            && report["fake_engine_audit_passed"] == true
            && report["owned_stop"]["cleanup_confirmed"] == true
            && report["owned_stop"]["forced"] == false
            && stop_ms < 2500.0
    );
    write_new(&out.join("report.json"), &report)?;
    Ok(report)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Explicit real Rust reqwest + owned Python handler; fake engine only, no model/device"]
async fn real_reqwest_owned_python_fake_connection_reuse() -> Result<()> {
    ensure!(
        std::env::var("AI_TUTOR_ASR_CONNECTION_DIAGNOSTIC").as_deref() == Ok("1"),
        "explicit fake connection diagnostic opt-in required"
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let requested = PathBuf::from(std::env::var("AI_TUTOR_ASR_CONNECTION_OUTPUT")?);
    ensure!(
        requested.is_absolute() && !requested.exists(),
        "fresh absolute output required"
    );
    let parent = requested
        .parent()
        .ok_or_else(|| anyhow!("output parent absent"))?
        .canonicalize()?;
    let name = requested
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("output name invalid"))?;
    ensure!(
        parent.starts_with(root.join("runtime/goal-baselines"))
            && !name.is_empty()
            && name.len() <= 80
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
        "output outside diagnostic boundary"
    );
    let out = parent.join(name);
    let binding = source_binding(&root)?;
    let before = protected(&root)?;
    fs::create_dir(&out)?;
    write_new(
        &out.join("manifest.json"),
        &json!({"test":TEST,"build":binding,
        "scope":"real AsrClient reqwest and Python Handler, explicit fake recognizer, empty placeholder assets",
        "real_model_calls":0,"device_calls":0,"owners":2,"fake_recording_start_ceiling":4,
        "http_post_ceiling":14,"startup_health_get_ceiling":2,"retries":0,
        "idle_wait_ms":3200,"socket_read_timeout_ms":3000,"owner_stop_maximum_ms":2500,
        "original_connect_timeout_ms":500,"response_observed_called":false,
        "protected_before_sha256":before}),
    )?;
    let completed = run_case(&root, &out.join("completed-idle"), false).await;
    let unfinished = run_case(&root, &out.join("unfinished-idle"), true).await;
    let after = protected(&root)?;
    let passed = completed
        .as_ref()
        .is_ok_and(|value| value["passed"] == true)
        && unfinished
            .as_ref()
            .is_ok_and(|value| value["passed"] == true)
        && before == after;
    let summary = |result: Result<Value>| match result {
        Ok(value) => value,
        Err(error) => {
            json!({"passed":false,"report_io_or_setup_error":error.to_string().chars().take(240).collect::<String>()})
        }
    };
    write_new(
        &out.join("report.json"),
        &json!({"test":TEST,"passed":passed,
        "completed_idle":summary(completed),"unfinished_idle":summary(unfinished),
        "protected_before_sha256":before,"protected_after_sha256":after,
        "real_model_or_acoustic_accuracy_claim":false,"original_failures_replaced":false}),
    )?;
    ensure!(
        passed,
        "fake connection reuse diagnostic incomplete; original observations retained"
    );
    Ok(())
}
