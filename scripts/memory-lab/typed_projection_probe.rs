//! Deterministic synthetic proposals through the real typed Store promotion API.
//! Exports exact future encoder inputs. No model, network, device or user DB.
use ai_tutor::{
    memory_embedding::EmbeddingProfile,
    store::{MemoryJobLimits, Store, s, uid},
};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const FIXTURE_SHA: &str = "858ee969827d54d1687d1a3966e726576a66024f9b462683323732dde94fbb81";
const METHOD: &str = include_str!("typed_projection_method_v1.json");
const PROMPT_VERSION: &str = "synthetic-typed-projection-v1";
const CLOCK_MS: i64 = 1_793_721_600_000;

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    ensure!(
        fs::metadata(path)?.len() < 4 * 1024 * 1024,
        "Fixture file exceeds bound"
    );
    let bytes = fs::read(path)?;
    ensure!(
        hash(&bytes) == expected,
        "Frozen fixture changed: {}",
        path.display()
    );
    Ok(bytes)
}
fn rows(bytes: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(bytes)?
        .lines()
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}
struct Frozen {
    sources: Vec<Value>,
    queries: Vec<Value>,
    files: Value,
}
fn load(root: &Path) -> Result<Frozen> {
    let base = root.join("rust/ai-tutor/tests/fixtures/diagnostic_postfreeze_v2");
    let manifest: Value =
        serde_json::from_slice(&checked(&base.join("manifest.json"), FIXTURE_SHA)?)?;
    let mut files = HashMap::new();
    for name in [
        "sources.jsonl",
        "queries.jsonl",
        "labels.jsonl",
        "scope_controls.jsonl",
    ] {
        files.insert(
            name,
            checked(&base.join(name), s(&manifest["files"][name], "sha256"))?,
        );
    }
    let sources = rows(&files["sources.jsonl"])?;
    let queries = rows(&files["queries.jsonl"])?;
    ensure!(
        sources.len() == 60 && queries.len() == 96,
        "Full frozen inputs required"
    );
    ensure!(
        sources.iter().all(|v| v["version"] == 1
            && v["status"] == "active"
            && v["source_type"] == "simulated"
            && v["namespace"] == "simulator"
            && v["scope"] == "current_activity"
            && v["hardware_verified"] == false),
        "Fixture source boundary changed"
    );
    Ok(Frozen {
        sources,
        queries,
        files: manifest["files"].clone(),
    })
}
fn kind(source: &Value) -> Option<&'static str> {
    match (s(source, "claim_type"), s(source, "event_kind")) {
        ("reported_observation", "observation") => Some("trial_observation"),
        ("task_specific_response", "learner_answer") => Some("concept_evidence"),
        ("accepted_plan", "plan") if source["accepted"] == true => {
            Some("accepted_pending_experiment")
        }
        ("explicit_correction", "correction") => Some("correction"),
        ("stated_goal", "learner_statement") => Some("learning_goal"),
        _ => None,
    }
}
fn excluded_reason(source: &Value) -> &'static str {
    match s(source, "claim_type") {
        "condition_limit" => "unsupported_condition_limit_kind",
        "unaccepted_plan" => "proposal_not_accepted",
        "temporary_request" => "temporary_request_not_persistent_preference",
        _ => "unsupported_source_type",
    }
}
fn prefix(kind: &str) -> &'static str {
    match kind {
        "trial_observation" => "据报告的尝试与观察",
        "concept_evidence" => "本次回答证据，不代表已掌握",
        "accepted_pending_experiment" => "已接受、尚待完成的实验",
        "correction" => "明确更正",
        "learning_goal" => "明确表达的学习目标",
        _ => unreachable!(),
    }
}
struct OwnedDirectory(PathBuf);
impl Drop for OwnedDirectory {
    fn drop(&mut self) {
        for name in [
            "projection.sqlite3",
            "projection.sqlite3-wal",
            "projection.sqlite3-shm",
        ] {
            let _ = fs::remove_file(self.0.join(name));
        }
        let _ = fs::remove_dir(&self.0);
    }
}
struct Fixture {
    store: Store,
    _directory: OwnedDirectory,
    lid: String,
}
impl Fixture {
    fn new() -> Result<Self> {
        let directory = std::env::temp_dir().join(uid("ai-tutor-typed-projection"));
        fs::create_dir(&directory)?;
        let owned = OwnedDirectory(directory.clone());
        let store = Store::open(directory.join("projection.sqlite3"), "simulator")?;
        let lid = s(
            &store.create_learner("SYNTHETIC_TYPED_PROJECTION", "zh", "guided")?,
            "learner_id",
        )
        .to_owned();
        Ok(Self {
            store,
            _directory: owned,
            lid,
        })
    }
    fn session(&self) -> Result<String> {
        Ok(s(&self.store.create_session(&self.lid)?, "session_id").to_owned())
    }
    fn event(&self, sid: &str, source: &Value) -> Result<Value> {
        let mut content = json!({"text":source["text"],"support_level":source["support_level"],"synthetic_fixture":true,"fixture_source_id":source["id"]});
        if !source["accepted"].is_null() {
            content["accepted"] = source["accepted"].clone();
        }
        let event = self.store.record_education_event(
            &self.lid,
            sid,
            s(source, "source_type"),
            s(source, "event_kind"),
            &content,
        )?;
        ensure!(
            event["event_version"] == source["version"]
                && event["event_kind"] == source["event_kind"]
                && event["content"]["text"] == source["text"]
                && event["content"]["support_level"] == source["support_level"],
            "Source changed during import"
        );
        ensure!(
            event["content"].get("explicit_preference").is_none()
                && event["content"].get("preference_scope").is_none(),
            "Invented preference markers"
        );
        Ok(event)
    }
    fn claim(&self, sid: &str, events: &[Value], clock: i64) -> Result<Value> {
        let ids: Vec<_> = events.iter().map(|v| s(v, "event_id").to_owned()).collect();
        let limits = MemoryJobLimits::default();
        let job =
            self.store
                .enqueue_memory_job(&self.lid, sid, &ids, PROMPT_VERSION, &limits, clock)?;
        let claim = self
            .store
            .claim_memory_job("synthetic-projection", &limits, clock)?
            .ok_or_else(|| anyhow!("Synthetic claim unavailable"))?;
        ensure!(
            claim["job"]["job_id"] == job["job_id"],
            "Wrong synthetic job"
        );
        Ok(claim)
    }
}
fn candidate(source: &Value, event: &Value, kind: &str) -> Value {
    let eid = s(event, "event_id");
    let mut value = json!({"kind":kind,"claim_type":source["claim_type"],"content":source["text"],"scope":"current_activity",
        "source_event_ids":[eid],"source_event_versions":{},"source_quotes":[{"event_id":eid,"quote":source["text"]}],
        "support_level":source["support_level"],"importance":{"goal_relevance":0,"continuity":0,"novel_change":0,"explicit_request":0}});
    value["source_event_versions"][eid] = event["event_version"].clone();
    value
}
fn project(frozen: &Frozen) -> Result<(Value, Vec<Value>)> {
    let f = Fixture::new()?;
    let mut packs = Vec::<String>::new();
    for source in &frozen.sources {
        let pack = s(source, "pack_id").to_owned();
        if !packs.contains(&pack) {
            packs.push(pack);
        }
    }
    ensure!(packs.len() == 6, "Expected six frozen topics");
    let mut decisions = vec![];
    let mut jobs = vec![];
    let mut sessions = vec![];
    let mut clock = CLOCK_MS;
    let mut mapped = HashMap::<String, String>::new();
    for pack in packs {
        let sid = f.session()?;
        let sources: Vec<_> = frozen
            .sources
            .iter()
            .filter(|v| s(v, "pack_id") == pack)
            .collect();
        let mut supported = vec![];
        for source in &sources {
            let event = f.event(&sid, source)?;
            let target = kind(source);
            let index = decisions.len();
            decisions.push(json!({"source_id":source["id"],"original_source":source,"original_text_sha256":hash(s(source,"text").as_bytes()),
                "event_id":event["event_id"],"event_version":event["event_version"],"event_kind":event["event_kind"],"event_source_grade":event["source_type"],
                "projected":false,"projection_kind":target,"exclusion_reason":if target.is_none(){Some(excluded_reason(source))}else{None},"live_imported_event":event}));
            if let Some(target) = target {
                supported.push((index, *source, event, target));
            }
        }
        ensure!(
            sources.len() == 10 && supported.len() == 7,
            "Declared topic projection count changed"
        );
        for batch in supported.chunks(5) {
            let events: Vec<_> = batch.iter().map(|(_, _, event, _)| event.clone()).collect();
            let claim = f.claim(&sid, &events, clock)?;
            let proposals: Vec<_> = batch
                .iter()
                .map(|(_, source, event, target)| candidate(source, event, target))
                .collect();
            let staged =
                f.store
                    .stage_memory_candidates(&claim["receipt"], &proposals, clock + 1)?;
            ensure!(staged.len() == batch.len(), "Staged proposal count changed");
            for ((index, source, _, target), staged) in batch.iter().zip(&staged) {
                let applied = f.store.apply_memory_candidate(
                    &f.lid,
                    s(staged, "candidate_id"),
                    &[*target],
                    clock + 2,
                )?;
                let text = s(&applied["memory"], "content");
                ensure!(
                    text == format!("{}：{}", prefix(target), s(source, "text")),
                    "Actual policy projection differs from predeclared contract"
                );
                ensure!(
                    applied["sources"].as_array().is_some_and(|v| v.len() == 1)
                        && applied["education_metadata"]["support_level"]
                            == source["support_level"]
                        && applied["education_metadata"]["source_grade"] == "simulated",
                    "Provenance lost during promotion"
                );
                let mid = s(&applied["memory"], "memory_id").to_owned();
                mapped.insert(mid.clone(), s(source, "id").to_owned());
                decisions[*index]["projected"] = json!(true);
                decisions[*index]["memory_id"] = json!(mid);
                decisions[*index]["encoding_text"] = json!(text);
                decisions[*index]["encoding_text_sha256"] = json!(hash(text.as_bytes()));
                decisions[*index]["memory_evidence"] = applied;
            }
            let status = f
                .store
                .memory_job_status(&f.lid, s(&claim["job"], "job_id"))?;
            ensure!(
                status["status"] == "applied"
                    && status["attempt_receipts"][0]["started_at_ms"].is_null(),
                "Synthetic projection must not claim a model call started"
            );
            jobs.push(json!({"job_id":status["job_id"],"status":status["status"],"candidate_count":staged.len(),"model_call_started":false}));
            clock += 10;
        }
        let session = f.store.session(&sid)?;
        ensure!(
            session["state"]["reported_result"] == ""
                && session["state"]["pending_step"] == ""
                && session["state"]["attempts"] == json!([]),
            "Projection invented teaching progress"
        );
        sessions.push(json!({"original_pack_id":pack,"original_session_id":sources[0]["session_id"],"store_session_id":sid,"progress_state_unchanged":true}));
    }
    let profile = EmbeddingProfile::BgeM3NativeFp32;
    let key = profile.model_key();
    let mut work = vec![];
    // No vector is committed: a second work request would repeat the first
    // batch. Report this 32-row check separately from all 42 live evidence reads.
    let first = f.store.embedding_work(&f.lid, &key, 32)?;
    for snapshot in first {
        let decision = decisions
            .iter()
            .find(|row| s(row, "source_id") == mapped[&snapshot.memory_id])
            .ok_or_else(|| anyhow!("Snapshot source missing"))?;
        ensure!(
            snapshot.model_key == key
                && snapshot.dimension == 1024
                && snapshot.content == s(decision, "encoding_text"),
            "Wrong encoder profile or actual snapshot text"
        );
        work.push(snapshot.memory_id);
    }
    ensure!(work.len() == 32, "Expected bounded first work batch");
    let mut inputs = vec![];
    let mut counts = BTreeMap::<String, usize>::new();
    let mut excluded = BTreeMap::<String, usize>::new();
    for decision in &decisions {
        if decision["projected"] == true {
            let current = f.store.memory_evidence(&f.lid, s(decision, "memory_id"))?;
            ensure!(
                current == decision["memory_evidence"],
                "Final live evidence changed"
            );
            *counts
                .entry(s(decision, "projection_kind").to_owned())
                .or_default() += 1;
            inputs.push(json!({"key":format!("source:{}",s(decision,"source_id")),"input_kind":"projected_memory","source_ids":[decision["source_id"]],"text":decision["encoding_text"],"text_sha256":decision["encoding_text_sha256"]}));
        } else {
            *excluded
                .entry(s(decision, "exclusion_reason").to_owned())
                .or_default() += 1;
        }
    }
    ensure!(
        inputs.len() == 42 && decisions.len() == 60 && f.store.memories(&f.lid)?.len() == 42,
        "Projection counts changed"
    );
    for condition in ["current_query", "query_plus_goal"] {
        for query in &frozen.queries {
            let text = if condition == "current_query" {
                s(query, "text").to_owned()
            } else {
                format!("{} {} ", s(query, "text"), s(query, "goal"))
            };
            inputs.push(json!({"key":format!("{condition}:{}",s(query,"id")),"input_kind":condition,"query_id":query["id"],"text_sha256":hash(text.as_bytes()),"text":text}));
        }
    }
    ensure!(
        inputs.len() == 234
            && inputs
                .iter()
                .map(|v| s(v, "key"))
                .collect::<HashSet<_>>()
                .len()
                == 234,
        "Encoder input identity changed"
    );
    Ok((
        json!({"decisions":decisions,"projected_count":42,"excluded_count":18,"kind_counts":counts,"exclusion_counts":excluded,
        "sessions":sessions,"jobs":jobs,"encoder_profile":profile.metadata(),"embedding_work_first_batch_count":work.len(),"embedding_work_total_coverage":false,
        "live_memory_evidence_rechecked":42,"persistent_preference_coverage":0,"measured_hint_effectiveness_coverage":0}),
        inputs,
    ))
}
fn save_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(bytes)?;
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 4 && args[0] == "--root" && args[2] == "--output",
        "usage: typed-projection-probe --root ROOT --output NEW_DIRECTORY"
    );
    let root = PathBuf::from(&args[1]).canonicalize()?;
    let output = PathBuf::from(&args[3]);
    ensure!(!output.exists(), "Preserve earlier projection outputs");
    let frozen = load(&root)?;
    let (projection, inputs) = project(&frozen)?;
    let mut encoded = vec![];
    for input in &inputs {
        serde_json::to_writer(&mut encoded, input)?;
        encoded.push(b'\n');
    }
    let report = json!({"status":"projection_completed_no_encoding","method":serde_json::from_str::<Value>(METHOD)?,"method_sha256":hash(METHOD.as_bytes()),
        "probe_sha256":hash(include_bytes!("typed_projection_probe.rs")),"fixture_manifest_sha256":FIXTURE_SHA,"original_fixture_files":frozen.files,
        "projection":projection,"encoder_inputs":{"file":"encoder_inputs.jsonl","sha256":hash(&encoded),"records":inputs.len()},
        "models_executed":false,"ranking_executed":false,"production_changed":false,"devices_used":false,"database":"owned disposable synthetic database only"});
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(&output)?;
    save_new(&output.join("encoder_inputs.jsonl"), &encoded)?;
    save_new(
        &output.join("report.json"),
        &serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "{{\"status\":\"projection_completed_no_encoding\",\"projected\":42,\"excluded\":18,\"encoder_inputs\":234}}"
    );
    Ok(())
}

#[cfg(test)]
#[path = "typed_projection_tests.rs"]
mod tests;
