//! Local-cache contracts only: fake unit vectors and temporary databases.
//! These tests do not establish semantic relevance or load an embedding model.
use ai_tutor::{
    memory_embedding::{DIM, EmbeddingProfile},
    store::{MemoryEmbeddingSnapshot, QueryEmbedding, Store, s},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};

// Fake unit vectors use an explicit registered profile; no model is loaded.
const MODEL: &str = "3804e3b06d6f86abe39d18a439e34e8fe451cdae3d0e9e4ebb33709cfc8d1dda";
fn vector(index: usize) -> Vec<f32> {
    let mut v = vec![0.0; DIM];
    v[index] = 1.0;
    v
}
struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    lid: String,
    sid: String,
    event: Value,
    mid: String,
}
impl Fixture {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("cache.sqlite3"), "simulator")?;
        let lid = s(
            &store.create_learner("SYNTHETIC", "zh", "guided")?,
            "learner_id",
        )
        .to_owned();
        let sid = s(&store.create_session(&lid)?, "session_id").to_owned();
        let event = store.record_education_event(
            &lid,
            &sid,
            "simulated",
            "observation",
            &json!({"text":"合成记录：第二次停车间隙更大。","synthetic_fixture":true}),
        )?;
        let mid = add(
            &store,
            &lid,
            &event,
            "合成记录：第二次停车间隙更大。",
            "reported_progress",
            "current_activity",
        )?;
        Ok(Self {
            dir,
            store,
            lid,
            sid,
            event,
            mid,
        })
    }
    fn snapshot(&self) -> Result<MemoryEmbeddingSnapshot> {
        Ok(self
            .store
            .embedding_work(&self.lid, MODEL, 32)?
            .into_iter()
            .find(|row| row.memory_id == self.mid)
            .unwrap())
    }
    fn count(&self) -> Result<i64> {
        Ok(self
            .store
            .db
            .query_row("SELECT COUNT(*) FROM memory_embeddings", [], |row| {
                row.get(0)
            })?)
    }
}
fn add(
    store: &Store,
    lid: &str,
    event: &Value,
    text: &str,
    kind: &str,
    scope: &str,
) -> Result<String> {
    let mut candidate = json!({"kind":kind,"scope":scope,"content":text,"source_event_ids":[event["event_id"]],"source_event_versions":{}});
    candidate["source_event_versions"][s(event, "event_id")] = event["event_version"].clone();
    Ok(store
        .add_memory(
            lid,
            &candidate,
            store.learner(lid)?["epoch"].as_i64().unwrap(),
        )?
        .unwrap())
}

#[test]
fn vector_cache_migrates_without_backfill_and_reopens_with_valid_receipts() -> Result<()> {
    let f = Fixture::new()?;
    // Reproduce the immediately preceding schema: remove the whole new feature,
    // including its triggers, while retaining the original memory/evidence DB.
    for row in f.store.rows(
        "SELECT name FROM sqlite_master WHERE type='trigger' AND name LIKE 'embedding_%'",
        [],
    )? {
        f.store
            .db
            .execute_batch(&format!("DROP TRIGGER \"{}\"", s(&row, "name")))?;
    }
    f.store.db.execute_batch("DROP TABLE memory_embeddings")?;
    let reopened = Store::open(f.dir.path().join("cache.sqlite3"), "simulator")?;
    ensure!(
        reopened
            .eligible_memory_embeddings(&f.lid, MODEL)?
            .is_empty()
    );
    let snapshot = reopened.embedding_work(&f.lid, MODEL, 1)?.remove(0);
    ensure!(reopened.commit_memory_embedding(&snapshot, MODEL, &vector(0))?);
    drop(reopened);
    let reopened = Store::open(f.dir.path().join("cache.sqlite3"), "simulator")?;
    ensure!(reopened.eligible_memory_embeddings(&f.lid, MODEL)?.len() == 1);
    ensure!(reopened.embedding_work(&f.lid, MODEL, 1)?.is_empty());
    Ok(())
}

