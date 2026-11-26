//! Explicit synthetic educational-memory evaluation. Each invocation makes at
//! most one budgeted model call, in a fresh simulator DB, and never applies it.
use ai_tutor::{
    kimi::{KimiProvider, memory_contract},
    memory_worker::{MemoryConsolidator, MemoryWorker, MemoryWorkerMode, MemoryWorkerPolicy},
    store::{MemoryJobLimits, Store, s, strings, uid},
};
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Mutex,
};

const FIXTURE: &str = include_str!("../../tests/fixtures/memory_education_probe_v1.json");
const BOUNDARIES: &str = include_str!("../../tests/fixtures/memory_education_boundaries_v1.json");
const HELP: &str = "memory-education-probe --real-synthetic [--root PATH] --language zh|en [--scenario continuity|constraints|preferences_and_support]\nOne synthetic, budgeted shadow call; no learner database, devices or automatic apply.\nJSON includes validated synthetic candidates for manual review. Exit 0 means protocol/structural checks passed, NOT semantic quality or educational coverage passed.";

fn fixture(scenario: &str) -> Result<Value> {
    if scenario == "continuity" {
        return Ok(serde_json::from_str(FIXTURE)?);
    }
    let all: Value = serde_json::from_str(BOUNDARIES)?;
    let mut value = all["scenarios"]
        .get(scenario)
        .cloned()
        .ok_or_else(|| anyhow!("Unknown fixed synthetic scenario"))?;
    value["fixture_version"] = all["fixture_version"].clone();
    value["synthetic_only"] = json!(true);
    Ok(value)
}

fn matches_expectation(candidate: &Value, expectation: &Value) -> bool {
    candidate["kind"] == expectation["kind"]
        && candidate["claim_type"] == expectation["claim_type"]
        && candidate["source_labels"]
            .as_array()
            .is_some_and(|labels| labels.contains(&expectation["source_label"]))
        && ["scope", "support_level"].iter().all(|key| {
            expectation
                .get(*key)
                .is_none_or(|value| candidate[*key] == *value)
        })
}

fn policy() -> MemoryWorkerPolicy {
    MemoryWorkerPolicy {
        mode: MemoryWorkerMode::SyntheticOnly,
        limits: MemoryJobLimits {
            max_pending: 1,
            max_attempts: 1,
            daily_reservations: 1,
            session_reservations: 1,
            ..Default::default()
        },
    }
}
struct TemporaryDirectory(PathBuf);
impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        for name in ["probe.sqlite3", "probe.sqlite3-wal", "probe.sqlite3-shm"] {
            let _ = std::fs::remove_file(self.0.join(name));
        }
        let _ = std::fs::remove_dir(&self.0);
    }
}
struct Probe {
    // Close SQLite before its directory is removed (fields drop in this order).
    stores: Mutex<HashMap<String, Store>>,
    _directory: TemporaryDirectory,
    learner: String,
    job: String,
    sources: Vec<Value>,
    fixture: Value,
}
impl Probe {
    fn new(language: &str, scenario: &str) -> Result<Self> {
        ensure!(
            ["zh", "en"].contains(&language),
            "Language must be zh or en"
        );
        let fixture = fixture(scenario)?;
        let events = fixture["cases"][language]["events"]
            .as_array()
            .ok_or_else(|| anyhow!("Synthetic fixture missing"))?;
        let path = std::env::temp_dir().join(uid("ai-tutor-education-probe"));
        std::fs::create_dir(&path)?;
        let directory = TemporaryDirectory(path.clone());
        let store = Store::open(path.join("probe.sqlite3"), "simulator")?;
        let learner = store.create_learner("SYNTHETIC_EDUCATION_PROBE", language, "guided")?;
        let lid = s(&learner, "learner_id").to_owned();
        let session = store.create_session(&lid)?;
        let sid = s(&session, "session_id");
        let mut sources = vec![];
        for specification in events {
            let mut event = store.record_education_event(
                &lid,
                sid,
                "simulated",
                s(specification, "kind"),
                &specification["content"],
            )?;
            event["fixture_label"] = specification["label"].clone();
            sources.push(event);
        }
        let ids: Vec<_> = sources
            .iter()
            .map(|source| s(source, "event_id").to_owned())
            .collect();
        let job = store.enqueue_memory_job(
            &lid,
            sid,
            &ids,
            memory_contract::PROMPT_VERSION,
            &policy().limits,
            chrono::Utc::now().timestamp_millis(),
        )?;
        Ok(Self {
            stores: Mutex::new(HashMap::from([("simulator".into(), store)])),
            _directory: directory,
            learner: lid,
            job: s(&job, "job_id").to_owned(),
            sources,
            fixture,
        })
    }

