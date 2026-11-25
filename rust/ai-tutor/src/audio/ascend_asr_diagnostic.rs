//! Explicit ASCEND file evaluation over the owned production ASR HTTP client.
//! This does not run capture, resampling, endpoint detection, UI or fallback.
use super::voice_model_chain_diagnostic::{hash, protected, stage_asr, write_new};
use super::*;
use crate::local_asr::{AsrMode, LocalAsrPolicy, MODEL_DIRECTORY, OwnedAsrService};
use crate::store::s;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Write,
    path::PathBuf,
};
use tokio::sync::watch;

const METHOD: &str = include_str!("../../../../scripts/validation/ascend_asr_method_v1.json");
const METHOD_PATH: &str = "scripts/validation/ascend_asr_method_v1.json";
const FAILURE_METHOD: &str =
    include_str!("../../../../scripts/validation/ascend_failure_prefix_method_v2.json");
const FAILURE_METHOD_PATH: &str = "scripts/validation/ascend_failure_prefix_method_v2.json";
const FAILURE_PREFIX_ROWS: usize = 363;
const PARAFORMER_METHOD_PATH: &str = "scripts/validation/paraformer_owned_method_v1.json";
const PARAFORMER_METHOD: &str =
    include_str!("../../../../scripts/validation/paraformer_owned_method_v1.json");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Profile {
    Zipformer,
    FailurePrefix,
    Paraformer,
}
impl Profile {
    fn name(self) -> &'static str {
        match self {
            Self::Zipformer => "accuracy_realtime",
            Self::FailurePrefix => "failure_prefix",
            Self::Paraformer => "paraformer",
        }
    }
    fn method(self) -> &'static str {
        match self {
            Self::Zipformer => METHOD,
            Self::FailurePrefix => FAILURE_METHOD,
            Self::Paraformer => PARAFORMER_METHOD,
        }
    }
    fn model_directory(self) -> &'static str {
        if self == Self::Paraformer {
            OwnedAsrService::PARAFORMER_DIAGNOSTIC_MODEL_DIRECTORY
        } else {
            MODEL_DIRECTORY
        }
    }
    fn backend(self) -> &'static str {
        if self == Self::Paraformer {
            "paraformer"
        } else {
            "zipformer"
        }
    }
    fn right_context_samples(self) -> usize {
        if self == Self::Paraformer {
            10560
        } else {
            9600
        }
    }
}

fn profile_for_run(manifest: &Value, mode: &str) -> Result<Profile> {
    let profile = match s(manifest, "evaluation_profile") {
        "accuracy_realtime" => Profile::Zipformer,
        "failure_prefix" => Profile::FailurePrefix,
        "paraformer" => Profile::Paraformer,
        _ => return Err(anyhow!("unknown explicit evaluation profile")),
    };
    ensure!(
        manifest["method"] == serde_json::from_str::<Value>(profile.method())?,
        "prepared profile method changed"
    );
    ensure!(
        profile != Profile::Paraformer
            || (manifest["backend"] == profile.backend()
                && manifest["model_directory"] == profile.model_directory()
                && manifest["model_final_right_context_samples"]
                    == profile.right_context_samples()),
        "Paraformer manifest backend/context mismatch"
    );
    ensure!(
        match profile {
            Profile::FailurePrefix => mode == "failure_prefix",
            _ => ["accuracy", "realtime"].contains(&mode),
        },
        "profile phase mismatch"
    );
    Ok(profile)
}

fn stage_paraformer(root: &Path, out: &Path) -> Result<(PathBuf, Value)> {
    let directory = Profile::Paraformer.model_directory();
    let method: Value = serde_json::from_str(PARAFORMER_METHOD)?;
    let source_manifest = root.join(directory).join("download-manifest.json");
    ensure!(
        hash(&source_manifest)? == method["model"]["asset_manifest"]["sha256"],
        "Paraformer download manifest changed"
    );
    let downloaded = read(&source_manifest)?;
    let assets = method["model"]["assets"]
        .as_array()
        .ok_or_else(|| anyhow!("Paraformer assets absent"))?;
    ensure!(assets.len() == 3, "Paraformer asset count changed");
    let fixture = out.join("fixture");
    fs::create_dir_all(fixture.join("bin/local-asr"))?;
    fs::create_dir_all(fixture.join(directory))?;
    let mut files = vec![];
    for name in ["managed_asr.py", "asr_service.py", "recognizer.py"] {
        let source = root.join("scripts/speech-service").join(name);
        let destination = fixture.join("bin/local-asr").join(name);
        fs::copy(&source, &destination)?;
        let expected = hash(&source)?;
        ensure!(
            hash(&destination)? == expected,
            "Paraformer staged service changed"
        );
        files.push(json!({"kind":"service","name":name,"sha256":expected}));
    }
    for asset in assets {
        let name = s(asset, "file");
        ensure!(
            ["encoder.int8.onnx", "decoder.int8.onnx", "tokens.txt"].contains(&name),
            "unexpected Paraformer asset"
        );
        ensure!(
            downloaded["files"]
                .as_array()
                .is_some_and(|files| files.contains(asset)),
            "Paraformer declared asset differs from download manifest"
        );
        let source = root.join(directory).join(name);
        let expected = s(asset, "sha256");
        ensure!(
            hash(&source)? == expected
                && Some(fs::metadata(&source)?.len()) == asset["size"].as_u64(),
            "fixed Paraformer asset changed"
        );
        let destination = fixture.join(directory).join(name);
        fs::hard_link(&source, &destination)?;
        ensure!(
            hash(&destination)? == expected,
            "Paraformer staged asset changed"
        );
        files.push(json!({"kind":"model","name":name,"sha256":expected,"bytes":asset["size"]}));
    }
    Ok((fixture, json!(files)))
}
const SOURCE_PATH: &str = "rust/ai-tutor/src/audio/ascend_asr_diagnostic.rs";
const SOURCE: &[u8] = include_bytes!("ascend_asr_diagnostic.rs");
const TEST: &str = "audio::streaming_asr::ascend_asr_diagnostic::real_ascend_zipformer";