#[test]
fn snapshots_bind_owner_namespace_epoch_model_and_content() -> Result<()> {
    let f = Fixture::new()?;
    let snapshot = f.snapshot()?;
    let other = s(
        &f.store.create_learner("OTHER_SYNTHETIC", "en", "guided")?,
        "learner_id",
    )
    .to_owned();
    let mut wrong = snapshot.clone();
    wrong.learner_id = other.clone();
    ensure!(!f.store.commit_memory_embedding(&wrong, MODEL, &vector(0))?);
    wrong = snapshot.clone();
    wrong.namespace = "real".into();
    ensure!(!f.store.commit_memory_embedding(&wrong, MODEL, &vector(0))?);
    wrong = snapshot.clone();
    wrong.learner_epoch += 1;
    ensure!(!f.store.commit_memory_embedding(&wrong, MODEL, &vector(0))?);
    wrong = snapshot.clone();
    wrong.content = "other input".into();
    ensure!(!f.store.commit_memory_embedding(&wrong, MODEL, &vector(0))?);
    ensure!(
        !f.store
            .commit_memory_embedding(&snapshot, "another-model", &vector(0))?
    );
    ensure!(
        f.store
            .commit_memory_embedding(&snapshot, MODEL, &vector(0))?
    );
    ensure!(
        f.store
            .eligible_memory_embeddings(&other, MODEL)?
            .is_empty()
    );
    ensure!(
        f.store
            .eligible_memory_embeddings(&f.lid, "another-model")?
            .is_empty()
    );
    let separate = Store::open(f.dir.path().join("real.sqlite3"), "real")?;
    ensure!(!separate.commit_memory_embedding(&snapshot, MODEL, &vector(0))?);
    Ok(())
}

#[test]
fn missing_invalid_and_stale_vectors_keep_the_lexical_fallback() -> Result<()> {
    let f = Fixture::new()?;
    let query = "第二次停车间隙";
    let expected = f.store.recall(&f.lid, query, 5)?;
    let fallback = f.store.recall_candidates(&f.lid, query, None, 5)?;
    ensure!(fallback.len() == expected.len() && fallback[0].memory == expected[0]);
    ensure!(fallback[0].semantic_rank.is_none());
    let snapshot = f.snapshot()?;
    ensure!(
        f.store
            .commit_memory_embedding(&snapshot, MODEL, &vec![0.0; DIM])
            .is_err()
    );
    ensure!(
        f.store
            .commit_memory_embedding(&snapshot, MODEL, &vec![f32::NAN; DIM])
            .is_err()
    );
    ensure!(
        f.store
            .commit_memory_embedding(&snapshot, MODEL, &[1.0])
            .is_err()
    );
    ensure!(f.count()? == 0);
    ensure!(
        f.store
            .commit_memory_embedding(&snapshot, MODEL, &vector(0))?
    );
    // Simulate a stale persisted cache even if invalidation triggers were missed.
    f.store.db.execute(
        "UPDATE memory_embeddings SET learner_epoch=learner_epoch+1",
        [],
    )?;
    ensure!(
        f.store
            .eligible_memory_embeddings(&f.lid, MODEL)?
            .is_empty()
    );
    let valid = vector(0);
    let fallback = f.store.recall_candidates(
        &f.lid,
        query,
        Some(QueryEmbedding {
            model_key: MODEL,
            values: &valid,
        }),
        5,
    )?;
    ensure!(fallback[0].memory == expected[0] && fallback[0].semantic_rank.is_none());
    let invalid = vec![f32::NAN; DIM];
    ensure!(
        f.store.recall_candidates(
            &f.lid,
            query,
            Some(QueryEmbedding {
                model_key: MODEL,
                values: &invalid
            }),
            5
        )?[0]
            .semantic_rank
            .is_none()
    );
    Ok(())
}

#[test]
fn correction_and_forgetting_purge_vectors_and_reject_late_work_after_reopen() -> Result<()> {
    for correction in [true, false] {
        let f = Fixture::new()?;
        let snapshot = f.snapshot()?;
        ensure!(
            f.store
                .commit_memory_embedding(&snapshot, MODEL, &vector(0))?
        );
        if correction {
            f.store.revise_education_event(
                &f.lid,
                &f.sid,
                s(&f.event, "event_id"),
                "simulated",
                &json!({"correction":"更正：第二次停车间隙没有变大。","synthetic_fixture":true}),
            )?;
        } else {
            f.store.forget(&f.lid, Some(&f.mid))?;
        }
        ensure!(f.count()? == 0);
        ensure!(
            !f.store
                .commit_memory_embedding(&snapshot, MODEL, &vector(0))?
        );
        let reopened = Store::open(f.dir.path().join("cache.sqlite3"), "simulator")?;
        ensure!(
            reopened
                .eligible_memory_embeddings(&f.lid, MODEL)?
                .is_empty()
        );
        ensure!(reopened.embedding_work(&f.lid, MODEL, 32)?.is_empty());
        ensure!(!reopened.commit_memory_embedding(&snapshot, MODEL, &vector(0))?);
    }
    Ok(())
}

