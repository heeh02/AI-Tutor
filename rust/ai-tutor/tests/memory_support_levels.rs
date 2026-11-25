//! Real Store job/receipt regressions using synthetic events and temporary SQLite.
use ai_tutor::store::{MemoryJobLimits, Store, s, strings};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::HashSet;

const NOW_MS: i64 = 1_793_798_340_000;

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    lid: String,
    events: Vec<Value>,
    job: Value,
    receipt: Value,
}

impl Fixture {
    fn new(levels: &[Option<&str>]) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("support-levels.sqlite3"), "simulator")?;
        let learner = store.create_learner("synthetic-support-levels", "en", "guided")?;
        let lid = s(&learner, "learner_id").to_owned();
        let session = store.create_session(&lid)?;
        let sid = s(&session, "session_id");
        let mut events = Vec::new();
        for (index, level) in levels.iter().enumerate() {
            let mut content = json!({"text":format!(
                "Synthetic answer {}: the loop adds two to the counter.", index + 1
            )});
            if let Some(level) = level {
                content["support_level"] = json!(level);
            }
            events.push(store.record_education_event(
                &lid,
                sid,
                "simulated",
                "learner_answer",
                &content,
            )?);
        }
        let ids = events
            .iter()
            .map(|event| s(event, "event_id").to_owned())
            .collect::<Vec<_>>();
        let limits = MemoryJobLimits::default();
        let job = store.enqueue_memory_job(
            &lid,
            sid,
            &ids,
            "synthetic-support-level-contract-v1",
            &limits,
            NOW_MS,
        )?;
        let claim = store
            .claim_memory_job("support-level-fixture", &limits, NOW_MS)?
            .expect("the fixture owns the only queued job");
        let receipt = claim["receipt"].clone();
        store.mark_memory_job_started(&receipt, NOW_MS + 1)?;
        Ok(Self {
            _dir: dir,
            store,
            lid,
            events,
            job,
            receipt,
        })
    }

    fn proposal(&self, indices: &[usize], support_level: &str) -> Value {
        let mut proposal = json!({
            "kind":"concept_evidence",
            "claim_type":"task_specific_response",
            "content":"Described the counter change in this task; this is not a mastery claim.",
            "scope":"current_activity",
            "source_event_ids":[],
            "source_event_versions":{},
            "source_quotes":[],
            "support_level":support_level,
            "importance":{"goal_relevance":3,"continuity":3,"novel_change":3,"explicit_request":3}
        });
        for &index in indices {
            let event = &self.events[index];
            let eid = s(event, "event_id");
            proposal["source_event_ids"]
                .as_array_mut()
                .unwrap()
                .push(json!(eid));
            proposal["source_event_versions"][eid] = event["event_version"].clone();
            proposal["source_quotes"]
                .as_array_mut()
                .unwrap()
                .push(json!({"event_id":eid,"quote":s(&event["content"], "text")}));
        }
        proposal
    }

    fn stage(&self, proposals: &[Value]) -> Result<Vec<Value>> {
        self.store
            .stage_memory_candidates(&self.receipt, proposals, NOW_MS + 2)
    }

    fn apply_and_check(&self, staged: &Value, indices: &[usize], level: &str) -> Result<()> {
        let evidence = self.store.apply_memory_candidate(
            &self.lid,
            s(staged, "candidate_id"),
            &["concept_evidence"],
            NOW_MS + 3,
        )?;
        ensure!(evidence["education_metadata"]["support_level"] == level);
        ensure!(evidence["education_metadata"]["source_grade"] == "simulated");
        ensure!(evidence["memory"]["source_type"] == "simulated");
        ensure!(evidence["education_metadata"]["source_count"] == json!(indices.len()));
        ensure!(evidence["education_metadata"]["importance"] == staged["payload"]["importance"]);
        let expected: HashSet<_> = indices
            .iter()
            .map(|&index| s(&self.events[index], "event_id").to_owned())
            .collect();
        let actual: HashSet<_> = strings(&evidence["memory"]["source_event_ids"])
            .into_iter()
            .collect();
        ensure!(actual == expected);
        let sources = evidence["sources"].as_array().unwrap();
        ensure!(sources.len() == indices.len());
        for source in sources {
            let original = self
                .events
                .iter()
                .find(|event| s(event, "event_id") == s(source, "event_id"))
                .expect("persisted evidence must be one of the actual input events");
            ensure!(source["event_version"] == original["event_version"]);
            ensure!(source["content"] == original["content"]);
        }
        Ok(())
    }

    fn assert_rejected(&self, support_level: &str) -> Result<()> {
        // A valid sibling candidate proves a rejection also leaves the whole
        // staging transaction unchanged, rather than partially saving a job.
        let valid = self.proposal(&[0], "independent");
        let mixed = self.proposal(&[0, 1], support_level);
        let result = self.stage(&[valid, mixed]);
        ensure!(
            result.is_err(),
            "mixed source support was incorrectly staged as {support_level}"
        );
        ensure!(result.unwrap_err().to_string().contains("提示程度"));
        let status = self
            .store
            .memory_job_status(&self.lid, s(&self.job, "job_id"))?;
        ensure!(status["candidates"].as_array().unwrap().is_empty());
        ensure!(self.store.memories(&self.lid)?.is_empty());
        Ok(())
    }
}