#[derive(Clone, Deserialize)]
struct Row {
    ordinal: usize,
    id: String,
    pcm_file: String,
    pcm_sha256: String,
    pcm_bytes: usize,
    sample_count: usize,
    sample_rate: u32,
}

fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn ms(at: Instant) -> f64 {
    at.elapsed().as_secs_f64() * 1000.0
}

fn inputs(path: &Path, expected: &str) -> Result<(Value, Vec<Row>, Vec<String>)> {
    ensure!(
        hash(&path.join("input-manifest.json"))? == expected,
        "input manifest changed"
    );
    let manifest = read(&path.join("input-manifest.json"))?;
    ensure!(
        manifest["dataset_sha256"]
            == "a4c81d2b5ed6124f052089a695972808c16e0ce0c365ec9773c5d1a8fcf043a7"
            && manifest["rows"] == 1315
            && manifest["reference_complete_rows"] == 1279
            && manifest["reference_uncertain_rows"] == 36,
        "input population changed"
    );
    ensure!(
        hash(&path.join("rows.json"))? == manifest["rows_sha256"]
            && hash(&path.join("realtime-subset.json"))? == manifest["subset_sha256"],
        "prepared input files changed"
    );
    for (name, expected) in manifest["copied_sha256"]
        .as_object()
        .ok_or_else(|| anyhow!("copied source manifest absent"))?
    {
        let file = path.join(name).canonicalize()?;
        ensure!(
            file.starts_with(path) && expected.as_str() == Some(hash(&file)?.as_str()),
            "prepared method or normalizer changed"
        );
    }
    ensure!(
        read(&path.join("ascend_asr_method_v1.json"))? == serde_json::from_str::<Value>(METHOD)?,
        "compiled method differs from prepared method"
    );
    // Unknown fields (including reference text) are intentionally not loaded
    // into the inference row type or sent to the recognition service.
    let rows: Vec<Row> = serde_json::from_slice(&fs::read(path.join("rows.json"))?)?;
    ensure!(rows.len() == 1315, "input row count changed");
    let mut seen = HashSet::new();
    for (ordinal, row) in rows.iter().enumerate() {
        ensure!(
            row.ordinal == ordinal && seen.insert(row.id.clone()),
            "input identity/order changed"
        );
        ensure!(
            row.sample_rate == 16000
                && (1600..=320000).contains(&row.sample_count)
                && row.pcm_bytes == row.sample_count * 2,
            "PCM format or duration changed"
        );
        let pcm = path.join(&row.pcm_file).canonicalize()?;
        ensure!(
            pcm.starts_with(path)
                && fs::metadata(&pcm)?.len() == row.pcm_bytes as u64
                && hash(&pcm)? == row.pcm_sha256,
            "PCM bytes changed"
        );
    }
    let subset: Vec<String> =
        serde_json::from_value(read(&path.join("realtime-subset.json"))?["ids"].clone())?;
    ensure!(
        subset.len() == 12
            && subset.iter().collect::<HashSet<_>>().len() == 12
            && subset.iter().all(|id| seen.contains(id)),
        "realtime subset changed"
    );
    Ok((manifest, rows, subset))
}