#[test]
fn source_versions_links_content_and_deletion_invalidate_cached_derivatives() -> Result<()> {
    for change in [
        "source_version",
        "source_deleted",
        "link_status",
        "memory_content",
        "memory_status",
        "session_deleted",
        "learner_deleted",
    ] {
        let f = Fixture::new()?;
        let snapshot = f.snapshot()?;
        ensure!(
            f.store
                .commit_memory_embedding(&snapshot, MODEL, &vector(0))?
        );
        match change {
            "source_version" => {
                f.store.db.execute(
                    "UPDATE education_event_meta SET version=version+1 WHERE event_id=?",
                    [s(&f.event, "event_id")],
                )?;
            }
            "source_deleted" => {
                f.store.db.execute(
                    "DELETE FROM events WHERE event_id=?",
                    [s(&f.event, "event_id")],
                )?;
            }
            "link_status" => {
                f.store.db.execute(
                    "UPDATE memory_source_links SET status='invalidated' WHERE memory_id=?",
                    [&f.mid],
                )?;
            }
            "memory_content" => {
                f.store.db.execute(
                    "UPDATE memories SET content='updated synthetic content' WHERE memory_id=?",
                    [&f.mid],
                )?;
            }
            "memory_status" => {
                f.store.db.execute(
                    "UPDATE memories SET status='superseded' WHERE memory_id=?",
                    [&f.mid],
                )?;
            }
            "session_deleted" => {
                f.store
                    .db
                    .execute("DELETE FROM sessions WHERE session_id=?", [&f.sid])?;
            }
            "learner_deleted" => {
                f.store
                    .db
                    .execute("DELETE FROM learners WHERE learner_id=?", [&f.lid])?;
            }
            _ => unreachable!(),
        }
        ensure!(f.count()? == 0, "{change} must erase vector payload");
        ensure!(
            !f.store
                .commit_memory_embedding(&snapshot, MODEL, &vector(0))?,
            "{change} must reject a late computation"
        );
    }
    Ok(())
}

#[test]
fn semantic_candidates_do_not_expand_activity_or_temporary_preference_scope() -> Result<()> {
    for (field, value) in [
        ("activity_scope", "unrelated_activity"),
        ("scope", "current_session"),
        ("scope", "cross_session_explicit"),
    ] {
        let f = Fixture::new()?;
        let snapshot = f.snapshot()?;
        ensure!(
            f.store
                .commit_memory_embedding(&snapshot, MODEL, &vector(0))?
        );
        f.store.db.execute(
            &format!("UPDATE memories SET {field}=? WHERE memory_id=?"),
            [value, &f.mid],
        )?;
        ensure!(f.store.embedding_work(&f.lid, MODEL, 32)?.is_empty());
        ensure!(
            f.store
                .eligible_memory_embeddings(&f.lid, MODEL)?
                .is_empty()
        );
        let query = vector(0);
        ensure!(
            f.store
                .recall_candidates(
                    &f.lid,
                    "第二次停车间隙",
                    Some(QueryEmbedding {
                        model_key: MODEL,
                        values: &query
                    }),
                    5
                )?
                .is_empty()
        );
    }
    let f = Fixture::new()?;
    let event=f.store.record_education_event(&f.lid,&f.sid,"simulated","learner_statement",&json!({"text":"以后每次请给短提示。","explicit_preference":true,"preference_scope":"persistent"}))?;
    let mid = add(
        &f.store,
        &f.lid,
        &event,
        "明确请求以后每次短提示。",
        "help_preference",
        "cross_session_explicit",
    )?;
    ensure!(
        f.store
            .embedding_work(&f.lid, MODEL, 32)?
            .iter()
            .any(|row| row.memory_id == mid)
    );
    Ok(())
}

