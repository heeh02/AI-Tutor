//! Real temporary Store snapshots and fake unit vectors; no encoder or device.
use ai_tutor::{
    memory_embedding::EmbeddingProfile,
    store::{EmbeddingCoverage, EmbeddingExclusion, MemoryEmbeddingSnapshot, Store, s},
};
use anyhow::{Result, ensure};
use serde_json::json;

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    lid: String,
    sid: String,
    key: String,
}
impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let store = Store::open(directory.path().join("coverage.sqlite3"), "simulator")?;
        let lid = s(&store.create_learner("Synthetic coverage", "en", "guided")?, "learner_id").to_owned();
        let sid = s(&store.create_session(&lid)?, "session_id").to_owned();
        Ok(Self { _dir:directory, store, lid, sid, key:EmbeddingProfile::BgeM3NativeFp32.model_key() })
    }
    fn add(&self, sid: &str, text: &str, preference: bool) -> Result<(String,String)> {
        let content = if preference {
            json!({"text":text,"explicit_preference":true,"preference_scope":"persistent"})
        } else { json!({"text":text}) };
        let event = self.store.record_education_event(&self.lid, sid, "simulated",
            if preference { "learner_statement" } else { "observation" }, &content)?;
        let memory = self.store.add_memory(&self.lid, &json!({
            "kind":if preference {"help_preference"} else {"reported_progress"},
            "scope":if preference {"cross_session_explicit"} else {"current_activity"},
            "content":text,"source_event_ids":[event["event_id"]]
        }), self.store.learner(&self.lid)?["epoch"].as_i64().unwrap())?.unwrap();
        Ok((memory,s(&event,"event_id").to_owned()))
    }
    fn snapshot(&self, mid: &str) -> Result<MemoryEmbeddingSnapshot> {
        Ok(self.store.embedding_work(&self.lid,&self.key,32)?.into_iter()
            .find(|snapshot| snapshot.memory_id == mid).unwrap())
    }
    fn cache(&self, mid: &str) -> Result<()> {
        let mut vector = vec![0.0;1024]; vector[0] = 1.0;
        ensure!(self.store.commit_memory_embedding(&self.snapshot(mid)?,&self.key,&vector)?);
        Ok(())
    }
    fn continue_from(&self, source: &str, current: &str) -> Result<String> {
        let generation = self.store.session(current)?["generation"].as_i64().unwrap();
        let after = self.store.bump(current,"idle")?;
        Ok(s(&self.store.continue_finished_session(&self.lid,source,"coverage-continuation",
            current,generation,after)?,"session_id").to_owned())
    }
}

#[test]
fn exact_too_long_exclusion_does_not_starve_later_work_or_claim_complete_coverage() -> Result<()> {
    let f = Fixture::new()?;
    let (short,_) = f.add(&f.sid,"The short independent observation.",false)?;
    let (long,_) = f.add(&f.sid,&"长".repeat(700),false)?;
    let first = f.store.embedding_work(&f.lid,&f.key,1)?.remove(0);
    ensure!(first.memory_id == long);
    // Simulate a confirmed TooLong result; Store does not infer token counts.
    let excluded = EmbeddingExclusion::from(&first);
    let serialized = serde_json::to_value(&excluded)?;
    ensure!(serialized.as_object().unwrap().len() == 4 && serialized.get("content").is_none());
    let next = f.store.embedding_work_excluding(&f.lid,&f.key,1,&[excluded.clone()])?;
    ensure!(next.len() == 1 && next[0].memory_id == short);
    ensure!(f.store.embedding_work(&f.lid,&f.key,1)?[0].memory_id == long);
    f.cache(&short)?;
    ensure!(f.store.embedding_work_excluding(&f.lid,&f.key,1,&[excluded.clone()])?.is_empty());
    ensure!(f.store.embedding_coverage_for_session(&f.sid,&f.key)? == EmbeddingCoverage { eligible:2,indexed:1 });
    ensure!(f.store.embedding_work_excluding(&f.lid,&f.key,1,&vec![excluded.clone();129]).is_err());
    for field in 0..4 {
        let mut changed = excluded.clone();
        match field {
            0 => changed.memory_id.push_str("-other"),
            1 => changed.learner_epoch += 1,
            2 => changed.memory_digest.push('0'),
            _ => changed.source_digest.push('0'),
        }
        ensure!(f.store.embedding_work_excluding(&f.lid,&f.key,1,&[changed])?[0].memory_id == long);
    }
    Ok(())
}