fn build_binding(root: &Path, file: &Path) -> Result<Value> {
    let file = file.canonicalize()?;
    ensure!(
        file.starts_with(root.join("runtime/goal-baselines")),
        "build manifest outside baseline"
    );
    let value = read(&file)?;
    let sources = value["source_files_sha256"]
        .as_object()
        .ok_or_else(|| anyhow!("build sources absent"))?;
    for required in [
        SOURCE_PATH,
        METHOD_PATH,
        FAILURE_METHOD_PATH,
        PARAFORMER_METHOD_PATH,
        "rust/ai-tutor/src/audio/streaming_asr.rs",
        "rust/ai-tutor/src/audio/voice_model_chain_diagnostic.rs",
        "rust/ai-tutor/src/local_asr.rs",
        "rust/ai-tutor/src/local_asr/owned.rs",
        "scripts/speech-service/managed_asr.py",
        "scripts/speech-service/asr_service.py",
        "scripts/speech-service/recognizer.py",
        "scripts/validation/ascend_prepare.py",
        "scripts/validation/ascend_score.py",
        "scripts/validation/ascend_normalization_v1.json",
    ] {
        ensure!(
            sources.contains_key(required),
            "build source coverage incomplete"
        );
    }
    for (name, expected) in sources {
        let current = root.join(name).canonicalize()?;
        ensure!(
            current.starts_with(root) && expected.as_str() == Some(hash(&current)?.as_str()),
            "build source changed"
        );
    }
    ensure!(
        fs::read(root.join(SOURCE_PATH))? == SOURCE
            && fs::read_to_string(root.join(METHOD_PATH))? == METHOD
            && fs::read_to_string(root.join(FAILURE_METHOD_PATH))? == FAILURE_METHOD
            && fs::read_to_string(root.join(PARAFORMER_METHOD_PATH))? == PARAFORMER_METHOD,
        "compiled diagnostic or method changed"
    );
    let exe = std::env::current_exe()?.canonicalize()?;
    let name = exe
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("executable filename absent"))?;
    let item = value["executables"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["name"] == name)
        .ok_or_else(|| anyhow!("test executable not archived"))?;
    ensure!(
        item["sha256"] == hash(&exe)? && exe == file.parent().unwrap().join(name).canonicalize()?,
        "exact archived executable required"
    );
    Ok(
        json!({"manifest":file,"manifest_sha256":hash(&file)?,"executable":exe,
        "executable_sha256":item["sha256"],"source_files_verified":sources.len()}),
    )
}

fn prepare(
    root: &Path,
    data: &Path,
    input_sha: &str,
    out: &Path,
    build: &Path,
    profile: Profile,
) -> Result<()> {
    ensure!(!out.exists(), "output already exists");
    let (input, _, subset) = inputs(data, input_sha)?;
    let binding = build_binding(root, build)?;
    fs::create_dir(out)?;
    let (_, assets) = if profile == Profile::Paraformer {
        stage_paraformer(root, out)?
    } else {
        stage_asr(root, out)?
    };
    let failure_prefix = profile == Profile::FailurePrefix;
    let python = root
        .join("runtime/speech-lab/.venv/Scripts/python.exe")
        .canonicalize()?;
    let mut scripts = BTreeMap::new();
    for name in ["managed_asr.py", "asr_service.py", "recognizer.py"] {
        scripts.insert(name, hash(&out.join("fixture/bin/local-asr").join(name))?);
    }
    write_new(
        &out.join("manifest.json"),
        &json!({"status":"prepared_without_models_or_devices","test":TEST,
        "input_directory":data,"input_manifest_sha256":input_sha,"input_manifest":input,
        "build":binding,"method":serde_json::from_str::<Value>(profile.method())?,"assets":assets,
        "evaluation_profile":profile.name(),"backend":profile.backend(),
        "model_directory":profile.model_directory(),"model_final_right_context_samples":profile.right_context_samples(),
        "service_scripts_sha256":scripts,"model_python":python,"model_python_sha256":hash(&python)?,
        "realtime_ids":subset,"service_instances_allowed":if failure_prefix {1}else{2},
        "corpus_utterance_ceiling":if failure_prefix {FAILURE_PREFIX_ROWS}else{1327},
        "source_pcm_transform":"none","services_started":false,"devices_requested":false}),
    )?;
    println!(
        "{}",
        json!({"prepared":true,"manifest_sha256":hash(&out.join("manifest.json"))?})
    );
    Ok(())
}

fn result_stub(row: &Row, mode: &str) -> Value {
    json!({"ordinal":row.ordinal,"id":row.id,"mode":mode,"attempted":false,"status":"not_run",
        "hypothesis":"","final_received":false,"error_code":null,"partial_at_failure":null,
        "sample_count":row.sample_count,"sample_rate":16000,"pcm_sha256":row.pcm_sha256,
        "source_pcm_bytes":row.pcm_bytes,"language_hint":"auto","automatic_retries":0,
        "capture_ui_endpoint_tested":false,"wire_chunk_samples":320})
}

