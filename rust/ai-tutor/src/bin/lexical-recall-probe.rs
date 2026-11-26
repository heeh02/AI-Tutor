//! Explicit synthetic shadow measurement. Never changes Store::recall or context.
#[path = "../memory_lexical.rs"]
mod memory_lexical;

use ai_tutor::store::{Store, s, strings, uid};
use anyhow::{Result, anyhow, ensure};
use memory_lexical::{Candidate, Document, Feature, Ranking};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const FIXTURE: &str = include_str!("../../tests/fixtures/e5_education_recall_v2.json");
const FIXTURE_SHA: &str = "0392be52062844b0dd0e558c500d5afe9c4f38b1cad923a6092e5fda125f89d7";
const METHOD: &str = include_str!("../../tests/fixtures/lexical_shadow_method_v1.json");
const METHOD_SHA: &str = "5296ca5b5116e213b020b9aabe68f6d524926e8ba4b69649bf6e706fce65e18c";
const TOP_K: usize = 5;
const CONTENT_BUDGET: usize = 2400;
const GOAL: &str = "弄明白停止距离和车速的关系";

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

struct TemporaryDatabase(PathBuf);
impl Drop for TemporaryDatabase {
    fn drop(&mut self) {
        for name in ["shadow.sqlite3", "shadow.sqlite3-wal", "shadow.sqlite3-shm"] {
            let _ = fs::remove_file(self.0.join(name));
        }
        let _ = fs::remove_dir(&self.0);
    }
}

struct SourceDocument {
    id: String,
    memory: Value,
    source_kinds: Vec<String>,
}

fn ranking_metrics(rank: &[String], relevant: &[String]) -> Value {
    if relevant.is_empty() {
        return json!({"positive_recall":"not_applicable","candidate_returned":!rank.is_empty(),
            "abstention":"not_calibrated; an empty feature intersection is not proof of absent evidence"});
    }
    let first = rank.iter().take(TOP_K).position(|id| relevant.contains(id));
    let found = relevant
        .iter()
        .filter(|id| rank.iter().take(3).any(|candidate| candidate == *id))
        .count();
    json!({"hit_at_1":rank.first().is_some_and(|id| relevant.contains(id)),"hit_at_3":found>0,
        "recall_at_3":found as f64/relevant.len() as f64,"all_required_at_3":found==relevant.len(),
        "reciprocal_rank_at_5":first.map_or(0.0, |rank| 1.0/(rank+1) as f64)})
}

fn aggregate(rows: &[Value], method: &str) -> Value {
    let positive: Vec<_> = rows
        .iter()
        .filter(|row| !row["relevant"].as_array().unwrap().is_empty())
        .collect();
    let negatives: Vec<_> = rows
        .iter()
        .filter(|row| row["relevant"].as_array().unwrap().is_empty())
        .collect();
    json!({"positive_cases":positive.len(),"negative_cases":negatives.len(),
        "hit_at_1":positive.iter().filter(|row| row[method]["metrics"]["hit_at_1"]==true).count(),
        "hit_at_3":positive.iter().filter(|row| row[method]["metrics"]["hit_at_3"]==true).count(),
        "mean_recall_at_3":positive.iter().map(|row| row[method]["metrics"]["recall_at_3"].as_f64().unwrap()).sum::<f64>()/positive.len() as f64,
        "all_required_at_3":positive.iter().filter(|row| row[method]["metrics"]["all_required_at_3"]==true).count(),
        "mrr_at_5":positive.iter().map(|row| row[method]["metrics"]["reciprocal_rank_at_5"].as_f64().unwrap()).sum::<f64>()/positive.len() as f64,
        "negative_cases_with_candidates":negatives.iter().filter(|row| row[method]["metrics"]["candidate_returned"]==true).count()})
}