#[test]
fn content_epoch_and_public_correction_make_old_exclusions_stale() -> Result<()> {
    let f = Fixture::new()?;
    let (other,_) = f.add(&f.sid,"An unrelated observation to forget.",false)?;
    let (mid,_) = f.add(&f.sid,"Original report text.",false)?;
    let old = EmbeddingExclusion::from(&f.snapshot(&mid)?);
    // A legacy in-place content change must also expire the digest receipt.
    f.store.db.execute("UPDATE memories SET content='Revised shorter report.' WHERE memory_id=?",[&mid])?;
    ensure!(f.store.embedding_work_excluding(&f.lid,&f.key,1,&[old])?[0].memory_id == mid);
    let before_epoch = EmbeddingExclusion::from(&f.snapshot(&mid)?);
    f.store.forget(&f.lid,Some(&other))?;
    let fresh = f.store.embedding_work_excluding(&f.lid,&f.key,1,&[before_epoch])?.remove(0);
    ensure!(fresh.memory_id == mid && fresh.learner_epoch == 1);
    let old = EmbeddingExclusion::from(&fresh);
    let corrected = f.store.correct_memory(&f.lid,&mid,"The learner's explicit corrected report.")?;
    let next = f.store.embedding_work_excluding(&f.lid,&f.key,32,&[old])?;
    ensure!(next.iter().any(|row|row.memory_id == corrected));
    ensure!(next.iter().all(|row|row.memory_id != mid));
    Ok(())
}

#[test]
fn coverage_matches_current_selected_ancestor_and_preference_scope_without_siblings() -> Result<()> {
    let f = Fixture::new()?;
    let (ancestor,ancestor_event) = f.add(&f.sid,"Ancestor observation.",false)?;
    f.store.finish_session(&f.sid)?;
    let sibling = s(&f.store.create_session(&f.lid)?,"session_id").to_owned();
    let (outside,_) = f.add(&sibling,"Unindexed sibling observation.",false)?;
    let (preference,_) = f.add(&sibling,"Always explain one step at a time.",true)?;
    let current = f.continue_from(&f.sid,&sibling)?;
    let (own,_) = f.add(&current,"Current observation.",false)?;
    ensure!(f.store.embedding_coverage_for_session(&current,&f.key)? == EmbeddingCoverage {eligible:3,indexed:0});
    for mid in [&ancestor,&preference,&own] { f.cache(mid)?; }
    ensure!(f.store.embedding_work(&f.lid,&f.key,32)?.iter().any(|row|row.memory_id == outside));
    ensure!(f.store.embedding_coverage_for_session(&current,&f.key)? == EmbeddingCoverage {eligible:3,indexed:3});
    // Even a persisted row is not indexed when its receipt no longer matches.
    f.store.db.execute("UPDATE memory_embeddings SET source_digest='stale' WHERE memory_id=?",[&own])?;
    ensure!(f.store.embedding_coverage_for_session(&current,&f.key)? == EmbeddingCoverage {eligible:3,indexed:2});
    f.store.db.execute("UPDATE education_event_meta SET version=version+1 WHERE event_id=?",[&ancestor_event])?;
    ensure!(f.store.embedding_coverage_for_session(&current,&f.key)? == EmbeddingCoverage {eligible:2,indexed:1});
    f.cache(&own)?;
    ensure!(f.store.embedding_coverage_for_session(&current,&f.key)? == EmbeddingCoverage {eligible:2,indexed:2});
    ensure!(f.store.embedding_coverage_for_session(&current,"unknown-profile").is_err());
    ensure!(f.store.embedding_coverage_for_session("unknown-session",&f.key).is_err());
    Ok(())
}

#[test]
fn invalid_comparison_background_is_neither_eligible_nor_indexed_even_with_primary_cache() -> Result<()> {
    let f = Fixture::new()?;
    f.store.save_comparison(&f.lid,&f.sid,&json!({"submission_id":"coverage-comparison","source_type":"simulated",
        "question":"What changed?","changed_variable":"Span","conditions":[{"name":"paper","status":"not_confirmed"}],
        "trials":[{"label":"A","setting":"15 cm","status":"reported","result":"Three coins once.","trial_count":1},
            {"label":"B","setting":"Longer, undecided","status":"not_tested","result":null,"trial_count":null}],
        "pending_plan":null}))?;
    let sources = f.store.recent_evidence(&f.sid)?;
    let observation = sources.iter().find(|row|row["event_kind"] == "observation").unwrap();
    let untested = sources.iter().find(|row|row["event_kind"] == "plan").unwrap();
    let mid = f.store.add_memory(&f.lid,&json!({"kind":"reported_progress","scope":"current_activity",
        "content":"Three coins once.","source_event_ids":[observation["event_id"]]}),0)?.unwrap();
    f.cache(&mid)?;
    ensure!(f.store.embedding_coverage_for_session(&f.sid,&f.key)? == EmbeddingCoverage {eligible:1,indexed:1});
    f.store.db.execute("DELETE FROM events WHERE event_id=?",[s(untested,"event_id")])?;
    ensure!(f.store.memory_evidence(&f.lid,&mid)?["comparison_context"]["status"] == "unavailable");
    ensure!(f.store.embedding_coverage_for_session(&f.sid,&f.key)? == EmbeddingCoverage {eligible:0,indexed:0});
    Ok(())
}
