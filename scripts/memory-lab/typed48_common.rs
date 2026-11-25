//! Shared only by the new typed-48 diagnostics; frozen typed-42 code stays intact.
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
    path::{Path, PathBuf},
};

pub const FIXTURE_SHA: &str = "858ee969827d54d1687d1a3966e726576a66024f9b462683323732dde94fbb81";
pub const LEGACY_REPORT_SHA: &str =
    "5feb84ddc072b05a8bc9106082bfec096f8f5ec3208403f0ca9c018cca19e426";
pub const LEGACY_INPUTS_SHA: &str =
    "d93cfa39f184e51359837ad6ed67f31688ada1a54a8c8d5f57df447df38e2368";
pub const METHOD: &str = include_str!("typed48_projection_method_v1.json");
const JOB_VERSION: &str = "synthetic-typed48-projection-v1";
const CLOCK_MS: i64 = 1_793_721_600_000;

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    ensure!(
        fs::metadata(path)?.len() < 4 * 1024 * 1024,
        "Fixture exceeds bound"
    );
    let bytes = fs::read(path)?;
    ensure!(
        hash(&bytes) == expected,
        "Frozen input changed: {}",
        path.display()
    );
    Ok(bytes)
}
fn jsonl(bytes: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(bytes)?
        .lines()
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}
pub struct Frozen {
    pub sources: Vec<Value>,
    pub queries: Vec<Value>,
    pub labels: Vec<Value>,
    pub files: Value,
    pub legacy_report: Value,
    pub legacy_inputs: Vec<Value>,
}
pub fn load(root: &Path) -> Result<Frozen> {
    let base = root.join("rust/ai-tutor/tests/fixtures/diagnostic_postfreeze_v2");
    let manifest: Value =
        serde_json::from_slice(&checked(&base.join("manifest.json"), FIXTURE_SHA)?)?;
    let mut data = HashMap::new();
    for name in [
        "sources.jsonl",
        "queries.jsonl",
        "labels.jsonl",
        "scope_controls.jsonl",
    ] {
        data.insert(
            name,
            jsonl(&checked(
                &base.join(name),
                s(&manifest["files"][name], "sha256"),
            )?)?,
        );
    }
    ensure!(
        data["sources.jsonl"].len() == 60
            && data["queries.jsonl"].len() == 96
            && data["labels.jsonl"].len() == 96
            && data["scope_controls.jsonl"].len() == 12,
        "Complete fixture required"
    );
    ensure!(
        data["sources.jsonl"].iter().all(|v| v["version"] == 1
            && v["status"] == "active"
            && v["source_type"] == "simulated"
            && v["namespace"] == "simulator"
            && v["scope"] == "current_activity"
            && v["hardware_verified"] == false),
        "Source boundary changed"
    );
    let baseline = root.join("runtime/goal-baselines/20261104/typed-projection-first");
    Ok(Frozen {
        sources: data.remove("sources.jsonl").unwrap(),
        queries: data.remove("queries.jsonl").unwrap(),
        labels: data.remove("labels.jsonl").unwrap(),
        files: manifest["files"].clone(),
        legacy_report: serde_json::from_slice(&checked(
            &baseline.join("report.json"),
            LEGACY_REPORT_SHA,
        )?)?,
        legacy_inputs: jsonl(&checked(
            &baseline.join("encoder_inputs.jsonl"),
            LEGACY_INPUTS_SHA,
        )?)?,
    })
}

