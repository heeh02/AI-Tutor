//! Temporary Store + synthetic unit vectors; no model or provider invocation.
use ai_tutor::{
    context::build_context_with_semantic,
    memory_embedding::EmbeddingProfile,
    store::{CandidateRanking, QueryEmbedding, Store, s},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    lid: String,
    sid: String,
    key: String,
}
impl Fixture {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("preference.sqlite3"),"simulator")?;
        let lid = s(&store.create_learner("Synthetic preference priority","en","guided")?,"learner_id").to_owned();
        let sid = s(&store.create_session(&lid)?,"session_id").to_owned();
        Ok(Self { _dir:dir,store,lid,sid,key:EmbeddingProfile::BgeM3NativeFp32.model_key() })
    }
    fn preference(&self, lid: &str, sid: &str, id: &str, text: &str) -> Result<Value> {
        self.store.save_help_preference(lid,sid,&json!({"content":text,"request_id":id,
            "expected_epoch":self.store.learner(lid)?["epoch"],
            "expected_generation":self.store.session(sid)?["generation"]}))
    }
    fn fact(&self, text: &str) -> Result<String> {
        let event = self.store.record_education_event(&self.lid,&self.sid,"simulated","observation",&json!({"text":text}))?;
        Ok(self.store.add_memory(&self.lid,&json!({"kind":"reported_progress","scope":"current_activity",
            "content":text,"source_event_ids":[event["event_id"]]}),self.store.learner(&self.lid)?["epoch"].as_i64().unwrap())?.unwrap())
    }
    fn cache_all(&self, preference: &str) -> Result<()> {
        for snapshot in self.store.embedding_work(&self.lid,&self.key,32)? {
            let vector = vector(if snapshot.memory_id == preference {1} else {0});
            ensure!(self.store.commit_memory_embedding(&snapshot,&self.key,&vector)?);
        }
        Ok(())
    }
    fn context(&self, text: &str) -> Result<Value> {
        let event = self.store.record_education_event(&self.lid,&self.sid,"simulated","learner_statement",&json!({"text":text}))?;
        build_context_with_semantic(&self.store,&self.store.session(&self.sid)?,&self.store.learner(&self.lid)?,
            text,s(&event,"event_id"),&json!({}),text,
            QueryEmbedding {model_key:&self.key,values:&vector(0)},CandidateRanking::DenseOnly)
    }
}
fn vector(index: usize) -> Vec<f32> {
    let mut vector = vec![0.0;1024]; vector[index] = 1.0; vector
}

#[test]
fn six_more_similar_facts_cannot_displace_an_explicit_persistent_help_preference() -> Result<()> {
    let f = Fixture::new()?;
    let saved = f.preference(&f.lid,&f.sid,"presentation-1","Always give one short example before explaining.")?;
    let mid = s(&saved,"memory_id");
    for index in 0..6 { f.fact(&format!("Synthetic observation {index}: the lamp was lit once; no repeat result was recorded."))?; }
    f.cache_all(mid)?;
    let dense = f.store.recall_candidates_for_session_ranked(&f.sid,"How should we start?",
        QueryEmbedding {model_key:&f.key,values:&vector(0)},5,CandidateRanking::DenseOnly)?;
    ensure!(dense.len() == 5 && dense.iter().all(|row|row.memory["memory_id"] != mid));
    let context = f.context("How should we start?")?;
    let memories = context["slow_memory"].as_array().unwrap();
    ensure!(memories[0]["memory_id"] == mid && memories.len() <= 5);
    ensure!(memories.iter().map(|row|s(row,"content").chars().count()).sum::<usize>() <= 2400);
    ensure!(memories.iter().filter(|row|row["memory_id"] == mid).count() == 1);
    ensure!(memories[0]["scope"] == "cross_session_explicit" && memories[0]["source_type"] == "simulated");
    let preference_event = context["evidence"].as_array().unwrap().iter()
        .find(|row|row["event_id"] == saved["event_id"]).unwrap();
    ensure!(preference_event["source_type"] == "simulated");
    Ok(())
}