async fn stop_requested(out: &Path) {
    loop {
        if out.join("stop-request.json").exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[derive(Default)]
struct TransportSummary {
    validated_responses: usize,
    complete_responses: usize,
    missing_responses: usize,
    invalid_responses: usize,
    missing_fields: usize,
    invalid_fields: usize,
    first: Option<ObservedTransport>,
    last: Option<ObservedTransport>,
    previous_connection: Option<u64>,
    last_request_by_connection: BTreeMap<u64, u64>,
    adjacent_connection_switches: usize,
    same_connection_request_nonincreasing: usize,
    close_responses: usize,
    keep_alive_responses: usize,
}
impl TransportSummary {
    // Called only after the existing body/identity/revision validator succeeds.
    fn observe(&mut self, transport: ObservedTransport) {
        self.validated_responses += 1;
        self.missing_fields += usize::from(transport.missing_header_fields);
        self.invalid_fields += usize::from(transport.invalid_header_fields);
        if transport.missing_header_fields > 0 {
            self.missing_responses += 1;
        }
        if transport.invalid_header_fields > 0 {
            self.invalid_responses += 1;
        }
        let (Some(connection), Some(request), Some(policy)) = (
            transport.connection_id,
            transport.request_id,
            transport.connection_policy,
        ) else {
            // Do not bridge an unknown response as a certified connection transition.
            self.previous_connection = None;
            return;
        };
        self.complete_responses += 1;
        self.first.get_or_insert(transport);
        self.last = Some(transport);
        if self
            .previous_connection
            .is_some_and(|previous| previous != connection)
        {
            self.adjacent_connection_switches += 1;
        }
        if self
            .last_request_by_connection
            .insert(connection, request)
            .is_some_and(|previous| request <= previous)
        {
            self.same_connection_request_nonincreasing += 1;
        }
        self.previous_connection = Some(connection);
        match policy {
            ConnectionPolicy::Close => self.close_responses += 1,
            ConnectionPolicy::KeepAlive => self.keep_alive_responses += 1,
        }
    }
    fn value(&self) -> Value {
        json!({"scope":"this row; validated start/chunk/final responses only; complete whitelisted headers",
            "validated_response_count":self.validated_responses,"complete_transport_response_count":self.complete_responses,
            "incomplete_transport_response_count":self.validated_responses-self.complete_responses,
            "responses_with_missing_headers":self.missing_responses,"responses_with_invalid_headers":self.invalid_responses,
            "missing_header_field_count":self.missing_fields,"invalid_header_field_count":self.invalid_fields,
            "all_validated_responses_have_complete_transport":self.validated_responses>0 && self.validated_responses==self.complete_responses,
            "first_connection_id":self.first.and_then(|value|value.connection_id),"first_request_id":self.first.and_then(|value|value.request_id),
            "last_connection_id":self.last.and_then(|value|value.connection_id),"last_request_id":self.last.and_then(|value|value.request_id),
            "distinct_connection_count":self.last_request_by_connection.len(),"adjacent_connection_switches":self.adjacent_connection_switches,
            "same_connection_request_nonincreasing":self.same_connection_request_nonincreasing,
            "close_response_count":self.close_responses,"keep_alive_response_count":self.keep_alive_responses,
            "header_scope":"accepted connection and parsed request identifiers, not OS connection attempts; no retry/loss inference"})
    }
}

async fn utterance(
    client: Arc<AsrClient>,
    row: &Row,
    pcm: Vec<u8>,
    mode: &str,
    out: &Path,
    phase_deadline: tokio::time::Instant,
) -> Value {
    let began = Instant::now();
    let sid = uuid::Uuid::new_v4().to_string();
    let token = row.ordinal as u64 + 1;
    let control = Control {
        cancelled: AtomicBool::new(false),
        failure: Mutex::new(None),
        generation: Arc::new(AtomicU64::new(token)),
        token,
        info: Arc::new(Mutex::new(json!({}))),
    };
    let connection = Connection::Managed(client.clone());
    let path = format!("/v1/asr/{sid}");
    let mut result = result_stub(row, mode);
    result["session_id"] = json!(sid);
    result["recording_generation"] = json!(token);
    result["instance_id"] = json!(client.instance_id);
    let mut stage = "start";
    let mut failure_origin = "not_requested";
    let mut failure_sequence = None::<u64>;
    let mut failure_bytes = None::<usize>;
    let mut send_failure = None;
    let mut transport_summary = TransportSummary::default();
    let mut revision = 0;
    let mut text = String::new();
    let mut first_pcm = None;
    let mut last_pcm = None;
    let mut first_partial = None;
    let mut chunks = 0usize;
    let mut sent_bytes = 0usize;
    let mut acknowledged_bytes = 0usize;
    let mut pacing = Vec::<f64>::new();
    let mut observations = Vec::<Value>::new();
    let operation: std::result::Result<(), String> = tokio::select! {biased;
        _=stop_requested(out)=>Err("operator_stop".into()),
        value=tokio::time::timeout_at(phase_deadline.min(tokio::time::Instant::now()+Duration::from_secs(60)),async {
            result["attempted"]=json!(true);failure_origin="response";
            let value=response_observed(connection.post("/v1/asr/start").json(&json!({"protocol_version":1,"session_id":sid,
                "recording_generation":token,"sample_rate":16000,"channels":1,"encoding":"pcm_s16le",
                "language":"auto","max_duration_ms":20000})),&control,Some(&client),Some(&mut send_failure)).await?;
            failure_origin="validate";
            connection.validate(&value.value,&sid,token,None,false,&mut revision,&mut text,&control)?;
            transport_summary.observe(value.transport);
            failure_origin="between_requests";
            result["start_response_ms"]=json!(ms(began));
            let media=tokio::time::Instant::now();result["media_origin_ms"]=json!(ms(began));
            for (sequence,bytes) in pcm.chunks(CHUNK_SAMPLES*2).enumerate() {
                stage="chunk";
                let end_samples=((sequence+1)*CHUNK_SAMPLES).min(row.sample_count);
                let expected=Duration::from_secs_f64(end_samples as f64/16000.0);
                if mode=="realtime" {tokio::time::sleep_until(media+expected).await;}
                let sent=ms(began);first_pcm.get_or_insert(sent);last_pcm=Some(sent);
                sent_bytes+=bytes.len();
                let late=media.elapsed().as_secs_f64()*1000.0-expected.as_secs_f64()*1000.0;
                if mode=="realtime" {pacing.push(late);}
                failure_origin="response";failure_sequence=Some(sequence as u64);failure_bytes=Some(bytes.len());
                let value=response_observed(connection.post(&format!("{path}/chunk?sequence={sequence}"))
                    .header("X-Recording-Generation",token).header("content-type","application/octet-stream")
                    .body(bytes.to_vec()),&control,Some(&client),Some(&mut send_failure)).await?;
                failure_origin="validate";
                connection.validate(&value.value,&sid,token,Some(sequence as u64),false,&mut revision,&mut text,&control)?;
            transport_summary.observe(value.transport);
                failure_origin="between_requests";failure_sequence=None;failure_bytes=None;
                let received=ms(began);chunks+=1;acknowledged_bytes+=bytes.len();
                if !text.trim().is_empty() {first_partial.get_or_insert(received);}
                if mode=="realtime" {observations.push(json!({"sequence":sequence,"end_sample":end_samples,
                    "expected_available_ms":expected.as_secs_f64()*1000.0,"send_ms":sent,
                    "response_ms":received,"pacing_late_ms":late,"revision":revision}));}
            }
            stage="final";let final_started=ms(began);result["eof_finalize_request_ms"]=json!(final_started);
            failure_origin="response";failure_sequence=Some(chunks as u64);failure_bytes=None;
            let value=response_observed(connection.post(&format!("{path}/final")).timeout(Duration::from_secs(5))
                .json(&json!({"recording_generation":token,"sequence":chunks})),&control,Some(&client),Some(&mut send_failure)).await?;
            failure_origin="validate";
            connection.validate(&value.value,&sid,token,Some(chunks as u64),true,&mut revision,&mut text,&control)?;
            transport_summary.observe(value.transport);
            let received=ms(began);result["final_received_ms"]=json!(received);
            result["eof_request_to_final_ms"]=json!(received-final_started);
            result["last_pcm_request_to_final_ms"]=json!(last_pcm.map(|v|received-v));
            result["hypothesis"]=json!(text);result["final_received"]=json!(true);
            result["status"]=json!(if text.trim().is_empty(){"ok_empty"}else{"ok"});
            Ok(())
        })=>value.unwrap_or_else(|_|Err("utterance_or_phase_deadline".into()))
    };
    if let Err(error) = operation {
        // response/validate already return finite local diagnostic strings,
        // never raw reqwest errors, headers, tokens or response bodies.
        result["failure_detail"] = json!(error.chars().take(240).collect::<String>());
        result["failure_origin"] = json!(if error == "operator_stop" {
            "operator_stop"
        } else if error == "utterance_or_phase_deadline" {
            "deadline"
        } else {
            failure_origin
        });
        result["failure_sequence"] = json!(failure_sequence);
        result["failure_payload_bytes"] = json!(failure_bytes);
        result["failure_last_valid_revision"] = json!(revision);
        result["client_unavailable_after_error"] = json!(client.unavailable());
        result["request_send_error"] = json!(send_failure);
        result["status"] = json!(if result["attempted"] == true {
            "failed"
        } else {
            "not_run"
        });
        result["error_code"] = json!(if error == "operator_stop" {
            "operator_stop"
        } else if error == "utterance_or_phase_deadline" {
            "utterance_or_phase_deadline"
        } else {
            match stage {
                "start" => "start_response_failed",
                "chunk" => "chunk_response_failed",
                _ => "final_response_failed",
            }
        });
        result["partial_at_failure"] = json!(text);
        result["hypothesis"] = json!("");
        // Independent of the cancelled inference future; never retries audio.
        if result["attempted"] == true {
            let cancel = client
                .post(&format!("{path}/cancel"))
                .timeout(Duration::from_millis(300))
                .json(&json!({"recording_generation":token}))
                .send()
                .await;
            result["cancel_request_attempted"] = json!(true);
            result["cancel_http_success"] =
                json!(cancel.as_ref().is_ok_and(|r| r.status().is_success()));
            result["cancel_receipt_validated"] = json!(false);
            result["cancel_transport_observation"] = json!(
                cancel
                    .as_ref()
                    .ok()
                    .map(|response| observed_transport(response.headers()))
            );
            result["cancel_transport_validation_scope"] =
                json!("headers_only; cancel body and identity unvalidated");
        }
        // Owner.stop below is the authoritative lifecycle proof; this HTTP
        // status is not a validated cancel or service-exit acknowledgement.
    }
    result["transport_summary"] = transport_summary.value();
    result["first_pcm_request_ms"] = json!(first_pcm);
    result["last_pcm_request_ms"] = json!(last_pcm);
    result["first_nonempty_partial_ms"] = json!(first_partial);
    result["first_partial_from_first_pcm_ms"] =
        json!(first_partial.zip(first_pcm).map(|(a, b)| a - b));
    result["chunks_acknowledged"] = json!(chunks);
    result["final_revision"] = json!(revision);
    result["elapsed_ms"] = json!(ms(began));
    result["media_duration_ms"] = json!(row.sample_count as f64 / 16.0);
    result["wall_to_media_ratio"] = json!(ms(began) / (row.sample_count as f64 / 16.0));
    result["source_pcm_transform"] = json!("none");
    result["pcm_bytes_send_attempted"] = json!(sent_bytes);
    result["pcm_bytes_acknowledged"] = json!(acknowledged_bytes);
    result["all_pcm_acknowledged"] = json!(acknowledged_bytes == row.pcm_bytes);
    if mode == "realtime" {
        result["chunk_observations"] = json!(observations);
        pacing.sort_by(f64::total_cmp);
        result["maximum_pacing_late_ms"] = json!(pacing.last());
        result["pacing_samples"] = json!(pacing.len());
    }
    result
}

fn append(file: &mut fs::File, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *file, value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

async fn run_phase(root: &Path, out: &Path, expected: &str, mode: &str) -> Result<()> {
    ensure!(
        ["accuracy", "realtime", "failure_prefix"].contains(&mode),
        "unknown phase"
    );
    ensure!(
        hash(&out.join("manifest.json"))? == expected,
        "run manifest changed"
    );
    let manifest = read(&out.join("manifest.json"))?;
    let profile = profile_for_run(&manifest, mode)?;
    let data = PathBuf::from(s(&manifest, "input_directory")).canonicalize()?;
    let (_, rows, subset) = inputs(&data, s(&manifest, "input_manifest_sha256"))?;
    ensure!(
        manifest["build"]
            == build_binding(root, &PathBuf::from(s(&manifest["build"], "manifest")))?,
        "run build binding changed"
    );
    for (name, expected) in manifest["service_scripts_sha256"]
        .as_object()
        .ok_or_else(|| anyhow!("service scripts absent"))?
    {
        ensure!(
            expected.as_str()
                == Some(hash(&out.join("fixture/bin/local-asr").join(name))?.as_str()),
            "staged service script changed"
        );
    }
    for asset in manifest["assets"]
        .as_array()
        .ok_or_else(|| anyhow!("model asset manifest absent"))?
    {
        if asset["kind"] == "model" {
            ensure!(
                hash(
                    &out.join("fixture")
                        .join(profile.model_directory())
                        .join(s(asset, "name"))
                )? == asset["sha256"],
                "staged model asset changed"
            );
        }
    }
    let python = PathBuf::from(s(&manifest, "model_python"));
    ensure!(
        hash(&python)? == manifest["model_python_sha256"],
        "model interpreter changed"
    );
    let chosen: Vec<_> = if mode == "accuracy" {
        rows.iter().collect()
    } else if mode == "failure_prefix" {
        rows.iter().take(FAILURE_PREFIX_ROWS).collect()
    } else {
        subset
            .iter()
            .map(|id| rows.iter().find(|row| &row.id == id).unwrap())
            .collect()
    };
    let phase_out = out.join(mode);
    fs::create_dir(&phase_out)?;
    write_new(
        &phase_out.join("run-start.json"),
        &json!({"manifest_sha256":expected,"mode":mode,
        "declared_rows":chosen.len(),"row_ids":chosen.iter().map(|r|&r.id).collect::<Vec<_>>(),
        "new_owned_service":true,"automatic_retries":0,"evaluation_profile":profile.name(),
        "backend":profile.backend(),"model_final_right_context_samples":profile.right_context_samples()}),
    )?;
    let before = protected(root)?;
    let began = Instant::now();
    let mut report = json!({"test":TEST,"mode":mode,"passed":false,"declared_rows":chosen.len(),
        "completed_rows":0,"attempted_rows":0,"failed_rows":0,"not_run_rows":0,"empty_hypothesis_rows":0,
        "source_pcm_transform":"none","warmup_scope":"one fresh owned process, existing synthetic silence warmup; OS cache not reset",
        "service_scope":"production client/response validation and owned service; no CaptureStream/capture/VAD/UI",
        "protected_before_sha256":before,"preparation_events":[],"devices_requested":false,"fallback_used":false,
        "automatic_service_restart":false,"model_services_started":0,
        "report_scope":if profile==Profile::FailurePrefix {"diagnostic_prefix_only"}else if profile==Profile::Paraformer {"independent_paraformer_candidate"}else{"original_declared_phase"},
        "original_accuracy_completion_credit":false,"evaluation_profile":profile.name(),
        "backend":profile.backend(),"model_final_right_context_samples":profile.right_context_samples()});
    let mut owner = None;
    let mut completed = 0usize;
    let mut attempted_count = 0usize;
    let mut successful_count = 0usize;
    let mut abort_code = None;
    let mut output_complete = true;
    let mut results = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(phase_out.join("results.jsonl"))?;
    let operation:Result<()>=async {
        let policy=LocalAsrPolicy{protocol_version:1,mode:AsrMode::Managed,python_executable:Some(python)};
        owner=Some(if profile==Profile::Paraformer {
            OwnedAsrService::spawn_paraformer_diagnostic(&out.join("fixture"),&policy)?
        } else {OwnedAsrService::spawn(&out.join("fixture"),&policy)?});
        report["model_services_started"]=json!(1);
        let (cancel,mut cancelled)=watch::channel(false);
        let client=tokio::select! {biased;
            _=stop_requested(&phase_out)=>{let _=cancel.send(true);return Err(anyhow!("operator_stop_during_preparation"));},
            value=owner.as_mut().unwrap().prepare(&mut cancelled,|value| {
                report["preparation_events"].as_array_mut().unwrap().push(json!({"observed_ms":ms(began),"event":value}));
            })=>value?,
        };
        client.check_warm().await?;report["instance_id"]=json!(client.instance_id);
        report["ready_and_warm_ms"]=json!(ms(began));
        let phase_deadline=tokio::time::Instant::now()+Duration::from_secs(if mode=="accuracy"{7200}else{600});
        for row in &chosen {
            if phase_out.join("stop-request.json").exists() || tokio::time::Instant::now()>=phase_deadline {
                abort_code=Some("operator_stop_or_phase_deadline");break;
            }
            let pcm=fs::read(data.join(&row.pcm_file))?;
            ensure!(pcm.len()==row.pcm_bytes && format!("{:x}",Sha256::digest(&pcm))==row.pcm_sha256,"prepared_pcm_changed");
            let value=utterance(client.clone(),row,pcm,mode,&phase_out,phase_deadline).await;
            if value["attempted"]==true {attempted_count+=1;}
            if value["status"]=="ok" || value["status"]=="ok_empty" {successful_count+=1;}
            report["attempted_rows"]=json!(attempted_count);report["completed_rows"]=json!(successful_count);
            completed+=1;
            if let Err(error)=append(&mut results,&value) {
                output_complete=false;
                report["unpersisted_last_result"]=value;
                return Err(error);
            }
            if value["status"]=="ok_empty" {report["empty_hypothesis_rows"]=json!(report["empty_hypothesis_rows"].as_u64().unwrap()+1);}
            if value["status"]=="failed" {report["failed_rows"]=json!(1);abort_code=Some("utterance_failed_phase_stopped_without_retry");break;}
            if value["status"]=="not_run" {abort_code=Some("stopped_before_utterance_start");break;}
            fs::write(phase_out.join("progress.json"),serde_json::to_vec_pretty(&json!({
                "mode":mode,"recorded_rows":completed,"attempted_rows":attempted_count,"declared_rows":chosen.len(),"elapsed_ms":ms(began)}))?)?;
        }
        Ok(())
    }.await;
    if let Err(error) = &operation {
        abort_code = Some(match error.to_string().as_str() {
            "prepared_pcm_changed" => "prepared_pcm_changed_after_preflight",
            "operator_stop_during_preparation" => "operator_stop_during_preparation",
            _ => "owned_preparation_or_input_io_failed",
        });
    }
    // Stop the exact owner before any additional fallible output I/O. A disk
    // error must not skip the explicit model-release/wait/Job receipts.
    report["owned_stop"] = match owner.as_mut() {
        Some(owner) => owner.stop().await,
        None => Value::Null,
    };
    // A partial JSONL write cannot be repaired by appending another row. Keep
    // the original bytes, retain the attempted result above, and stop writing.
    for row in chosen.iter().skip(completed).take(if output_complete {
        chosen.len() - completed
    } else {
        0
    }) {
        let mut value = result_stub(row, mode);
        value["error_code"] = json!(abort_code.unwrap_or("not_run"));
        if append(&mut results, &value).is_err() {
            output_complete = false;
            break;
        }
    }
    report["not_run_rows"] = json!(chosen.len() - attempted_count);
    report["abort_code"] = json!(abort_code);
    report["operation_completed"] = json!(operation.is_ok() && abort_code.is_none());
    report["output_completion_confirmed"] = json!(output_complete);
    let after = protected(root).unwrap_or(Value::Null);
    report["protected_files_unchanged"] =
        json!(!after.is_null() && after == report["protected_before_sha256"]);
    report["protected_after_sha256"] = after;
    report["total_ms"] = json!(ms(began));
    report["results_sha256"] =
        hash(&phase_out.join("results.jsonl")).map_or(Value::Null, |value| json!(value));
    report["complete_inference_population"] =
        json!(attempted_count == chosen.len() && successful_count == chosen.len());
    report["passed"] = json!(
        report["operation_completed"] == true
            && report["complete_inference_population"] == true
            && output_complete
            && report["owned_stop"]["cleanup_confirmed"] == true
            && report["protected_files_unchanged"] == true
    );
    write_new(&phase_out.join("report.json"), &report)?;
    ensure!(
        report["passed"] == true,
        "ASCEND phase incomplete or cleanup unconfirmed; all rows retained"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Explicit prepare/accuracy/realtime ASCEND Zipformer evaluation; public prerecorded audio, no devices"]
async fn real_ascend_zipformer() -> Result<()> {
    ensure!(
        std::env::var("AI_TUTOR_ASCEND_ASR").as_deref() == Ok("1"),
        "explicit ASCEND opt-in required"
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let input = PathBuf::from(std::env::var("AI_TUTOR_ASCEND_INPUT")?).canonicalize()?;
    ensure!(
        input.starts_with(root.join("runtime/goal-baselines")),
        "input outside baseline"
    );
    let output = PathBuf::from(std::env::var("AI_TUTOR_ASCEND_OUTPUT")?);
    ensure!(output.is_absolute(), "absolute output required");
    let parent = output
        .parent()
        .ok_or_else(|| anyhow!("output parent absent"))?
        .canonicalize()?;
    let name = output
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| anyhow!("output name absent"))?;
    ensure!(
        parent.starts_with(root.join("runtime/goal-baselines"))
            && !name.is_empty()
            && name.len() <= 80
            && name.bytes().all(|v| v.is_ascii_alphanumeric() || v == b'-'),
        "output boundary invalid"
    );
    let out = parent.join(name);
    match std::env::var("AI_TUTOR_ASCEND_MODE")?.as_str() {
        "prepare" => prepare(
            &root,
            &input,
            &std::env::var("AI_TUTOR_ASCEND_INPUT_SHA256")?,
            &out,
            &PathBuf::from(std::env::var("AI_TUTOR_ASCEND_BUILD_MANIFEST")?),
            Profile::Zipformer,
        ),
        "prepare_failure_prefix" => prepare(
            &root,
            &input,
            &std::env::var("AI_TUTOR_ASCEND_INPUT_SHA256")?,
            &out,
            &PathBuf::from(std::env::var("AI_TUTOR_ASCEND_BUILD_MANIFEST")?),
            Profile::FailurePrefix,
        ),
        "prepare_paraformer" => prepare(
            &root,
            &input,
            &std::env::var("AI_TUTOR_ASCEND_INPUT_SHA256")?,
            &out,
            &PathBuf::from(std::env::var("AI_TUTOR_ASCEND_BUILD_MANIFEST")?),
            Profile::Paraformer,
        ),
        mode @ ("accuracy" | "realtime" | "failure_prefix") => {
            run_phase(
                &root,
                &out,
                &std::env::var("AI_TUTOR_ASCEND_RUN_SHA256")?,
                mode,
            )
            .await
        }
        _ => Err(anyhow!(
            "explicit prepare/prepare_paraformer then accuracy/realtime, or prepare_failure_prefix/failure_prefix required"
        )),
    }
}

#[cfg(test)]
#[path = "ascend_asr_diagnostic_tests.rs"]
mod tests;