pub fn kind(source: &Value) -> Option<&'static str> {
    match (s(source, "claim_type"), s(source, "event_kind")) {
        ("reported_observation", "observation") => Some("trial_observation"),
        ("task_specific_response", "learner_answer") => Some("concept_evidence"),
        ("accepted_plan", "plan") if source["accepted"] == true => {
            Some("accepted_pending_experiment")
        }
        ("explicit_correction", "correction") => Some("correction"),
        ("stated_goal", "learner_statement") => Some("learning_goal"),
        ("condition_limit", "learner_statement") if source["support_level"] == "unknown" => {
            Some("context_constraint")
        }
        _ => None,
    }
}
fn exclusion(source: &Value) -> &'static str {
    match s(source, "claim_type") {
        "unaccepted_plan" => "proposal_not_accepted",
        "temporary_request" => "temporary_request_not_persistent_preference",
        _ => "unsupported_source_type_or_support",
    }
}
fn prefix(kind: &str) -> &'static str {
    match kind {
        "trial_observation" => "据报告的尝试与观察",
        "concept_evidence" => "本次回答证据，不代表已掌握",
        "accepted_pending_experiment" => "已接受、尚待完成的实验",
        "correction" => "明确更正",
        "learning_goal" => "明确表达的学习目标",
        "context_constraint" => "特定任务的条件与推断边界",
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
pub struct Projected {
    pub store: Store,
    _directory: OwnedDirectory,
    pub learner_id: String,
    pub decisions: Vec<Value>,
    pub inputs: Vec<Value>,
    pub summary: Value,
}
impl Projected {
    pub fn verify_live(&self) -> Result<()> {
        ensure!(
            self.store.memories(&self.learner_id)?.len() == 48,
            "Live memory count changed"
        );
        for d in self.decisions.iter().filter(|d| d["projected"] == true) {
            ensure!(
                self.store
                    .memory_evidence(&self.learner_id, s(d, "memory_id"))?
                    == d["memory_evidence"],
                "Live evidence changed"
            );
        }
        Ok(())
    }
}

fn candidate(source: &Value, event: &Value, kind: &str) -> Value {
    let id = s(event, "event_id");
    let mut v = json!({"kind":kind,"claim_type":source["claim_type"],"content":source["text"],
        "scope":"current_activity","support_level":source["support_level"],"source_event_ids":[id],
        "source_event_versions":{},"source_quotes":[{"event_id":id,"quote":source["text"]}],
        "importance":{"goal_relevance":0,"continuity":0,"novel_change":0,"explicit_request":0}});
    v["source_event_versions"][id] = event["event_version"].clone();
    v
}

pub fn complete_set_available(alternatives: &Value, available: &HashSet<String>) -> bool {
    alternatives.as_array().is_some_and(|sets| {
        sets.iter().any(|set| {
            set.as_array().is_some_and(|ids| {
                !ids.is_empty()
                    && ids
                        .iter()
                        .all(|id| id.as_str().is_some_and(|id| available.contains(id)))
            })
        })
    })
}
fn coverage(frozen: &Frozen, available: &HashSet<String>) -> Result<Value> {
    let old: HashSet<String> = frozen.legacy_report["projection"]["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["projected"] == true)
        .map(|d| s(d, "source_id").to_owned())
        .collect();
    let mut cases = vec![];
    for query in &frozen.queries {
        let label = frozen
            .labels
            .iter()
            .find(|v| v["query_id"] == query["id"])
            .ok_or_else(|| anyhow!("Query label missing"))?;
        let sets = &label["sufficient_evidence_sets"];
        let retained = |pool: &HashSet<String>| -> Vec<Value> {
            sets.as_array()
                .unwrap()
                .iter()
                .filter(|set| complete_set_available(&json!([set]), pool))
                .cloned()
                .collect()
        };
        cases.push(json!({"query_id":query["id"],"mode":query["mode"],"answerable":label["answerable"],
            "original_sufficient_evidence_sets":sets,"legacy42_complete_sets":retained(&old),
            "typed48_complete_sets":retained(available),"legacy42_available":complete_set_available(sets,&old),
            "typed48_available":complete_set_available(sets,available)}));
    }
    let strata = |mode: Option<&str>| -> Value {
        let rows: Vec<_> = cases
            .iter()
            .filter(|c| {
                mode.is_none_or(|mode| {
                    if mode == "cross_language" {
                        ["zh_to_en", "en_to_zh"].contains(&s(c, "mode"))
                    } else {
                        s(c, "mode") == mode
                    }
                })
            })
            .collect();
        json!({"original_positive":rows.iter().filter(|c| c["answerable"]==true).count(),
            "original_negative":rows.iter().filter(|c| c["answerable"]==false).count(),
            "legacy42_available_positive":rows.iter().filter(|c| c["answerable"]==true && c["legacy42_available"]==true).count(),
            "typed48_available_positive":rows.iter().filter(|c| c["answerable"]==true && c["typed48_available"]==true).count(),
            "legacy42_available_alternatives":rows.iter().map(|c| c["legacy42_complete_sets"].as_array().unwrap().len()).sum::<usize>(),
            "typed48_available_alternatives":rows.iter().map(|c| c["typed48_complete_sets"].as_array().unwrap().len()).sum::<usize>()})
    };
    Ok(
        json!({"meaning":"source availability only, not retrieval success or abstention", "overall":strata(None),
        "cross_language":strata(Some("cross_language")),"mixed_multi":strata(Some("mixed_multi")),"cases":cases}),
    )
}

pub fn project(frozen: &Frozen) -> Result<Projected> {
    let directory = std::env::temp_dir().join(uid("ai-tutor-typed48"));
    fs::create_dir(&directory)?;
    let owned = OwnedDirectory(directory.clone());
    let store = Store::open(directory.join("projection.sqlite3"), "simulator")?;
    let learner_id = s(
        &store.create_learner("SYNTHETIC_TYPED48", "zh", "guided")?,
        "learner_id",
    )
    .to_owned();
    let mut projected = Projected {
        store,
        _directory: owned,
        learner_id,
        decisions: vec![],
        inputs: vec![],
        summary: Value::Null,
    };
    let mut packs = vec![];
    for source in &frozen.sources {
        if !packs.contains(&s(source, "pack_id")) {
            packs.push(s(source, "pack_id"));
        }
    }
    ensure!(packs.len() == 6, "Expected six topics");
    let mut clock = CLOCK_MS;
    let mut jobs = vec![];
    let mut sessions = vec![];
    for pack in packs {
        let sid = s(
            &projected.store.create_session(&projected.learner_id)?,
            "session_id",
        )
        .to_owned();
        let sources: Vec<_> = frozen
            .sources
            .iter()
            .filter(|v| s(v, "pack_id") == pack)
            .collect();
        let mut batchable = vec![];
        for source in &sources {
            let mut content = json!({"text":source["text"],"support_level":source["support_level"],
                "synthetic_fixture":true,"fixture_source_id":source["id"]});
            if !source["accepted"].is_null() {
                content["accepted"] = source["accepted"].clone();
            }
            let event = projected.store.record_education_event(
                &projected.learner_id,
                &sid,
                "simulated",
                s(source, "event_kind"),
                &content,
            )?;
            ensure!(
                event["content"] == content
                    && event["event_kind"] == source["event_kind"]
                    && event["event_version"] == source["version"]
                    && event["source_type"] == "simulated",
                "Import changed source"
            );
            let target = kind(source);
            let index = projected.decisions.len();
            projected.decisions.push(json!({"source_id":source["id"],"original_source":source,
                "original_text_sha256":hash(s(source,"text").as_bytes()),"event_id":event["event_id"],
                "live_imported_event":event,"projected":false,"projection_kind":target,
                "exclusion_reason":if target.is_none(){Some(exclusion(source))}else{None}}));
            if let Some(target) = target {
                batchable.push((index, *source, event, target));
            }
        }
        ensure!(
            sources.len() == 10 && batchable.len() == 8,
            "Topic must project eight sources"
        );
        for batch in batchable.chunks(5) {
            let ids: Vec<_> = batch
                .iter()
                .map(|(_, _, event, _)| s(event, "event_id").to_owned())
                .collect();
            let limits = MemoryJobLimits::default();
            let job = projected.store.enqueue_memory_job(
                &projected.learner_id,
                &sid,
                &ids,
                JOB_VERSION,
                &limits,
                clock,
            )?;
            let claim = projected
                .store
                .claim_memory_job("typed48", &limits, clock)?
                .ok_or_else(|| anyhow!("No synthetic job"))?;
            ensure!(claim["job"]["job_id"] == job["job_id"], "Wrong job");
            let proposals: Vec<_> = batch
                .iter()
                .map(|(_, source, event, target)| candidate(source, event, target))
                .collect();
            let staged = projected.store.stage_memory_candidates(
                &claim["receipt"],
                &proposals,
                clock + 1,
            )?;
            ensure!(staged.len() == batch.len(), "Proposal count changed");
            for ((index, source, event, target), staged) in batch.iter().zip(&staged) {
                let evidence = projected.store.apply_memory_candidate(
                    &projected.learner_id,
                    s(staged, "candidate_id"),
                    &[*target],
                    clock + 2,
                )?;
                let text = format!("{}：{}", prefix(target), s(source, "text"));
                let memory_kind = match *target {
                    "trial_observation" => "reported_progress",
                    "accepted_pending_experiment" => "pending_experiment",
                    other => other,
                };
                ensure!(
                    evidence["namespace"] == "simulator"
                        && evidence["memory"]["content"] == text
                        && evidence["memory"]["kind"] == memory_kind
                        && evidence["memory"]["scope"] == "current_activity"
                        && evidence["memory"]["activity_scope"] == "modular_rover"
                        && evidence["memory"]["status"] == "active"
                        && evidence["memory"]["learner_id"] == projected.learner_id
                        && evidence["memory"]["source_type"] == "simulated"
                        && evidence["memory"]["epoch"] == 0,
                    "Actual memory differs from declared projection"
                );
                ensure!(
                    evidence["sources"] == json!([event])
                        && evidence["source_links"]
                            == json!([{"event_id":event["event_id"],"event_version":1}])
                        && evidence["memory"]["source_event_ids"] == json!([event["event_id"]]),
                    "Source provenance changed"
                );
                let mut metadata = staged["payload"].clone();
                metadata.as_object_mut().unwrap().retain(|key, _| {
                    [
                        "kind",
                        "claim_type",
                        "support_level",
                        "importance",
                        "source_grade",
                        "source_count",
                    ]
                    .contains(&key.as_str())
                });
                ensure!(
                    evidence["education_metadata"] == metadata
                        && metadata["source_grade"] == "simulated"
                        && metadata["source_count"] == 1
                        && metadata["support_level"] == source["support_level"],
                    "Metadata changed"
                );
                let row = &mut projected.decisions[*index];
                row["projected"] = json!(true);
                row["memory_id"] = evidence["memory"]["memory_id"].clone();
                row["encoding_text"] = json!(text);
                row["encoding_text_sha256"] = json!(hash(text.as_bytes()));
                row["memory_evidence"] = evidence;
            }
            let status = projected
                .store
                .memory_job_status(&projected.learner_id, s(&job, "job_id"))?;
            ensure!(
                status["status"] == "applied"
                    && status["attempt_receipts"][0]["started_at_ms"].is_null(),
                "Synthetic job receipt changed"
            );
            jobs.push(json!({"job_id":job["job_id"],"candidate_count":batch.len(),"status":"applied","model_call_started":false}));
            clock += 10;
        }
        let session = projected.store.session(&sid)?;
        ensure!(
            session["state"]["reported_result"] == ""
                && session["state"]["pending_step"] == ""
                && session["state"]["attempts"] == json!([]),
            "Projection invented progress"
        );
        sessions
            .push(json!({"pack_id":pack,"store_session_id":sid,"progress_state_unchanged":true}));
    }
    let mut counts = BTreeMap::<String, usize>::new();
    let mut exclusions = BTreeMap::<String, usize>::new();
    for d in &projected.decisions {
        if d["projected"] == true {
            *counts
                .entry(s(d, "projection_kind").to_owned())
                .or_default() += 1;
            projected.inputs.push(json!({"key":format!("source:{}",s(d,"source_id")),"input_kind":"projected_memory","source_ids":[d["source_id"]],"text":d["encoding_text"],"text_sha256":d["encoding_text_sha256"]}));
        } else {
            *exclusions
                .entry(s(d, "exclusion_reason").to_owned())
                .or_default() += 1;
        }
    }
    ensure!(
        projected.inputs.len() == 48
            && projected.decisions.len() == 60
            && counts["context_constraint"] == 6,
        "Projection count changed"
    );
    for condition in ["current_query", "query_plus_goal"] {
        for query in &frozen.queries {
            let text = if condition == "current_query" {
                s(query, "text").to_owned()
            } else {
                format!("{} {} ", s(query, "text"), s(query, "goal"))
            };
            projected.inputs.push(json!({"key":format!("{condition}:{}",s(query,"id")),"input_kind":condition,"query_id":query["id"],"text_sha256":hash(text.as_bytes()),"text":text}));
        }
    }
    let indexed: HashMap<_, _> = projected.inputs.iter().map(|v| (s(v, "key"), v)).collect();
    ensure!(
        indexed.len() == 240 && projected.inputs.len() == 240 && frozen.legacy_inputs.len() == 234,
        "Input count changed"
    );
    for old in &frozen.legacy_inputs {
        ensure!(
            indexed.get(s(old, "key")) == Some(&old),
            "Existing encoded input changed"
        );
    }
    let legacy = frozen.legacy_report["projection"]["decisions"]
        .as_array()
        .unwrap();
    for old in legacy.iter().filter(|v| v["projected"] == true) {
        let new = projected
            .decisions
            .iter()
            .find(|v| v["source_id"] == old["source_id"])
            .ok_or_else(|| anyhow!("Legacy source missing"))?;
        ensure!(
            new["original_source"] == old["original_source"]
                && new["encoding_text"] == old["encoding_text"]
                && new["memory_evidence"]["education_metadata"]
                    == old["memory_evidence"]["education_metadata"],
            "Existing projection changed"
        );
    }
    let available: HashSet<_> = projected
        .decisions
        .iter()
        .filter(|v| v["projected"] == true)
        .map(|v| s(v, "source_id").to_owned())
        .collect();
    let profile = EmbeddingProfile::BgeM3NativeFp32;
    let work = projected
        .store
        .embedding_work(&projected.learner_id, &profile.model_key(), 32)?;
    ensure!(work.len() == 32, "Expected first bounded work batch");
    for snapshot in &work {
        let d = projected
            .decisions
            .iter()
            .find(|v| v["memory_id"] == snapshot.memory_id)
            .ok_or_else(|| anyhow!("Work source missing"))?;
        ensure!(
            snapshot.dimension == 1024
                && snapshot.model_key == profile.model_key()
                && snapshot.content == s(d, "encoding_text"),
            "Snapshot text/profile changed"
        );
    }
    projected.summary = json!({"projected_count":48,"excluded_count":12,"kind_counts":counts,"exclusion_counts":exclusions,
        "sessions":sessions,"jobs":jobs,"encoder_profile":profile.metadata(),"live_memory_evidence_rechecked":48,
        "legacy42_text_and_metadata_equal":42,"legacy192_query_inputs_equal":192,
        "embedding_work_first_batch_count":32,"embedding_work_total_coverage":false,
        "availability":coverage(frozen,&available)?,"original_scope_controls_count":12,"scope_controls_executed":false,
        "persistent_preference_coverage":0,"measured_hint_effectiveness_coverage":0});
    projected.verify_live()?;
    Ok(projected)
}