#[test]
fn foreign_owner_and_legacy_activity_preferences_cannot_occupy_the_explicit_list() -> Result<()> {
    let mut f = Fixture::new()?;
    let saved = f.preference(&f.lid,&f.sid,"own","Always let me see a short example first.")?;
    let mut legacy = Vec::new();
    for index in 0..6 {
        let mid = f.fact(&format!("Synthetic legacy per-activity help {index}."))?;
        // Deliberately construct legacy rows that current production writers
        // reject. This fixture tests filtering before top-five, not promotion.
        f.store.db.execute("UPDATE memories SET kind='help_preference',updated_at='2099-01-01T00:00:00Z' WHERE memory_id=?",[&mid])?;
        let evidence = f.store.memory_evidence(&f.lid,&mid)?;
        ensure!(evidence["activity_binding"]["status"] == "unresolved"
            && evidence["sources"][0]["session_id"] == f.sid);
        legacy.push(mid);
    }
    let not_explicit = f.fact("Synthetic legacy row without any explicit persistent preference source.")?;
    f.store.db.execute("UPDATE memories SET kind='help_preference',scope='cross_session_explicit',updated_at='2100-01-01T00:00:00Z' WHERE memory_id=?",[&not_explicit])?;
    let unresolved_query = f.store.recall_for_session(&f.sid,"",5)?;
    ensure!(unresolved_query.len() == 1 && unresolved_query[0]["memory_id"] == saved["memory_id"],
        "unresolved legacy fixture rows: {}",json!(unresolved_query));
    // The production trigger correctly revoked derived activity bindings when
    // the fixture changed kind/scope. Simulate pre-binding legacy rows, then let
    // the normal Store migration derive identity from their actual live sources.
    for mid in legacy.iter().chain(std::iter::once(&not_explicit)) {
        f.store.db.execute("DELETE FROM memory_activity_bindings WHERE memory_id=?",[mid])?;
    }
    f.store = Store::open(f._dir.path().join("preference.sqlite3"),"simulator")?;
    let activity = f.store.session(&f.sid)?["activity_id"].clone();
    for mid in &legacy {
        let evidence = f.store.memory_evidence(&f.lid,mid)?;
        ensure!(evidence["activity_binding"]["status"] == "bound"
            && evidence["activity_binding"]["activity_id"] == activity
            && evidence["sources"][0]["session_id"] == f.sid);
    }
    let foreign = f.store.create_learner("Synthetic other owner","en","guided")?;
    let foreign_lid = s(&foreign,"learner_id");
    let foreign_session = f.store.create_session(foreign_lid)?;
    let foreign_preference = f.preference(foreign_lid,s(&foreign_session,"session_id"),"foreign","Always use the foreign owner's setting.")?;
    let empty_query = f.store.recall_for_session(&f.sid,"",5)?;
    ensure!(empty_query.len() == 5 && empty_query.iter().all(|row|row["scope"] == "current_activity"),
        "legacy fixture empty-query rows: {}",json!(empty_query));
    let explicit = f.store.explicit_help_preferences_for_session(&f.sid)?;
    ensure!(explicit.len() == 1 && explicit[0]["memory_id"] == saved["memory_id"]);
    ensure!(explicit.iter().all(|row|row["learner_id"] == f.lid && row["memory_id"] != foreign_preference["memory_id"]));
    ensure!(f.store.explicit_help_preferences_for_session("missing-session").is_err());
    Ok(())
}

#[test]
fn correction_and_forgetting_revoke_old_preferences_without_upgrading_simulated_sources() -> Result<()> {
    let f = Fixture::new()?;
    let saved = f.preference(&f.lid,&f.sid,"original","Always start with a short example.")?;
    let old = s(&saved,"memory_id");
    let corrected = f.store.correct_memory(&f.lid,old,"Always ask one short question first.")?;
    let preferences = f.store.explicit_help_preferences_for_session(&f.sid)?;
    ensure!(preferences.len() == 1 && preferences[0]["memory_id"] == corrected);
    ensure!(preferences[0]["memory_id"] != old && preferences[0]["source_type"] == "simulated");
    let evidence = f.store.memory_evidence(&f.lid,&corrected)?;
    ensure!(evidence["sources"][0]["source_type"] == "simulated"
        && evidence["sources"][0]["event_kind"] == "teacher_setting"
        && evidence["sources"][0]["content"]["explicit_preference"] == true
        && evidence["sources"][0]["content"]["preference_scope"] == "persistent");
    f.store.forget(&f.lid,Some(&corrected))?;
    ensure!(f.store.explicit_help_preferences_for_session(&f.sid)?.is_empty());
    let context = f.context("How should we start now?")?;
    ensure!(context["slow_memory"].as_array().unwrap().iter().all(|row|row["kind"] != "help_preference"));
    ensure!(!context.to_string().contains(old) && !context.to_string().contains(&corrected));
    Ok(())
}