fn selected_ids(ranking: &Ranking, lengths: &BTreeMap<String, usize>) -> Vec<String> {
    let mut result = Vec::new();
    let mut remaining = CONTENT_BUDGET;
    for candidate in &ranking.candidates {
        if result.len() == TOP_K {
            break;
        }
        let size = lengths[&candidate.id];
        if size <= remaining {
            remaining -= size;
            result.push(candidate.id.clone());
        }
    }
    result
}

fn origin(feature: &Feature, raw: &BTreeSet<Feature>, goal: &BTreeSet<Feature>) -> Value {
    json!({"in_current_query":raw.contains(feature),"in_goal":goal.contains(feature),
        "goal_only":!raw.contains(feature) && goal.contains(feature)})
}

fn explain(
    candidate: &Candidate,
    document: &SourceDocument,
    raw: &BTreeSet<Feature>,
    goal: &BTreeSet<Feature>,
    selected: bool,
) -> Value {
    let raw_count = candidate
        .matched
        .iter()
        .filter(|item| raw.contains(&item.feature))
        .count();
    let goal_only_count = candidate
        .matched
        .iter()
        .filter(|item| !raw.contains(&item.feature) && goal.contains(&item.feature))
        .count();
    json!({"id":candidate.id,"score":candidate.score,"selected_within_budget":selected,
        "document_feature_count":candidate.document_feature_count,"content":document.memory["content"],
        "kind":document.memory["kind"],"scope":document.memory["scope"],"source_type":document.memory["source_type"],
        "source_event_kinds":document.source_kinds,"source_evidence_checked":true,
        "relevance_status":candidate.relevance_status,"current_query_match_count":raw_count,
        "goal_only_match_count":goal_only_count,"only_goal_matched":raw_count==0 && goal_only_count>0,
        "matched":candidate.matched.iter().map(|item| json!({"feature":item.feature,
            "document_frequency":item.document_frequency,"idf":item.idf,"origin":origin(&item.feature,raw,goal)})).collect::<Vec<_>>()})
}

