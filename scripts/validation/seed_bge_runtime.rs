//! Persistent, isolated typed48 preparation. No provider, encoder or device calls.
#[allow(dead_code)]
#[path = "../memory-lab/typed48_common.rs"]
mod typed48;

use ai_tutor::store::{MemoryJobLimits, Store, empty_state, s};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::{collections::{BTreeMap, HashSet}, fs, io::{Seek, SeekFrom, Write}, path::{Path, PathBuf}};
use typed48::{Frozen, hash};

const METHOD_PATH: &str = "runtime/goal-baselines/20261110/bge-runtime/evaluation-method.json";
const METHOD_SHA: &str = "6f50ee1da233a7c40b592eacd57b0fd145634224cf6a90886581bd63469ca0eb";
const PROJECTION_PATH: &str = "runtime/goal-baselines/20261104/typed48-projection-first/report.json";
const HELPER_PATH: &str = "scripts/memory-lab/typed48_common.rs";
const HISTORY_PATH: &str = "scripts/memory-lab/scoped_semantic_probe.rs";
const JOB_VERSION: &str = "synthetic-typed48-projection-v1";

fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    ensure!(fs::metadata(path)?.len() <= 8 * 1024 * 1024, "Frozen artifact exceeds limit");
    let bytes = fs::read(path)?;
    ensure!(hash(&bytes) == expected, "Frozen artifact changed: {}", path.display());
    Ok(bytes)
}
fn reference<'a>(method: &'a Value, path: &str) -> Result<&'a Value> {
    method["reference_artifacts"].as_array().ok_or_else(||anyhow!("Missing reference bindings"))?
        .iter().find(|entry|s(entry,"path").replace('\\',"/") == path)
        .ok_or_else(||anyhow!("Missing frozen reference: {path}"))
}
fn load(root: &Path) -> Result<(Value,Frozen,Value)> {
    let method: Value = serde_json::from_slice(&checked(&root.join(METHOD_PATH),METHOD_SHA)?)?;
    ensure!(method["root_data_contract_approved"] == true && method["fixture"]["source_count"] == 60,
        "Unexpected preparation contract");
    for entry in method["fixture"]["files"].as_array().unwrap() {
        checked(&root.join(s(entry,"path").replace('\\',"/")),s(entry,"sha256"))?;
    }
    let helper = checked(&root.join(HELPER_PATH),s(reference(&method,HELPER_PATH)?,"sha256"))?;
    ensure!(hash(include_bytes!("../memory-lab/typed48_common.rs")) == hash(&helper),
        "Compiled typed48 helper differs from the frozen project helper");
    checked(&root.join(HISTORY_PATH),s(reference(&method,HISTORY_PATH)?,"sha256"))?;
    let projection: Value = serde_json::from_slice(&checked(&root.join(PROJECTION_PATH),
        s(reference(&method,PROJECTION_PATH)?,"sha256"))?)?;
    let frozen = typed48::load(root)?;
    for source in &frozen.sources {
        ensure!(method["fixture"]["source_text_hashes"][s(source,"id")] == hash(s(source,"text").as_bytes()),
            "Original source text hash changed");
    }
    for slot in method["slots"].as_array().unwrap() {
        let query = frozen.queries.iter().find(|row|row["id"] == slot["original_query_id"])
            .ok_or_else(||anyhow!("Frozen slot has no original query"))?;
        ensure!(query["text"] == slot["question_exact"]
            && hash(s(query,"text").as_bytes()) == slot["question_utf8_sha256"], "Frozen slot text differs");
    }
    Ok((method,frozen,projection))
}
fn proposal(source: &Value, event: &Value, kind: &str) -> Value {
    let id = s(event,"event_id");
    let mut value = json!({"kind":kind,"claim_type":source["claim_type"],"content":source["text"],
        "scope":"current_activity","support_level":source["support_level"],"source_event_ids":[id],
        "source_event_versions":{},"source_quotes":[{"event_id":id,"quote":source["text"]}],
        "importance":{"goal_relevance":0,"continuity":0,"novel_change":0,"explicit_request":0}});
    value["source_event_versions"][id] = event["event_version"].clone();
    value
}
fn clock() -> i64 { chrono::Utc::now().timestamp_millis() }