#[test]
fn indexing_and_candidate_recall_cover_more_than_the_ui_first_page() -> Result<()> {
    let f = Fixture::new()?;
    for index in 0..101 {
        let text = format!("Synthetic unrelated observation {index}.");
        let event = f.store.record_education_event(
            &f.lid,
            &f.sid,
            "simulated",
            "observation",
            &json!({"text":text}),
        )?;
        add(
            &f.store,
            &f.lid,
            &event,
            &text,
            "reported_progress",
            "current_activity",
        )?;
    }
    let mut indexed = 0;
    loop {
        let work = f.store.embedding_work(&f.lid, MODEL, 32)?;
        if work.is_empty() {
            break;
        }
        for snapshot in work {
            ensure!(f.store.commit_memory_embedding(
                &snapshot,
                MODEL,
                &vector(if snapshot.memory_id == f.mid { 0 } else { 1 })
            )?);
            indexed += 1;
        }
    }
    ensure!(indexed == 102 && f.store.memories(&f.lid)?.len() == 100);
    let query = vector(0);
    let result = f.store.recall_candidates(
        &f.lid,
        "completely distinct query",
        Some(QueryEmbedding {
            model_key: MODEL,
            values: &query,
        }),
        5,
    )?;
    ensure!(
        result
            .iter()
            .any(|row| s(&row.memory, "memory_id") == f.mid && row.semantic_rank == Some(1))
    );
    Ok(())
}

#[test]
fn high_similarity_is_only_an_owner_filtered_candidate_not_verified_relevance() -> Result<()> {
    let f = Fixture::new()?;
    let snapshot = f.snapshot()?;
    ensure!(
        f.store
            .commit_memory_embedding(&snapshot, MODEL, &vector(0))?
    );
    let other = s(
        &f.store.create_learner("OTHER_SYNTHETIC", "en", "guided")?,
        "learner_id",
    )
    .to_owned();
    let query = vector(0);
    ensure!(
        f.store
            .recall_candidates(
                &other,
                "木星的卫星",
                Some(QueryEmbedding {
                    model_key: MODEL,
                    values: &query
                }),
                5
            )?
            .is_empty()
    );
    let result = f.store.recall_candidates(
        &f.lid,
        "木星的卫星",
        Some(QueryEmbedding {
            model_key: MODEL,
            values: &query,
        }),
        5,
    )?;
    ensure!(result.len() == 1 && result[0].cosine == Some(1.0));
    ensure!(result[0].relevance_status == "candidate_requires_relevance_review");
    let serialized = serde_json::to_value(&result[0])?;
    ensure!(serialized.get("confidence").is_none() && serialized.get("source_grade").is_none());
    ensure!(
        f.store.recall(&f.lid, "木星的卫星", 5)?.is_empty(),
        "default recall must remain unchanged"
    );
    Ok(())
}

fn profile_vector(profile: EmbeddingProfile, index: usize) -> Vec<f32> {
    let mut result = vec![0.0; profile.dimension()];
    result[index] = 1.0;
    result
}