fn create_documents(
    store: &Store,
    fixture: &Value,
) -> Result<(String, Vec<SourceDocument>, HashMap<String, String>)> {
    let learner = store.create_learner("SYNTHETIC_LEXICAL_SHADOW", "en", "guided")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    let mut documents = Vec::new();
    let mut memory_ids = HashMap::new();
    for id in strings(&fixture["candidate_pool"]) {
        let document = fixture["documents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .ok_or_else(|| anyhow!("Frozen source document missing"))?;
        let event_kind = document["event_kind"].as_str().unwrap_or("observation");
        let kind = document["memory_kind"]
            .as_str()
            .unwrap_or("reported_progress");
        let mut content = json!({"text":document["text"],"synthetic_fixture":true});
        if event_kind == "plan" {
            content["accepted"] = json!(true);
        }
        let event = store.record_education_event(lid, sid, "simulated", event_kind, &content)?;
        let mid = store.add_memory(lid,&json!({"kind":kind,"scope":"current_activity","content":document["text"],
            "source_event_ids":[event["event_id"]],"source_event_versions":{s(&event,"event_id"):event["event_version"]}}),0)?
            .ok_or_else(|| anyhow!("Frozen diagnostic memory was rejected"))?;
        let evidence = store.memory_evidence(lid, &mid)?;
        let memory = evidence["memory"].clone();
        ensure!(
            memory["learner_id"] == lid
                && memory["source_type"] == "simulated"
                && memory["activity_scope"] == "modular_rover"
                && memory["scope"] == "current_activity"
                && memory["status"] == "active"
                && evidence["namespace"] == "simulator",
            "Synthetic scope/evidence mismatch"
        );
        let source_kinds = evidence["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|source| s(source, "event_kind").into())
            .collect();
        memory_ids.insert(mid, id.clone());
        documents.push(SourceDocument {
            id,
            memory,
            source_kinds,
        });
    }
    Ok((lid.into(), documents, memory_ids))
}

fn validate_frozen() -> Result<(Value, Value)> {
    ensure!(
        hash(FIXTURE.as_bytes()) == FIXTURE_SHA && hash(METHOD.as_bytes()) == METHOD_SHA,
        "Frozen fixture or method changed"
    );
    let fixture: Value = serde_json::from_str(FIXTURE)?;
    let method: Value = serde_json::from_str(METHOD)?;
    ensure!(
        fixture["synthetic_only"] == true
            && fixture["documents"].as_array().unwrap().len() == 16
            && fixture["cases"].as_array().unwrap().len() == 25,
        "Only the fixed v2 diagnostic is supported"
    );
    ensure!(
        method["method_version"] == memory_lexical::METHOD_VERSION
            && strings(&method["english_function_words"])
                .iter()
                .map(String::as_str)
                .eq(memory_lexical::FUNCTION_WORDS.iter().copied()),
        "Compiled feature method does not match preregistration"
    );
    let goal_source = fixture["documents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "zh_goal")
        .unwrap();
    ensure!(
        s(goal_source, "text").contains(GOAL) && method["conditions"][1]["goal"] == GOAL,
        "Synthetic goal must remain an unchanged substring of the declared goal source"
    );
    Ok((fixture, method))
}

fn evaluate(root: &Path) -> Result<Value> {
    let (fixture, method) = validate_frozen()?;
    let first_path = root.join("runtime/goal-baselines/20261104/e5-education-first/report.json");
    let first_bytes = fs::read(&first_path)?;
    let first: Value = serde_json::from_slice(&first_bytes)?;
    ensure!(
        first["fixture_sha256"] == FIXTURE_SHA,
        "Historical lexical result used a different fixture"
    );
    let path = std::env::temp_dir().join(uid("ai-tutor-lexical-shadow"));
    fs::create_dir(&path)?;
    let _cleanup = TemporaryDatabase(path.clone());
    let store = Store::open(path.join("shadow.sqlite3"), "simulator")?;
    let mut raw_rows = Vec::new();
    let mut goal_rows = Vec::new();
    for case in fixture["cases"].as_array().unwrap() {
        let (lid, documents, memory_ids) = create_documents(&store, &fixture)?;
        let shadow_documents: Vec<_> = documents
            .iter()
            .map(|row| Document {
                id: &row.id,
                text: s(&row.memory, "content"),
            })
            .collect();
        let lengths: BTreeMap<_, _> = documents
            .iter()
            .map(|row| (row.id.clone(), s(&row.memory, "content").chars().count()))
            .collect();
        let current_query = s(case, "query");
        let raw_features = memory_lexical::features(current_query);
        let relevant = strings(&case["relevant"]);
        let original = first["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == case["id"])
            .ok_or_else(|| anyhow!("Historical query missing"))?;
        ensure!(
            original["query"] == case["query"] && original["relevant"] == case["relevant"],
            "Historical question/labels changed"
        );
        for (condition, goal) in [("current_query", ""), ("query_plus_goal", GOAL)] {
            // Mirrors context.rs's nonempty-goal branch; full build_context is
            // deliberately not invoked and no task state is committed.
            let query = if goal.is_empty() {
                current_query.to_owned()
            } else {
                format!("{current_query} {goal} ")
            };
            let goal_features = memory_lexical::features(goal);
            let legacy: Vec<_> = store
                .recall(&lid, &query, TOP_K)?
                .iter()
                .map(|memory| memory_ids[s(memory, "memory_id")].clone())
                .collect();
            let ranking = memory_lexical::rank(&query, &shadow_documents);
            let selected = selected_ids(&ranking, &lengths);
            let explanations: Vec<_> = ranking
                .candidates
                .iter()
                .map(|candidate| {
                    let document = documents.iter().find(|row| row.id == candidate.id).unwrap();
                    explain(
                        candidate,
                        document,
                        &raw_features,
                        &goal_features,
                        selected.contains(&candidate.id),
                    )
                })
                .collect();
            let only_goal_count = explanations
                .iter()
                .filter(|row| {
                    row["selected_within_budget"] == true && row["only_goal_matched"] == true
                })
                .count();
            let mut row = json!({"id":case["id"],"group_unchanged":case["group"],"relevant":relevant,
                "current_query":current_query,"effective_query":query,"goal":goal,
                "legacy_store":{"ranked_ids":legacy,"metrics":ranking_metrics(&legacy,&relevant),
                    "implementation":"unmodified Store::recall; character bigram + pending bonus + activity-name bypass",
                    "tie_break":"updated_at descending, then existing active-memory order"},
                "shadow":{"ranked_ids":selected,"metrics":ranking_metrics(&selected,&relevant),
                    "eligible_document_count":ranking.eligible_document_count,"query_features":ranking.query_features,
                    "no_overlap_document_count":ranking.no_overlap_document_count,"all_scored_candidates":explanations,
                    "selected_goal_only_candidates":only_goal_count,"relevance_status":"candidate_requires_relevance_review",
                    "empty_result_is_calibrated_abstention":false}});
            if condition == "current_query" {
                let historic_ids = strings(&original["lexical"]["ranked_ids"]);
                row["historical_first_lexical_verbatim"] = original["lexical"].clone();
                row["historical_first_lexical"] = json!({"ranked_ids":historic_ids,"metrics":ranking_metrics(&historic_ids,&relevant)});
                row["fresh_legacy_order_matches_first"] = json!(legacy == historic_ids);
                raw_rows.push(row);
            } else {
                goal_rows.push(row);
            }
        }
    }
    let raw_aggregate = json!({"legacy_store":aggregate(&raw_rows,"legacy_store"),"shadow":aggregate(&raw_rows,"shadow"),
        "historical_first_lexical":aggregate(&raw_rows,"historical_first_lexical")});
    let goal_aggregate = json!({"legacy_store":aggregate(&goal_rows,"legacy_store"),"shadow":aggregate(&goal_rows,"shadow"),
        "selected_goal_only_candidates":goal_rows.iter().map(|row| row["shadow"]["selected_goal_only_candidates"].as_u64().unwrap()).sum::<u64>()});
    Ok(
        json!({"schema_version":1,"probe":"offline_lexical_shadow","method":method,"method_sha256":METHOD_SHA,
        "fixture_sha256":FIXTURE_SHA,"historical_first_report_sha256":hash(&first_bytes),
        "compiled_shadow_source_sha256":hash(include_bytes!("../memory_lexical.rs")),
        "compiled_store_source_sha256":hash(include_bytes!("../store.rs")),
        "compiled_context_source_sha256":hash(include_bytes!("../context.rs")),
        "top_k":TOP_K,"content_budget_chars":CONTENT_BUDGET,"conditions":[
            {"id":"current_query","cases":raw_rows,"aggregate":raw_aggregate},
            {"id":"query_plus_goal","goal":GOAL,"goal_source":"zh_goal","cases":goal_rows,"aggregate":goal_aggregate}],
        "case_count":50,"synthetic_only":true,"temporary_database_only":true,
        "real_learner_database_opened":false,"full_build_context_invoked":false,
        "models_called":false,"devices_used":false,"downloads":false,"production_recall_changed":false,
        "semantic_quality_pass":false,"acceptance_threshold_calibrated":false,
        "limitations":["v2 is an already observed diagnostic set, not holdout; no tuning after viewing these results",
            "The goal condition mirrors the real context query string, with a fixed synthetic goal; it does not test the entire context builder",
            "Original question relevance labels remain unchanged in the goal condition; goal matches are explained separately",
            "The same actual Store supplies live typed source/scope checks; source grade and memory kind do not contribute to the shadow score",
            "Temporary drawing request remains the original diagnostic topical report, not a persistent preference",
            "q01's original group label is retained; it is English-to-English, not cross-language",
            "No synonym, translation, numeric-unit interpretation or semantic acceptance is introduced"]}),
    )
}

fn main() -> Result<()> {
    let mut root = std::env::current_dir()?;
    let mut output = None;
    let mut explicit = false;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--local-synthetic" => explicit = true,
            "--root" => root = PathBuf::from(args.next().ok_or_else(|| anyhow!("Missing root"))?),
            "--output" => {
                output = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow!("Missing output"))?,
                ))
            }
            "--help" | "-h" => {
                println!(
                    "lexical-recall-probe --local-synthetic --output <new-report.json> [--root PATH]\nFrozen v2, two query conditions; isolated Store baseline and pure lexical shadow; no model or devices."
                );
                return Ok(());
            }
            _ => return Err(anyhow!("Unknown argument")),
        }
    }
    ensure!(explicit, "Explicit --local-synthetic is required");
    let output = output.ok_or_else(|| anyhow!("--output is required"))?;
    ensure!(
        !output.exists(),
        "Use a new output path to preserve evidence"
    );
    let report = evaluate(&root)?;
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    file.write_all(&serde_json::to_vec_pretty(&report)?)?;
    println!(
        "{}",
        json!({"report_written":true,"case_count":50,"models_called":false,"production_recall_changed":false})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frozen_method_and_original_question_labels_remain_intact() -> Result<()> {
        let (fixture, _) = validate_frozen()?;
        assert_eq!(
            fixture["cases"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["relevant"] == json!([]))
                .count(),
            4
        );
        Ok(())
    }
    #[test]
    fn source_checked_fixture_keeps_plan_and_temporary_request_types() -> Result<()> {
        let (fixture, _) = validate_frozen()?;
        let path = std::env::temp_dir().join(uid("ai-tutor-lexical-shadow-test"));
        fs::create_dir(&path)?;
        let _cleanup = TemporaryDatabase(path.clone());
        let store = Store::open(path.join("shadow.sqlite3"), "simulator")?;
        let (_, documents, _) = create_documents(&store, &fixture)?;
        assert_eq!(documents.len(), 16);
        for id in ["zh_plan_wall", "en_plan_speed"] {
            let row = documents.iter().find(|row| row.id == id).unwrap();
            assert_eq!(row.memory["kind"], "pending_experiment");
            assert_eq!(row.source_kinds, ["plan"]);
        }
        let temporary = documents
            .iter()
            .find(|row| row.id == "en_help_report")
            .unwrap();
        assert_eq!(temporary.memory["kind"], "reported_progress");
        assert_eq!(temporary.source_kinds, ["learner_statement"]);
        assert!(
            documents
                .iter()
                .all(|row| row.memory["source_type"] == "simulated")
        );
        Ok(())
    }
    #[test]
    fn multi_source_coverage_and_mrr_use_the_same_five_candidate_window() {
        let rank = vec!["one".into(), "other".into()];
        let partial = ranking_metrics(&rank, &["one".into(), "two".into()]);
        assert_eq!(partial["recall_at_3"], 0.5);
        assert_eq!(partial["all_required_at_3"], false);
        let six: Vec<_> = (0..6).map(|n| n.to_string()).collect();
        assert_eq!(
            ranking_metrics(&six, &["5".into()])["reciprocal_rank_at_5"],
            0.0
        );
        assert_eq!(ranking_metrics(&[], &[])["candidate_returned"], false);
    }
    #[test]
    fn goal_only_match_is_explained_without_becoming_a_question_match() {
        let raw = memory_lexical::features("magma");
        let goal = memory_lexical::features("velocity");
        let feature = goal.first().unwrap();
        assert_eq!(origin(feature, &raw, &goal)["goal_only"], true);
        assert_eq!(origin(feature, &raw, &goal)["in_current_query"], false);
    }
    #[test]
    fn selection_skips_whole_oversized_records_without_truncation() {
        let ranking = memory_lexical::rank(
            "alpha",
            &[
                Document {
                    id: "a",
                    text: "alpha",
                },
                Document {
                    id: "b",
                    text: "alpha",
                },
            ],
        );
        let lengths = BTreeMap::from([("a".into(), CONTENT_BUDGET + 1), ("b".into(), 10)]);
        assert_eq!(selected_ids(&ranking, &lengths), ["b"]);
    }
}
