//! Shared synthetic tutor diagnostics: provenance, one attempt and no delivery.
use ai_tutor::{
    kimi::{
        KimiProvider,
        acp_stream::SEGMENT_CHANNEL_CAPACITY,
        tutor_segments::{DeliveryBinding, DeliveryLedger, SpeechState, TutorSegment},
    },
    store::{s, uid},
};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};
use tokio::sync::mpsc;

pub(super) const SUPPORT_SOURCE: &[u8] = include_bytes!("tutor_probe_support.rs");
pub(super) const POLICY: &str = "prompts/tutor_core.md";
pub(super) const SCHEMA: &str =
    "implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json";

pub(super) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn read(path: &Path) -> Result<Vec<u8>> {
    ensure!(
        fs::metadata(path)?.len() <= 24 * 1024 * 1024,
        "Diagnostic input exceeds bound"
    );
    Ok(fs::read(path)?)
}
pub(super) fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    let bytes = read(path)?;
    ensure!(
        hash(&bytes) == expected,
        "Frozen artifact changed: {}",
        path.display()
    );
    Ok(bytes)
}
pub(super) fn write_new(path: &Path, value: &Value) -> Result<String> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(hash(&bytes))
}
pub(super) fn executable() -> Result<Value> {
    let path = std::env::current_exe()?.canonicalize()?;
    Ok(json!({"path":path,"sha256":hash(&fs::read(std::env::current_exe()?)?)}))
}
pub(super) fn source_hashes(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut names = vec![
        "scripts/validation/tutor_probe_support.rs".to_owned(),
        "Cargo.toml".to_owned(),
        "rust/ai-tutor/Cargo.toml".to_owned(),
        "Cargo.lock".to_owned(),
        "rust/ai-tutor/src/lib.rs".to_owned(),
        "rust/ai-tutor/src/context.rs".to_owned(),
        "rust/ai-tutor/src/store.rs".to_owned(),
        "rust/ai-tutor/src/tutor.rs".to_owned(),
        "rust/ai-tutor/src/tutor/citation.rs".to_owned(),
        "rust/ai-tutor/src/tutor_segments.rs".to_owned(),
        "rust/ai-tutor/src/memory_contract.rs".to_owned(),
        "rust/ai-tutor/src/memory_embedding.rs".to_owned(),
        "rust/ai-tutor/src/memory_embedding/profile.rs".to_owned(),
        "prompts/tutor-agent.md".to_owned(),
        "prompts/memory_consolidation.md".to_owned(),
        POLICY.to_owned(),
        SCHEMA.to_owned(),
    ];
    for entry in fs::read_dir(root.join("rust/ai-tutor/src"))? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if (name == "kimi.rs" || name.starts_with("kimi_")) && name.ends_with(".rs") {
            names.push(format!("rust/ai-tutor/src/{name}"));
        }
    }
    for entry in fs::read_dir(root.join("rust/ai-tutor/src/store"))? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.ends_with(".rs") {
            names.push(format!("rust/ai-tutor/src/store/{name}"));
        }
    }
    names
        .into_iter()
        .map(|name| Ok((name.clone(), hash(&read(&root.join(name))?))))
        .collect()
}
pub(super) struct TemporaryStore(pub PathBuf);
impl Drop for TemporaryStore {
    fn drop(&mut self) {
        for name in ["case.sqlite3", "case.sqlite3-wal", "case.sqlite3-shm"] {
            let _ = fs::remove_file(self.0.join(name));
        }
        let _ = fs::remove_dir(&self.0);
    }
}
pub(super) fn selected(rows: &[Value], case: Option<&str>) -> Result<Vec<usize>> {
    match case {
        Some(id) => Ok(vec![
            rows.iter()
                .position(|v| s(v, "case_id") == id)
                .ok_or_else(|| anyhow!("Unknown frozen case"))?,
        ]),
        None => Ok((0..rows.len()).collect()),
    }
}
pub(super) fn preflight_attempts(prepared: &Path, rows: &[Value], indices: &[usize]) -> Result<()> {
    for &i in indices {
        ensure!(
            !prepared
                .join(format!("attempt-{}.json", s(&rows[i], "case_id")))
                .exists()
                && !prepared
                    .join(format!("not-run-{}.json", s(&rows[i], "case_id")))
                    .exists(),
            "Case already attempted or frozen as not run; no retry"
        );
    }
    Ok(())
}
fn collect_segment(
    ledger: &mut DeliveryLedger,
    segment: TutorSegment,
    saved: &mut Vec<Value>,
    began: Instant,
) -> Result<()> {
    ledger.accept(&segment)?;
    saved.push(json!({"sequence":segment.sequence(),"text":segment.text(),"used_evidence_ids":segment.used_evidence_ids(),
        "binding":segment.binding(),"consumer_observed_ms":began.elapsed().as_secs_f64()*1000.0,
        "protocol_validated":true,"displayed":false,"played":false}));
    Ok(())
}
async fn generate_case(provider: &KimiProvider, row: &Value) -> Result<Value> {
    let request = &row["request"];
    let rid = s(request, "request_id");
    let generation = request["generation"]
        .as_u64()
        .ok_or_else(|| anyhow!("Missing generation"))?;
    let mut ledger = DeliveryLedger::new(DeliveryBinding {
        request_id: rid.into(),
        generation,
    })?;
    let (sender, mut receiver) = mpsc::channel(SEGMENT_CHANNEL_CAPACITY);
    let began = Instant::now();
    let operation = provider.generate_segments(request, generation, sender);
    tokio::pin!(operation);
    let mut saved = vec![];
    let mut channel_open = true;
    let mut consumer_error = None;
    let outcome = loop {
        tokio::select! {biased;
            result=&mut operation=>break result,
            segment=receiver.recv(),if channel_open=>{
                match segment {
                    Some(segment) if consumer_error.is_none()=>{
                        if let Err(error)=collect_segment(&mut ledger,segment,&mut saved,began) {
                            consumer_error=Some(error.to_string());
                            if let Err(cancel)=provider.cancel(rid).await {consumer_error=Some(format!("{error}; cancellation: {cancel}"));}
                        }
                    },
                    Some(_)=>{},
                    None=>channel_open=false,
                }
            }
        }
    };
    // Preserve already validated diagnostic prefixes even when final validation
    // fails. They are never given display/play receipts or treated as a final.
    while let Ok(segment) = receiver.try_recv() {
        if consumer_error.is_none() {
            if let Err(error) = collect_segment(&mut ledger, segment, &mut saved, began) {
                consumer_error = Some(error.to_string());
            }
        }
    }
    let mut final_reply = Value::Null;
    let error = match outcome {
        Ok(reply) if consumer_error.is_none() => match ledger.finish_validated(&reply) {
            Ok(()) => {
                final_reply = reply.into_reply();
                None
            }
            Err(error) => Some(error.to_string()),
        },
        Ok(_) => consumer_error,
        Err(error) => Some(error.to_string()),
    };
    if error.is_some() {
        ledger.invalidate();
    }
    let status = provider.status();
    let metrics = status["metrics"]["last_acp_segments"].clone();
    let cleanup = metrics["process_reaped"] == true
        && metrics["owned_directory_removed"] == true
        && metrics["stderr_drained"] == true
        && status["running"] == 0;
    ensure!(
        ledger
            .segments()
            .iter()
            .all(|v| !v.displayed && v.speech == SpeechState::NotStarted),
        "Diagnostic issued delivery receipt"
    );
    Ok(
        json!({"case_id":row["case_id"],"language":row["language"],"request_id":rid,"generation":generation,
        "reply_validated":!final_reply.is_null(),"final_reply":final_reply,"validated_segments":saved,"error":error,
        "protocol_and_cleanup_passed":error.is_none() && cleanup,"cleanup_confirmed":cleanup,"provider_metrics":metrics,
        "elapsed_ms":began.elapsed().as_secs_f64()*1000.0,"delivery_ledger":ledger,
        "first_segment_time_scope":"consumer observation including gate/session setup; not display or audio",
        "displayed":false,"played":false,"candidates_applied":false,"devices_opened":false,"real_learner_data":false,
        "raw_provider_stream_retained":false,"semantic_quality_pass":null,"semantic_review_status":"pending_independent_review"}),
    )
}
pub(super) async fn run_model_cases(
    root: &Path,
    prepared: &Path,
    manifest_sha: &str,
    rows: &[Value],
    indices: &[usize],
) -> Result<()> {
    preflight_attempts(prepared, rows, indices)?;
    let provider = KimiProvider::new(root)?;
    let run_id = uid("run");
    let mut outcomes = vec![];
    let operation:Result<()>=async {
        let audit=provider.probe().await?;ensure!(audit["available"]==true,"Verified tool-free provider required");
        let identity=json!({"version":audit["version"],"binary_sha256":audit["binary_sha256"],"agent_sha256":audit["agent_sha256"],
            "tools":audit["tools"],"subagents":audit["subagents"],"model_alias_and_sampling_independently_frozen":false});
        write_new(&prepared.join(format!("{run_id}-start.json")),&json!({"case_ids":indices.iter().map(|&i|&rows[i]["case_id"]).collect::<Vec<_>>(),"provider":identity,"prepared_manifest_sha256":manifest_sha,"automatic_retries":0}))?;
        for &i in indices {
            let row=&rows[i];let id=s(row,"case_id");
            write_new(&prepared.join(format!("attempt-{id}.json")),&json!({"case_id":id,"request_id":row["request"]["request_id"],"run_id":run_id,"attempted_at":chrono::Utc::now().to_rfc3339(),"retries":0,"request_sha256":hash(&serde_json::to_vec(&row["request"])?) }))?;
            let mut result=match generate_case(&provider,row).await {
                Ok(value)=>value,
                Err(error)=>json!({"case_id":id,"language":row["language"],"request_id":row["request"]["request_id"],
                    "reply_validated":false,"final_reply":null,"validated_segments":[],"protocol_and_cleanup_passed":false,
                    "error":error.to_string(),"semantic_quality_pass":null,"displayed":false,"played":false,
                    "candidates_applied":false,"devices_opened":false,"real_learner_data":false,
                    "provider_metrics":provider.status()["metrics"]["last_acp_segments"]}),
            };
            result["run_id"]=json!(run_id);result["provider"]=identity.clone();
            write_new(&prepared.join(format!("result-{id}.json")),&result)?;
            outcomes.push(json!({"case_id":id,"protocol_and_cleanup_passed":result["protocol_and_cleanup_passed"]}));
            ensure!(result["protocol_and_cleanup_passed"]==true,"Case failed; result preserved, later cases not attempted");
        }
        Ok(())
    }.await;
    let closed = provider.close().await;
    write_new(
        &prepared.join(format!("{run_id}-summary.json")),
        &json!({"outcomes":outcomes,
        "operation_error":operation.as_ref().err().map(ToString::to_string),"provider_close_ok":closed.is_ok(),
        "provider_close_error":closed.as_ref().err().map(ToString::to_string),"provider_running_after_close":provider.status()["running"],
        "semantic_quality_pass":null,"displayed":false,"played":false,"candidates_applied":false,"devices_opened":false}),
    )?;
    operation?;
    closed?;
    Ok(())
}