fn seed(store: &Store, frozen: &Frozen, original: &Value, method: &Value) -> Result<Value> {
    // Each public Store operation keeps its normal transaction. On any failure
    // the isolated attempt is retained for diagnosis and is never resumed.
        let lid = s(&store.create_learner("Synthetic bilingual memory runtime", "zh", "guided")?,"learner_id").to_owned();
        let mut packs = Vec::new();
        for source in &frozen.sources {
            let pack = s(source,"pack_id").to_owned();
            if !packs.contains(&pack) { packs.push(pack); }
        }
        ensure!(packs.len() == 6, "Expected six parent activities");
        let mut decisions = Vec::<Value>::new();
        let mut parents = BTreeMap::<String,Value>::new();
        let mut jobs = Vec::new();
        for pack in &packs {
            let session = store.create_session(&lid)?;
            let sid = s(&session,"session_id");
            let sources: Vec<_> = frozen.sources.iter().filter(|source|s(source,"pack_id") == pack).collect();
            ensure!(sources.len() == 10, "A parent must contain all ten original sources");
            let mut batchable = Vec::new();
            for source in sources {
                // Evaluation aliases and labels live only in the external report.
                let mut content = json!({"text":source["text"],"support_level":source["support_level"]});
                if !source["accepted"].is_null() { content["accepted"] = source["accepted"].clone(); }
                let event = store.record_education_event(&lid,sid,"simulated",s(source,"event_kind"),&content)?;
                ensure!(event["content"] == content && event["event_kind"] == source["event_kind"]
                    && event["event_version"] == 1 && event["source_type"] == "simulated", "Source import changed evidence");
                let target = typed48::kind(source);
                let index = decisions.len();
                decisions.push(json!({"source_id":source["id"],"pack_id":pack,"original_source":source,
                    "original_text_sha256":hash(s(source,"text").as_bytes()),"event_id":event["event_id"],
                    "event_version":event["event_version"],"session_id":sid,"activity_id":session["activity_id"],
                    "live_imported_event":event,"projected":false,"projection_kind":target,"memory_id":null,
                    "exclusion_reason":if target.is_some(){Value::Null}else{json!(match s(source,"claim_type"){
                        "unaccepted_plan"=>"proposal_not_accepted","temporary_request"=>"temporary_request_not_persistent_preference",
                        _=>"unsupported_source_type_or_support"})}}));
                if let Some(target) = target { batchable.push((index,source,event,target)); }
            }
            ensure!(batchable.len() == 8, "Frozen projection must retain eight sources per pack");
            for batch in batchable.chunks(5) {
                let ids: Vec<_> = batch.iter().map(|(_,_,event,_)|s(event,"event_id").to_owned()).collect();
                let limits = MemoryJobLimits::default();
                let job = store.enqueue_memory_job(&lid,sid,&ids,JOB_VERSION,&limits,clock())?;
                let claim = store.claim_memory_job_for("bge-runtime-seed",&limits,clock(),s(&job,"job_id"))?
                    .ok_or_else(||anyhow!("Synthetic projection job could not be claimed"))?;
                ensure!(claim["job"]["job_id"] == job["job_id"], "Claimed unrelated work");
                let proposals: Vec<_> = batch.iter().map(|(_,source,event,target)|proposal(source,event,target)).collect();
                let staged = store.stage_memory_candidates(&claim["receipt"],&proposals,clock())?;
                ensure!(staged.len() == batch.len(), "Candidate count changed");
                for ((index,source,event,target),staged) in batch.iter().zip(&staged) {
                    let evidence = store.apply_memory_candidate(&lid,s(staged,"candidate_id"),&[*target],clock())?;
                    let expected = original["projection"]["decisions"].as_array().unwrap().iter()
                        .find(|row|row["source_id"] == source["id"]).ok_or_else(||anyhow!("Missing frozen projection"))?;
                    ensure!(expected["projected"] == true && expected["projection_kind"] == *target
                        && evidence["memory"]["content"] == expected["encoding_text"]
                        && evidence["education_metadata"] == expected["memory_evidence"]["education_metadata"],
                        "Production text or education metadata differs from frozen typed48 projection");
                    ensure!(evidence["namespace"] == "simulator" && evidence["memory"]["learner_id"] == lid
                        && evidence["memory"]["source_type"] == "simulated" && evidence["memory"]["epoch"] == 0
                        && evidence["memory"]["scope"] == "current_activity" && evidence["memory"]["status"] == "active"
                        && evidence["sources"] == json!([event])
                        && evidence["source_links"] == json!([{"event_id":event["event_id"],"event_version":1}]),
                        "Projected source/owner/scope changed");
                    let row = &mut decisions[*index];
                    row["projected"] = json!(true);
                    row["memory_id"] = evidence["memory"]["memory_id"].clone();
                    row["encoding_text"] = evidence["memory"]["content"].clone();
                    row["encoding_text_sha256"] = json!(hash(s(&evidence["memory"],"content").as_bytes()));
                    row["memory_evidence"] = evidence;
                }
                let status = store.memory_job_status(&lid,s(&job,"job_id"))?;
                ensure!(status["status"] == "applied" && status["attempt_receipts"][0]["started_at_ms"].is_null(),
                    "Synthetic projection must have no model-start receipt");
                jobs.push(json!({"job_id":job["job_id"],"pack_id":pack,"candidate_count":batch.len(),
                    "status":"applied","model_call_started":false}));
            }
            ensure!(store.session(sid)?["state"] == empty_state(), "Projection invented current progress");
            store.finish_session(sid)?;
            // Frozen scoped::seed_history: finish first so history adds no new
            // summaries, then reconstruct only the original sourced goal/plan.
            let mut state = store.session(sid)?["state"].clone();
            for (field,suffix) in [("goal","s10"),("pending_step","s07")] {
                let alias = format!("{pack}_{suffix}");
                let row = decisions.iter().find(|row|row["source_id"] == alias).unwrap();
                ensure!(if field == "goal" {row["original_source"]["claim_type"] == "stated_goal"}
                    else {row["original_source"]["accepted"] == true}, "Historical source boundary changed");
                state[field] = row["original_source"]["text"].clone();
                state["field_evidence"][field] = json!({"source_event_ids":[row["event_id"]],
                    "source_event_versions":{(s(row,"event_id")):1}});
            }
            store.checkpoint(sid,&state)?;
            let coverage = store.embedding_coverage_for_session(sid,
                &ai_tutor::memory_embedding::EmbeddingProfile::BgeM3NativeFp32.model_key())?;
            ensure!(coverage.eligible == 8 && coverage.indexed == 0, "Parent retrieval scope must contain exactly eight unindexed memories");
            parents.insert(pack.clone(),json!({"session_id":sid,"activity_id":session["activity_id"],
                "status":"finished","source_count":10,"memory_count":8,"embedding_coverage":coverage,"historical_state":state}));
        }
        ensure!(decisions.len() == 60 && decisions.iter().filter(|row|row["projected"] == true).count() == 48
            && store.memories(&lid)?.len() == 48, "Complete 60-source/48-memory pool required");
        let all_ids: Vec<_> = decisions.iter().map(|row|s(row,"event_id").to_owned()).collect();
        ensure!(store.source_events(&lid,&all_ids)?.len() == 60, "Imported source pool became invalid");
        for row in decisions.iter().filter(|row|row["projected"] == true) {
            ensure!(store.memory_evidence(&lid,s(row,"memory_id"))? == row["memory_evidence"], "Live evidence changed");
        }
        let empty = store.create_session(&lid)?;
        ensure!(empty["state"] == empty_state() && empty["turns"] == json!([])
            && empty["continuation"].is_null(), "Final selectable activity must be new and empty");
        let mut slots = Vec::new();
        for slot in method["slots"].as_array().unwrap() {
            let pack = s(slot,"pack_id");
            let available: HashSet<String> = decisions.iter().filter(|row|row["projected"] == true && row["pack_id"] == pack)
                .map(|row|s(row,"source_id").to_owned()).collect();
            if slot["original_frozen_label"]["answerable"] == true {
                ensure!(typed48::complete_set_available(&slot["minimum_sufficient_evidence_sets_unchanged"],&available),
                    "A complete original answer set is not available in the selected parent");
            }
            slots.push(json!({"slot":slot["slot"],"original_query_id":slot["original_query_id"],"pack_id":pack,
                "source_session_id":parents[pack]["session_id"],"source_activity_id":parents[pack]["activity_id"],
                "selected_parent_memory_count":8,"full_learner_memory_count":48,
                "quality_scope":"Eight legally scoped parent memories; not retrieval over an unfiltered 48-memory pool",
                "question_exact":slot["question_exact"],"question_utf8_sha256":slot["question_utf8_sha256"],
                "execution":"Use ordinary HTTP continue from this finished parent into a fresh child for this slot; never chain through another slot"}));
        }
        Ok(json!({"status":"seeded_no_model_or_encoding","synthetic_only":true,"namespace":"simulator",
            "learner_id":lid,"source_count":60,"memory_count":48,"excluded_count":12,
            "retrieval_scope_note":"Each question continues exactly one parent with eight legal memories; the complete learner database contains 48. Do not report this as unfiltered 48-memory retrieval quality.",
            "parents":parents,"source_mapping":decisions,"jobs":jobs,"slots":slots,
            "selectable_session_id":empty["session_id"],"selectable_activity_id":empty["activity_id"],
            "selectable_session":empty,"additional_canaries":[],"provider_metrics":store.run_metrics()?,
            "model_calls":0,"encoding_calls":0,"network_calls":0,"device_calls":0}))
}