#[test]
fn mixed_independent_and_hinted_cannot_be_staged_as_independent() -> Result<()> {
    Fixture::new(&[Some("independent"), Some("hinted")])?.assert_rejected("independent")
}

#[test]
fn mixed_independent_and_hinted_cannot_be_staged_as_hinted() -> Result<()> {
    Fixture::new(&[Some("independent"), Some("hinted")])?.assert_rejected("hinted")
}

#[test]
fn explicit_and_unknown_cannot_be_staged_as_explicit() -> Result<()> {
    Fixture::new(&[Some("independent"), Some("unknown")])?.assert_rejected("independent")
}

#[test]
fn explicit_and_missing_cannot_be_staged_as_explicit() -> Result<()> {
    Fixture::new(&[Some("independent"), None])?.assert_rejected("independent")
}

#[test]
fn matching_explicit_levels_survive_job_stage_and_apply() -> Result<()> {
    for level in ["independent", "hinted", "scaffolded"] {
        let fixture = Fixture::new(&[Some(level), Some(level)])?;
        let staged = fixture.stage(&[fixture.proposal(&[0, 1], level)])?;
        ensure!(staged.len() == 1 && staged[0]["status"] == "shadow");
        fixture.apply_and_check(&staged[0], &[0, 1], level)?;
        ensure!(
            fixture
                .store
                .memory_job_status(&fixture.lid, s(&fixture.job, "job_id"))?["status"]
                == "applied"
        );
    }
    Ok(())
}

#[test]
fn mixed_support_can_remain_unknown_without_importance_upgrading_it() -> Result<()> {
    let fixture = Fixture::new(&[Some("independent"), Some("hinted"), Some("unknown"), None])?;
    let staged = fixture.stage(&[fixture.proposal(&[0, 1, 2, 3], "unknown")])?;
    fixture.apply_and_check(&staged[0], &[0, 1, 2, 3], "unknown")?;
    Ok(())
}

#[test]
fn mixed_job_may_keep_separate_source_bound_support_levels() -> Result<()> {
    let fixture = Fixture::new(&[Some("independent"), Some("hinted")])?;
    let staged = fixture.stage(&[
        fixture.proposal(&[0], "independent"),
        fixture.proposal(&[1], "hinted"),
    ])?;
    ensure!(staged.len() == 2);
    fixture.apply_and_check(&staged[0], &[0], "independent")?;
    fixture.apply_and_check(&staged[1], &[1], "hinted")?;
    ensure!(fixture.store.memories(&fixture.lid)?.len() == 2);
    ensure!(
        fixture
            .store
            .memory_job_status(&fixture.lid, s(&fixture.job, "job_id"))?["status"]
            == "applied"
    );
    Ok(())
}