    fn report(&self, language: &str, scenario: &str, outcome: Value) -> Result<Value> {
        let stores = self.stores.lock().unwrap();
        let store = &stores["simulator"];
        let status = store.memory_job_status(&self.learner, &self.job)?;
        let saved = status["candidates"]
            .as_array()
            .ok_or_else(|| anyhow!("Candidate status missing"))?;
        let mut candidates = vec![];
        let mut versions_match = true;
        let mut counts_match = true;
        let mut source_kinds_match = true;
        let mut plans_pending = true;
        let mut temporary_scope = true;
        let mut constraint_scope_and_support = true;
        for candidate in saved {
            let payload = &candidate["payload"];
            let ids = strings(&payload["source_event_ids"]);
            let sources: Vec<_> = self
                .sources
                .iter()
                .filter(|source| ids.iter().any(|id| id == s(source, "event_id")))
                .collect();
            let unique: HashSet<_> = ids.iter().collect();
            counts_match &= ids.len() == unique.len()
                && payload["source_count"].as_u64() == Some(unique.len() as u64);
            versions_match &= sources.len() == ids.len()
                && sources.iter().all(|source| {
                    payload["source_event_versions"][s(source, "event_id")]
                        == source["event_version"]
                });
            let allowed: &[&str] = match s(payload, "kind") {
                "learning_goal" | "help_preference" => &["learner_statement", "teacher_setting"],
                "context_constraint" => &["learner_statement"],
                "trial_observation" => &["observation"],
                "concept_evidence" | "revisable_misconception" => &["learner_answer"],
                "accepted_pending_experiment" => &["plan"],
                "correction" => &["correction"],
                _ => &[],
            };
            source_kinds_match &= sources
                .iter()
                .all(|source| allowed.contains(&s(source, "event_kind")));
            if payload["kind"] == "context_constraint" {
                constraint_scope_and_support &= payload["claim_type"] == "condition_limit"
                    && payload["scope"] == "current_activity"
                    && payload["support_level"] == "unknown";
            }
            if sources.iter().any(|source| source["event_kind"] == "plan") {
                plans_pending &= payload["kind"] == "accepted_pending_experiment"
                    && payload["claim_type"] == "accepted_plan";
            }
            if payload["kind"] == "help_preference"
                && sources
                    .iter()
                    .any(|source| source["content"]["preference_scope"] == "temporary")
            {
                temporary_scope &= payload["scope"] == "current_session";
            }
            let labels: Vec<_> = sources
                .iter()
                .map(|source| source["fixture_label"].clone())
                .collect();
            candidates.push(json!({"status":candidate["status"],"kind":payload["kind"],"claim_type":payload["claim_type"],"content":payload["content"],"scope":payload["scope"],
                "support_level":payload["support_level"],"importance":payload["importance"],"source_count":payload["source_count"],"source_grade":payload["source_grade"],
                "source_labels":labels,"source_event_versions":payload["source_event_versions"],"source_quotes":payload["source_quotes"]}));
        }
        let excluded = self.fixture["excluded_source_labels"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let checks = json!({
            "worker_reached_validated_shadow":outcome["status"]=="shadow_staged",
            "at_most_one_budget_reservation":status["attempts"].as_u64().is_some_and(|attempts|attempts<=1),
            "actual_provider_started_receipt":outcome["provider_started"]==true,
            "all_candidates_remain_shadow":saved.iter().all(|candidate|candidate["status"]=="shadow"),
            "no_memory_applied":store.memories(&self.learner)?.is_empty(),
            "source_ids_and_versions_match":versions_match,
            "unique_source_counts_match":counts_match,
            "typed_sources_match_candidate_categories":source_kinds_match,
            "plan_sources_keep_pending_category":plans_pending,
            "temporary_preferences_keep_session_scope":temporary_scope,
            "context_constraints_keep_activity_scope_and_unknown_support":constraint_scope_and_support,
            "excluded_sources_not_promoted_to_candidates":candidates.iter().all(|candidate| candidate["source_labels"].as_array().unwrap().iter().all(|label| !excluded.contains(label))),
            "simulated_source_grade_retained":saved.iter().all(|candidate|candidate["payload"]["source_grade"]=="simulated"),
        });
        let structure_pass = checks
            .as_object()
            .unwrap()
            .values()
            .all(|check| check == true);
        let coverage:Vec<_>=self.fixture["minimal_expectations"].as_array().unwrap().iter().map(|expectation|{
            let observed=candidates.iter().any(|candidate|matches_expectation(candidate, expectation));
            json!({"source_label":expectation["source_label"],"expected_kind":expectation["kind"],"expected_scope":expectation["scope"],"expected_support_level":expectation["support_level"],"expectation":expectation["coverage"],"matching_typed_candidate_observed":observed,"semantic_accuracy":"not_assessed"})
        }).collect();
        Ok(json!({
            "probe":"synthetic_educational_memory_continuity","fixture_version":self.fixture["fixture_version"],"prompt_version":memory_contract::PROMPT_VERSION,"schema_version":memory_contract::SCHEMA_VERSION,"language":language,"scenario":scenario,
            "real_learner_data":false,"temporary_simulator_database":true,"shadow_only":true,"raw_provider_text_retained":false,
            "worker":outcome,"budget_receipts":status["attempt_receipts"],
            "synthetic_sources":self.sources,"validated_candidates":candidates,
            "structure":{"all_passed":structure_pass,"checks":checks},
            "coverage_observations":{"meaning":"category/source coverage only; not a semantic correctness score","items":coverage},
            "manual_semantic_review":{"status":"required_not_performed","criteria":self.fixture["manual_review_criteria"],"semantic_accuracy":null,"meaningful_omissions":null,"plan_misrecorded_as_completed":null},
            "interpretation":"Exit 0 indicates transport and structural checks only. Empty candidates can pass structure while missing all educational coverage. Textual fidelity, uncertainty and plan-as-completion require manual review."
        }))
    }
}

async fn evaluate<P: MemoryConsolidator + ?Sized>(
    provider: &P,
    language: &str,
    scenario: &str,
) -> Result<Value> {
    let probe = Probe::new(language, scenario)?;
    let outcome = MemoryWorker::new()
        .run_once(&probe.stores, provider, "simulator", &policy())
        .await?;
    probe.report(language, scenario, outcome)
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut root = std::env::current_dir()?;
    let mut language = None;
    let mut explicit = false;
    let mut scenario = "continuity".to_owned();
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                return Ok(());
            }
            "--real-synthetic" => explicit = true,
            "--root" => root = PathBuf::from(args.next().ok_or_else(|| anyhow!("Missing root"))?),
            "--language" => language = args.next(),
            "--scenario" => scenario = args.next().ok_or_else(|| anyhow!("Missing scenario"))?,
            _ => bail!("Unknown argument"),
        }
    }
    let language = language.ok_or_else(|| anyhow!("Explicit --language zh|en is required"))?;
    ensure!(
        explicit && ["zh", "en"].contains(&language.as_str()),
        "Explicit synthetic flag and zh|en are required"
    );
    fixture(&scenario)?;
    let provider = KimiProvider::new(root)?;
    ensure!(
        provider.probe().await?["available"] == true,
        "Run native --verify-kimi for the current agent hash first"
    );
    let result = evaluate(&provider, &language, &scenario).await;
    provider.close().await?;
    let report = result?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    ensure!(
        report["structure"]["all_passed"] == true,
        "Synthetic evaluation failed structural checks; inspect JSON (semantic review is separate)"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_tutor::memory_worker::{CancelFuture, ProviderFuture};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::watch;

    enum Response {
        Valid,
        Empty,
        PlanAsResult,
        PersistentTemporary,
    }
    struct Fake {
        response: Response,
        calls: AtomicUsize,
    }
    impl MemoryConsolidator for Fake {
        fn consolidate<'a>(
            &'a self,
            request: &'a Value,
            started: watch::Sender<bool>,
        ) -> ProviderFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let _ = started.send(true);
                let mut candidates = vec![];
                if !matches!(self.response, Response::Empty) {
                    for source in request["context"]["sources"].as_array().unwrap() {
                        let preference = source["content"]["explicit_preference"] == true;
                        let (mut kind, mut claim) = match s(source, "event_kind") {
                            "observation" => ("trial_observation", "reported_observation"),
                            "plan" => ("accepted_pending_experiment", "accepted_plan"),
                            _ if preference => ("help_preference", "explicit_preference"),
                            _ => ("learning_goal", "stated_goal"),
                        };
                        if matches!(self.response, Response::PlanAsResult)
                            && source["event_kind"] == "plan"
                        {
                            kind = "trial_observation";
                            claim = "reported_observation";
                        }
                        let scope = if preference {
                            if matches!(self.response, Response::PersistentTemporary) {
                                "cross_session_explicit"
                            } else {
                                "current_session"
                            }
                        } else {
                            "current_activity"
                        };
                        let eid = s(source, "event_id");
                        let mut candidate = json!({"kind":kind,"claim_type":claim,"content":source["content"]["text"],"scope":scope,"support_level":"unknown",
                        "source_event_ids":[eid],"source_event_versions":{},"source_quotes":[{"event_id":eid,"quote":source["content"]["text"]}],
                        "importance":{"goal_relevance":2,"continuity":2,"novel_change":1,"explicit_request":1}});
                        candidate["source_event_versions"][eid] = source["event_version"].clone();
                        candidates.push(candidate);
                    }
                }
                Ok(
                    json!({"schema_version":memory_contract::SCHEMA_VERSION,"candidates":candidates}),
                )
            })
        }
        fn cancel<'a>(&'a self, _request_id: &'a str) -> CancelFuture<'a> {
            Box::pin(async { Ok(()) })
        }
    }
    fn fake(response: Response) -> Fake {
        Fake {
            response,
            calls: AtomicUsize::new(0),
        }
    }

    #[test]
    fn bilingual_fixtures_preserve_one_comparison_unperformed_plan_and_temporary_preference()
    -> Result<()> {
        for language in ["zh", "en"] {
            let probe = Probe::new(language, "continuity")?;
            ensure!(probe.sources.len() == 4);
            let comparison = probe
                .sources
                .iter()
                .find(|row| row["fixture_label"] == "comparison")
                .unwrap();
            ensure!(
                comparison["event_kind"] == "observation"
                    && comparison["content"]["comparison"]["trials_per_threshold"] == 1
            );
            ensure!(comparison["content"]["comparison"]["sample_insufficient"] == true);
            let plan = probe
                .sources
                .iter()
                .find(|row| row["fixture_label"] == "pending_plan")
                .unwrap();
            ensure!(
                plan["event_kind"] == "plan"
                    && plan["content"]["accepted"] == true
                    && plan["content"]["execution_status"] == "not_started"
            );
            let preference = probe
                .sources
                .iter()
                .find(|row| row["fixture_label"] == "temporary_preference")
                .unwrap();
            ensure!(preference["content"]["preference_scope"] == "temporary");
        }
        Ok(())
    }

    #[tokio::test]
    async fn valid_fake_candidates_are_reported_for_review_without_claiming_semantic_success()
    -> Result<()> {
        for language in ["zh", "en"] {
            let provider = fake(Response::Valid);
            let report = evaluate(&provider, language, "continuity").await?;
            ensure!(
                provider.calls.load(Ordering::SeqCst) == 1
                    && report["structure"]["all_passed"] == true
            );
            ensure!(report["validated_candidates"].as_array().unwrap().len() == 4);
            ensure!(
                report["manual_semantic_review"]["status"] == "required_not_performed"
                    && report["manual_semantic_review"]["semantic_accuracy"].is_null()
            );
            ensure!(
                report["coverage_observations"]["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|row| row["matching_typed_candidate_observed"] == true)
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn unsafe_temporary_scope_and_store_rejected_plan_are_separate_structural_failures()
    -> Result<()> {
        let persistent = evaluate(&fake(Response::PersistentTemporary), "en", "continuity").await?;
        ensure!(
            persistent["worker"]["status"] == "shadow_staged"
                && persistent["structure"]["all_passed"] == false
        );
        ensure!(
            persistent["structure"]["checks"]["temporary_preferences_keep_session_scope"] == false
        );
        let plan = evaluate(&fake(Response::PlanAsResult), "zh", "continuity").await?;
        ensure!(plan["worker"]["status"] == "failed" && plan["validated_candidates"] == json!([]));
        ensure!(plan["structure"]["all_passed"] == false);
        Ok(())
    }

    #[tokio::test]
    async fn zero_candidates_can_pass_structure_but_never_count_as_educational_coverage()
    -> Result<()> {
        let report = evaluate(&fake(Response::Empty), "zh", "continuity").await?;
        ensure!(
            report["structure"]["all_passed"] == true
                && report["validated_candidates"] == json!([])
        );
        ensure!(
            report["coverage_observations"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["matching_typed_candidate_observed"] == false)
        );
        ensure!(report["manual_semantic_review"]["semantic_accuracy"].is_null());
        Ok(())
    }

    #[test]
    fn boundary_scenarios_bind_distinct_support_levels_and_excluded_unaccepted_plans() -> Result<()>
    {
        for language in ["zh", "en"] {
            let constraints = Probe::new(language, "constraints")?;
            ensure!(constraints.sources.len() == 4);
            let plan = constraints
                .sources
                .iter()
                .find(|row| row["fixture_label"] == "unaccepted_plan")
                .unwrap();
            ensure!(plan["event_kind"] == "plan" && plan["content"]["accepted"] == false);
            ensure!(constraints.fixture["excluded_source_labels"] == json!(["unaccepted_plan"]));
            let preferences = Probe::new(language, "preferences_and_support")?;
            ensure!(preferences.sources.len() == 4);
            let levels: Vec<_> = preferences
                .sources
                .iter()
                .filter(|row| row["event_kind"] == "learner_answer")
                .map(|row| row["content"]["support_level"].clone())
                .collect();
            ensure!(
                levels
                    == json!(["independent", "hinted"])
                        .as_array()
                        .unwrap()
                        .to_vec()
            );
            let empty = preferences.report(language, "preferences_and_support", json!({}))?;
            ensure!(
                empty["coverage_observations"]["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|item| item["matching_typed_candidate_observed"] == false)
            );
        }
        ensure!(fixture("unknown").is_err());
        Ok(())
    }

    #[test]
    fn coverage_cannot_count_wrong_scope_or_wrong_support_as_success() {
        let expectation = json!({"source_label":"hinted_response","kind":"concept_evidence","claim_type":"task_specific_response","scope":"current_activity","support_level":"hinted"});
        let mut candidate = json!({"source_labels":["hinted_response"],"kind":"concept_evidence","claim_type":"task_specific_response","scope":"current_activity","support_level":"independent"});
        assert!(!matches_expectation(&candidate, &expectation));
        candidate["support_level"] = json!("hinted");
        assert!(matches_expectation(&candidate, &expectation));
        candidate["scope"] = json!("cross_session_explicit");
        assert!(!matches_expectation(&candidate, &expectation));
    }
}