#[test]
fn old_384_rows_survive_profile_migration_and_old_writer_remains_compatible() -> Result<()> {
    let f = Fixture::new()?;
    let snapshot = f.snapshot()?;
    ensure!(
        f.store
            .commit_memory_embedding(&snapshot, MODEL, &vector(0))?
    );
    let original: Vec<u8> =
        f.store
            .db
            .query_row("SELECT vector FROM memory_embeddings", [], |row| row.get(0))?;
    for row in f.store.rows(
        "SELECT name FROM sqlite_master WHERE type='trigger' AND name LIKE 'embedding_%'",
        [],
    )? {
        f.store
            .db
            .execute_batch(&format!("DROP TRIGGER \"{}\"", s(&row, "name")))?;
    }
    f.store.db.execute_batch("ALTER TABLE memory_embeddings RENAME TO cache_from_new_schema;
        CREATE TABLE memory_embeddings(namespace TEXT NOT NULL,learner_id TEXT NOT NULL,memory_id TEXT NOT NULL,
            model_key TEXT NOT NULL,learner_epoch INTEGER NOT NULL,memory_digest TEXT NOT NULL,source_digest TEXT NOT NULL,
            vector BLOB NOT NULL CHECK(length(vector)=1536),PRIMARY KEY(namespace,learner_id,memory_id,model_key));
        INSERT INTO memory_embeddings SELECT namespace,learner_id,memory_id,model_key,learner_epoch,memory_digest,source_digest,vector FROM cache_from_new_schema;
        DROP TABLE cache_from_new_schema;")?;
    let before = f.store.memory_evidence(&f.lid, &f.mid)?;
    let upgraded = Store::open(f.dir.path().join("cache.sqlite3"), "simulator")?;
    ensure!(upgraded.memory_evidence(&f.lid, &f.mid)? == before);
    let (dimension, bytes): (usize, Vec<u8>) = upgraded.db.query_row(
        "SELECT dimension,vector FROM memory_embeddings",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(dimension == 384 && bytes == original);
    ensure!(upgraded.eligible_memory_embeddings(&f.lid, MODEL)?.len() == 1);
    let triggers = upgraded.rows(
        "SELECT name,sql FROM sqlite_master WHERE type='trigger' AND name LIKE 'embedding_%'",
        [],
    )?;
    ensure!(
        triggers.len() == 10
            && triggers
                .iter()
                .all(|row| !s(row, "sql").contains("memory_embeddings_384_previous"))
    );
    // The previous executable omits the new dimension column: DEFAULT 384 must
    // preserve that legitimate E5 write, without accepting a BGE-size payload.
    upgraded.db.execute("INSERT OR REPLACE INTO memory_embeddings(namespace,learner_id,memory_id,model_key,learner_epoch,memory_digest,source_digest,vector) VALUES(?,?,?,?,?,?,?,?)",
        rusqlite::params![snapshot.namespace,snapshot.learner_id,snapshot.memory_id,MODEL,snapshot.learner_epoch,snapshot.memory_digest,snapshot.source_digest,original])?;
    ensure!(upgraded.eligible_memory_embeddings(&f.lid, MODEL)?.len() == 1);
    upgraded.revise_education_event(
        &f.lid,
        &f.sid,
        s(&f.event, "event_id"),
        "simulated",
        &json!({"correction":"合成更正：未观察到间隙增加。"}),
    )?;
    ensure!(
        upgraded
            .db
            .query_row("SELECT COUNT(*) FROM memory_embeddings", [], |row| row
                .get::<_, i64>(0))?
            == 0
    );
    ensure!(!upgraded.commit_memory_embedding(&snapshot, MODEL, &vector(0))?);
    Ok(())
}

#[test]
fn profiles_separate_dimensions_and_same_dimension_artifacts() -> Result<()> {
    let f = Fixture::new()?;
    for profile in EmbeddingProfile::ALL {
        let key = profile.model_key();
        let snapshot = f.store.embedding_work(&f.lid, &key, 1)?.remove(0);
        ensure!(snapshot.dimension == profile.dimension());
        ensure!(
            f.store
                .commit_memory_embedding(&snapshot, &key, &profile_vector(profile, 0))?
        );
        ensure!(f.store.eligible_memory_embeddings(&f.lid, &key)?.len() == 1);
        let other = if profile == EmbeddingProfile::BgeM3NativeFp32 {
            EmbeddingProfile::E5NativeFp32
        } else {
            EmbeddingProfile::BgeM3NativeFp32
        };
        ensure!(
            f.store
                .commit_memory_embedding(&snapshot, &key, &profile_vector(other, 0))
                .is_err()
        );
        ensure!(!f.store.commit_memory_embedding(
            &snapshot,
            &other.model_key(),
            &profile_vector(other, 0)
        )?);
        let mut wrong = snapshot.clone();
        wrong.dimension = other.dimension();
        ensure!(
            !f.store
                .commit_memory_embedding(&wrong, &key, &profile_vector(profile, 0))?
        );
    }
    ensure!(f.count()? == 4);
    ensure!(
        f.store
            .embedding_work(&f.lid, "unregistered-weight", 1)
            .is_err()
    );
    ensure!(
        f.store
            .eligible_memory_embeddings(&f.lid, "unregistered-weight")?
            .is_empty()
    );
    Ok(())
}

#[test]
fn persisted_dimension_must_match_registered_profile_and_blob() -> Result<()> {
    let f = Fixture::new()?;
    let profile = EmbeddingProfile::BgeM3NativeFp32;
    let key = profile.model_key();
    let snapshot = f.store.embedding_work(&f.lid, &key, 1)?.remove(0);
    let value = profile_vector(profile, 0);
    ensure!(f.store.commit_memory_embedding(&snapshot, &key, &value)?);
    ensure!(
        f.store
            .db
            .execute("UPDATE memory_embeddings SET dimension=512", [])
            .is_err()
    );
    ensure!(
        f.store
            .db
            .execute("UPDATE memory_embeddings SET vector=x'00000000'", [])
            .is_err()
    );
    let wrong: Vec<u8> = vector(0).iter().flat_map(|v| v.to_le_bytes()).collect();
    f.store.db.execute(
        "UPDATE memory_embeddings SET dimension=384,vector=?",
        [wrong],
    )?;
    ensure!(f.store.eligible_memory_embeddings(&f.lid, &key)?.is_empty());
    let fallback = f.store.recall_candidates(
        &f.lid,
        "第二次停车间隙",
        Some(QueryEmbedding {
            model_key: &key,
            values: &value,
        }),
        5,
    )?;
    ensure!(!fallback.is_empty() && fallback.iter().all(|row| row.semantic_rank.is_none()));
    Ok(())
}

#[test]
fn bge_receipts_reject_real_source_revision_forgetting_and_owner_changes() -> Result<()> {
    for change in ["source_revision", "forget", "epoch", "owner"] {
        let f = Fixture::new()?;
        let profile = EmbeddingProfile::BgeM3NativeFp32;
        let key = profile.model_key();
        let value = profile_vector(profile, 0);
        let snapshot = f.store.embedding_work(&f.lid, &key, 1)?.remove(0);
        ensure!(f.store.commit_memory_embedding(&snapshot, &key, &value)?);
        match change {
            "source_revision" => {
                f.store.revise_education_event(
                    &f.lid,
                    &f.sid,
                    s(&f.event, "event_id"),
                    "simulated",
                    &json!({"correction":"合成更正：先前记录不准确。"}),
                )?;
            }
            "forget" => {
                f.store.forget(&f.lid, Some(&f.mid))?;
            }
            "epoch" => {
                f.store.db.execute(
                    "UPDATE learners SET epoch=epoch+1 WHERE learner_id=?",
                    [&f.lid],
                )?;
            }
            "owner" => {
                f.store
                    .db
                    .execute("DELETE FROM learners WHERE learner_id=?", [&f.lid])?;
            }
            _ => unreachable!(),
        }
        ensure!(f.count()? == 0);
        ensure!(!f.store.commit_memory_embedding(&snapshot, &key, &value)?);
    }
    Ok(())
}

#[test]
fn bge_candidate_budget_keeps_whole_sources_and_cannot_cross_owner() -> Result<()> {
    let f = Fixture::new()?;
    let profile = EmbeddingProfile::BgeM3NativeFp32;
    let key = profile.model_key();
    let query = profile_vector(profile, 0);
    for index in 0..5 {
        let text = format!("{index}{}", "合成观察".repeat(170));
        let event = f.store.record_education_event(
            &f.lid,
            &f.sid,
            "simulated",
            "observation",
            &json!({"text":text}),
        )?;
        add(
            &f.store,
            &f.lid,
            &event,
            &text,
            "reported_progress",
            "current_activity",
        )?;
    }
    for snapshot in f.store.embedding_work(&f.lid, &key, 32)? {
        ensure!(f.store.commit_memory_embedding(&snapshot, &key, &query)?);
    }
    let results = f.store.recall_candidates(
        &f.lid,
        "合成观察",
        Some(QueryEmbedding {
            model_key: &key,
            values: &query,
        }),
        5,
    )?;
    ensure!(
        results
            .iter()
            .map(|row| s(&row.memory, "content").chars().count())
            .sum::<usize>()
            <= 2400
    );
    for row in &results {
        let evidence = f
            .store
            .memory_evidence(&f.lid, s(&row.memory, "memory_id"))?;
        ensure!(row.memory["content"] == evidence["memory"]["content"]);
    }
    let other = s(
        &f.store.create_learner("OTHER_SYNTHETIC", "en", "guided")?,
        "learner_id",
    )
    .to_owned();
    ensure!(
        f.store
            .recall_candidates(
                &other,
                "合成观察",
                Some(QueryEmbedding {
                    model_key: &key,
                    values: &query
                }),
                5
            )?
            .is_empty()
    );
    ensure!(f.store.eligible_memory_embeddings(&other, &key)?.is_empty());
    Ok(())
}