fn write_report(file: &mut fs::File, value: &Value) -> Result<()> {
    file.seek(SeekFrom::Start(0))?; file.set_len(0)?;
    serde_json::to_writer_pretty(&mut *file,value)?; file.write_all(b"\n")?; file.sync_all()?;
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(args.len() == 6 && args[0] == "--root" && args[2] == "--output-root" && args[4] == "--report",
        "usage: seed-bge-runtime --root PROJECT_ROOT --output-root NEW_PREVIEW_ROOT --report NEW_REPORT.json");
    let root = PathBuf::from(&args[1]).canonicalize()?;
    let output = PathBuf::from(&args[3]); let report = PathBuf::from(&args[5]);
    ensure!(!report.exists(), "Report already exists; never overwrite a seed attempt");
    let (method,frozen,projection) = load(&root)?;
    let data = output.join("runtime/data");
    if data.exists() { ensure!(fs::read_dir(&data)?.next().is_none(), "Output database directory must be new or empty"); }
    fs::create_dir_all(&data)?;
    let output = output.canonicalize()?;
    ensure!(output != root, "Cannot seed the delivered project root");
    fs::create_dir_all(report.parent().filter(|path|!path.as_os_str().is_empty()).unwrap_or(Path::new(".")))?;
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&report)?;
    let mut artifact = json!({"status":"seed_attempt_started","at_utc":chrono::Utc::now().to_rfc3339(),
        "method_path":METHOD_PATH,"method_sha256":METHOD_SHA,"fixture_files":method["fixture"]["files"],
        "seed_source_sha256":hash(include_bytes!("seed_bge_runtime.rs")),"output_root":output,
        "database":output.join("runtime/data/simulator.sqlite3"),"synthetic_only":true,"model_calls":0});
    write_report(&mut file,&artifact)?;
    let result = (|| -> Result<Value> {
        let database = output.join("runtime/data/simulator.sqlite3");
        // Reserve the new path atomically. Store performs all schema/business writes.
        fs::OpenOptions::new().write(true).create_new(true).open(&database)?;
        let store = Store::open(&database,"simulator")?;
        let result = seed(&store,&frozen,&projection,&method)?;
        drop(store);
        Ok(result)
    })();
    match result {
        Ok(result) => {
            for (key,value) in result.as_object().unwrap() { artifact[key] = value.clone(); }
            write_report(&mut file,&artifact)?;
            println!("{}",json!({"status":artifact["status"],"learner_id":artifact["learner_id"],
                "selectable_session_id":artifact["selectable_session_id"],"source_count":60,"memory_count":48,"model_calls":0}));
            Ok(())
        }
        Err(error) => {
            artifact["status"] = json!("seed_failed_no_retry"); artifact["error"] = json!(error.to_string());
            write_report(&mut file,&artifact)?;
            Err(error)
        }
    }
}
