//! Temporary SQLite and synthetic unit vectors. No encoder, model or device.
use ai_tutor::{
    context::{build_context, build_context_with_semantic, memory_recall_query},
    memory_embedding::EmbeddingProfile,
    store::{CandidateRanking, QueryEmbedding, RecallCandidate, Store, s},
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
        let store = Store::open(dir.path().join("semantic.sqlite3"), "simulator")?;
        let learner = store.create_learner("SYNTHETIC", "en", "guided")?;
        let lid = s(&learner, "learner_id").to_owned();
        let sid = s(&store.create_session(&lid)?, "session_id").to_owned();
        Ok(Self {
            _dir: dir,
            store,
            lid,
            sid,
            key: EmbeddingProfile::BgeM3NativeFp32.model_key(),
        })
    }
    fn add(&self, sid: &str, text: &str, kind: &str) -> Result<(String, Value)> {
        let (event_kind, scope, content) = match kind {
            "pending_experiment" => (
                "plan",
                "current_activity",
                json!({"text":text,"accepted":true}),
            ),
            "help_preference" => (
                "learner_statement",
                "cross_session_explicit",
                json!({"text":text,"explicit_preference":true,"preference_scope":"persistent"}),
            ),
            _ => ("observation", "current_activity", json!({"text":text})),
        };
        let event =
            self.store
                .record_education_event(&self.lid, sid, "simulated", event_kind, &content)?;
        let mut candidate = json!({"kind":kind,"scope":scope,"content":text,"source_event_ids":[event["event_id"]],"source_event_versions":{}});
        candidate["source_event_versions"][s(&event, "event_id")] = event["event_version"].clone();
        let mid = self
            .store
            .add_memory(
                &self.lid,
                &candidate,
                self.store.learner(&self.lid)?["epoch"].as_i64().unwrap(),
            )?
            .unwrap();
        Ok((mid, event))
    }
    fn cache(&self, mid: &str, vector: &[f32]) -> Result<()> {
        let snapshot = self
            .store
            .embedding_work(&self.lid, &self.key, 32)?
            .into_iter()
            .find(|row| row.memory_id == mid)
            .unwrap();
        ensure!(
            self.store
                .commit_memory_embedding(&snapshot, &self.key, vector)?
        );
        Ok(())
    }
    fn ranked(
        &self,
        sid: &str,
        text: &str,
        ranking: CandidateRanking,
    ) -> Result<Vec<RecallCandidate>> {
        self.store.recall_candidates_for_session_ranked(
            sid,
            text,
            QueryEmbedding {
                model_key: &self.key,
                values: &vector(1.0, 0.0),
            },
            5,
            ranking,
        )
    }
    fn continue_from(&self, source: &str, current: &str, nonce: &str) -> Result<String> {
        let generation = self.store.session(current)?["generation"].as_i64().unwrap();
        ensure!(
            self.store
                .continuation_replay(&self.lid, source, nonce, current, generation)?
                .is_none()
        );
        let after = self.store.bump(current, "idle")?;
        Ok(s(
            &self
                .store
                .continue_finished_session(&self.lid, source, nonce, current, generation, after)?,
            "session_id",
        )
        .to_owned())
    }
}
fn vector(a: f32, b: f32) -> Vec<f32> {
    let mut values = vec![0.0; 1024];
    values[0] = a;
    values[1] = b;
    values
}
fn ids(rows: &[RecallCandidate]) -> Vec<&str> {
    rows.iter().map(|row| s(&row.memory, "memory_id")).collect()
}

#[test]
fn explicit_hybrid_matches_legacy_on_the_same_snapshot_and_default_stays_lexical() -> Result<()> {
    let f = Fixture::new()?;
    let (mid, _) = f.add(&f.sid, "recorded gap observation", "reported_progress")?;
    let input = "recorded gap";
    let event = f.store.record_education_event(
        &f.lid,
        &f.sid,
        "simulated",
        "learner_statement",
        &json!({"text":input}),
    )?;
    let session = f.store.session(&f.sid)?;
    let learner = f.store.learner(&f.lid)?;
    let before = build_context(
        &f.store,
        &session,
        &learner,
        input,
        s(&event, "event_id"),
        &json!({}),
    )?;
    f.cache(&mid, &vector(1.0, 0.0))?;
    let ranked = f.ranked(&f.sid, input, CandidateRanking::HybridRrf)?;
    let old = f.store.recall_candidates_for_session(
        &f.sid,
        input,
        Some(QueryEmbedding {
            model_key: &f.key,
            values: &vector(1.0, 0.0),
        }),
        5,
    )?;
    ensure!(serde_json::to_value(ranked)? == serde_json::to_value(old)?);
    let after = build_context(
        &f.store,
        &session,
        &learner,
        input,
        s(&event, "event_id"),
        &json!({}),
    )?;
    ensure!(
        before == after,
        "writing vectors must not change default context"
    );
    let semantic = build_context_with_semantic(
        &f.store,
        &session,
        &learner,
        input,
        s(&event, "event_id"),
        &json!({}),
        input,
        QueryEmbedding {
            model_key: &f.key,
            values: &vector(1.0, 0.0),
        },
        CandidateRanking::HybridRrf,
    )?;
    ensure!(
        semantic == before,
        "same selected memory must share the entire evidence projection"
    );
    for field in [
        "ranking_score",
        "cosine",
        "semantic_rank",
        "lexical_rank",
        "relevance_status",
    ] {
        ensure!(!semantic.to_string().contains(field));
    }
    Ok(())
}

#[test]
fn dense_only_ignores_lexical_plan_bonus_and_never_adds_uncached_records() -> Result<()> {
    let f = Fixture::new()?;
    let (progress, _) = f.add(&f.sid, "zz observed", "reported_progress")?;
    let (plan, _) = f.add(&f.sid, "zz next comparison not run", "pending_experiment")?;
    f.cache(&progress, &vector(1.0, 0.0))?;
    f.cache(&plan, &vector(0.8, 0.6))?;
    let (uncached, _) = f.add(&f.sid, "zz zz extra plan not run", "pending_experiment")?;
    ensure!(s(&f.store.recall_for_session(&f.sid, "zz", 5)?[0], "kind") == "pending_experiment");
    let rows = f.ranked(&f.sid, "zz", CandidateRanking::DenseOnly)?;
    ensure!(ids(&rows) == vec![progress.as_str(), plan.as_str()]);
    ensure!(
        rows.iter()
            .all(|row| row.lexical_rank.is_none() && row.cosine == Some(row.ranking_score))
    );
    ensure!(!ids(&rows).contains(&uncached.as_str()));
    Ok(())
}

#[test]
fn explicit_invalid_queries_error_even_without_cache_while_legacy_keeps_fallback() -> Result<()> {
    let f = Fixture::new()?;
    f.add(&f.sid, "recorded observation", "reported_progress")?;
    for mode in [CandidateRanking::DenseOnly, CandidateRanking::HybridRrf] {
        for (key, values) in [
            (&f.key[..], vec![]),
            (&f.key[..], vec![0.0; 1024]),
            (&f.key[..], vec![f32::NAN; 1024]),
            ("unknown-profile", vector(1.0, 0.0)),
            (&f.key[..], vec![1.0; 384]),
        ] {
            ensure!(
                f.store
                    .recall_candidates_for_session_ranked(
                        &f.sid,
                        "recorded",
                        QueryEmbedding {
                            model_key: key,
                            values: &values
                        },
                        5,
                        mode
                    )
                    .is_err()
            );
        }
    }
    ensure!(
        f.ranked(&f.sid, "recorded", CandidateRanking::DenseOnly)?
            .is_empty()
    );
    ensure!(
        f.ranked(&f.sid, "recorded", CandidateRanking::HybridRrf)?
            .len()
            == 1
    );
    ensure!(
        f.store
            .recall_candidates_for_session(
                &f.sid,
                "recorded",
                Some(QueryEmbedding {
                    model_key: "unknown",
                    values: &[]
                }),
                5
            )?
            .len()
            == 1
    );
    Ok(())
}

#[test]
fn activity_chain_and_owner_filter_precede_dense_top_five() -> Result<()> {
    let f = Fixture::new()?;
    let (ancestor, _) = f.add(&f.sid, "ancestor observation", "reported_progress")?;
    f.cache(&ancestor, &vector(0.6, 0.8))?;
    f.store.finish_session(&f.sid)?;
    let sibling = s(&f.store.create_session(&f.lid)?, "session_id").to_owned();
    let mut excluded = vec![];
    for index in 0..6 {
        let (mid, _) = f.add(
            &sibling,
            &format!("unselected sibling {index}"),
            "reported_progress",
        )?;
        f.cache(&mid, &vector(1.0, 0.0))?;
        excluded.push(mid);
    }
    let (preference, _) = f.add(&sibling, "Always use short hints", "help_preference")?;
    f.cache(&preference, &vector(0.0, 1.0))?;
    f.store.finish_session(&sibling)?;
    let child = f.continue_from(&f.sid, &sibling, "choose-ancestor")?;
    let (parent, _) = f.add(&child, "parent observation", "reported_progress")?;
    f.cache(&parent, &vector(0.8, 0.6))?;
    f.store.finish_session(&child)?;
    let leaf = f.continue_from(&child, &child, "choose-parent")?;
    for mode in [CandidateRanking::DenseOnly, CandidateRanking::HybridRrf] {
        let rows = f.ranked(&leaf, "unselected sibling", mode)?;
        let actual = ids(&rows);
        ensure!(
            actual.contains(&ancestor.as_str())
                && actual.contains(&parent.as_str())
                && actual.contains(&preference.as_str())
        );
        ensure!(excluded.iter().all(|id| !actual.contains(&id.as_str())));
    }
    let other = f.store.create_learner("OTHER_SYNTHETIC", "en", "guided")?;
    let other_session = f.store.create_session(s(&other, "learner_id"))?;
    ensure!(
        f.ranked(
            s(&other_session, "session_id"),
            "unselected sibling",
            CandidateRanking::DenseOnly
        )?
        .is_empty()
    );
    Ok(())
}

#[test]
fn semantic_context_keeps_continuation_current_input_and_requires_the_exact_query() -> Result<()> {
    let f = Fixture::new()?;
    let (mid, origin) = f.add(&f.sid, "remembered observation", "reported_progress")?;
    f.cache(&mid, &vector(1.0, 0.0))?;
    f.store.finish_session(&f.sid)?;
    let sid = f.continue_from(&f.sid, &f.sid, "explicit-selection")?;
    let text = "How did that comparison go?";
    let input = f.store.record_education_event(
        &f.lid,
        &sid,
        "simulated",
        "learner_statement",
        &json!({"text":text}),
    )?;
    let session = f.store.session(&sid)?;
    let learner = f.store.learner(&f.lid)?;
    let lexical = build_context(
        &f.store,
        &session,
        &learner,
        text,
        s(&input, "event_id"),
        &json!({}),
    )?;
    let context = build_context_with_semantic(
        &f.store,
        &session,
        &learner,
        text,
        s(&input, "event_id"),
        &json!({}),
        text,
        QueryEmbedding {
            model_key: &f.key,
            values: &vector(1.0, 0.0),
        },
        CandidateRanking::DenseOnly,
    )?;
    ensure!(
        context["task"] == lexical["task"] && context["continuation"] == lexical["continuation"]
    );
    ensure!(context["user_input"] == text && context["slow_memory"][0]["memory_id"] == mid);
    let serialized = context["evidence"].to_string();
    ensure!(
        serialized.contains(s(&origin, "event_id")) && serialized.contains(s(&input, "event_id"))
    );
    ensure!(
        build_context_with_semantic(
            &f.store,
            &session,
            &learner,
            text,
            s(&input, "event_id"),
            &json!({}),
            "other query",
            QueryEmbedding {
                model_key: &f.key,
                values: &vector(1.0, 0.0)
            },
            CandidateRanking::DenseOnly
        )
        .is_err()
    );
    let continuation_text = "continue";
    let input = f.store.record_education_event(
        &f.lid,
        &sid,
        "simulated",
        "learner_statement",
        &json!({"text":continuation_text}),
    )?;
    let query = memory_recall_query(continuation_text, "").unwrap();
    ensure!(query != continuation_text);
    ensure!(
        build_context_with_semantic(
            &f.store,
            &session,
            &learner,
            continuation_text,
            s(&input, "event_id"),
            &json!({}),
            continuation_text,
            QueryEmbedding {
                model_key: &f.key,
                values: &vector(1.0, 0.0)
            },
            CandidateRanking::DenseOnly
        )
        .is_err()
    );
    build_context_with_semantic(
        &f.store,
        &session,
        &learner,
        continuation_text,
        s(&input, "event_id"),
        &json!({}),
        query,
        QueryEmbedding {
            model_key: &f.key,
            values: &vector(1.0, 0.0),
        },
        CandidateRanking::DenseOnly,
    )?;
    Ok(())
}

#[test]
fn source_revision_forget_and_epoch_invalidate_dense_cache_and_late_commit() -> Result<()> {
    for change in ["revision", "forget", "epoch"] {
        let f = Fixture::new()?;
        let (mid, event) = f.add(&f.sid, "recorded observation", "reported_progress")?;
        let snapshot = f.store.embedding_work(&f.lid, &f.key, 32)?.remove(0);
        ensure!(
            f.store
                .commit_memory_embedding(&snapshot, &f.key, &vector(1.0, 0.0))?
        );
        match change {
            "revision" => {
                f.store.revise_education_event(
                    &f.lid,
                    &f.sid,
                    s(&event, "event_id"),
                    "simulated",
                    &json!({"correction":"Correction: observation was wrong"}),
                )?;
            }
            "forget" => {
                f.store.forget(&f.lid, Some(&mid))?;
            }
            "epoch" => {
                f.store.db.execute(
                    "UPDATE learners SET epoch=epoch+1 WHERE learner_id=?",
                    [&f.lid],
                )?;
            }
            _ => unreachable!(),
        }
        ensure!(
            !f.store
                .commit_memory_embedding(&snapshot, &f.key, &vector(1.0, 0.0))?
        );
        ensure!(
            f.ranked(&f.sid, "recorded", CandidateRanking::DenseOnly)?
                .is_empty()
        );
        let ranked = f.ranked(&f.sid, "recorded", CandidateRanking::HybridRrf)?;
        let old = f
            .store
            .recall_candidates_for_session(&f.sid, "recorded", None, 5)?;
        ensure!(serde_json::to_value(ranked)? == serde_json::to_value(old)?);
    }
    Ok(())
}

#[test]
fn dense_candidates_preserve_whole_records_and_fixed_count_and_character_budgets() -> Result<()> {
    let f = Fixture::new()?;
    for index in 0..6 {
        let text = format!("{index}{}", "x".repeat(700));
        let (mid, _) = f.add(&f.sid, &text, "reported_progress")?;
        f.cache(&mid, &vector(1.0, 0.0))?;
    }
    let rows = f.store.recall_candidates_for_session_ranked(
        &f.sid,
        "x",
        QueryEmbedding {
            model_key: &f.key,
            values: &vector(1.0, 0.0),
        },
        50,
        CandidateRanking::DenseOnly,
    )?;
    ensure!(rows.len() == 3);
    ensure!(
        rows.iter()
            .map(|r| s(&r.memory, "content").chars().count())
            .sum::<usize>()
            <= 2400
    );
    for row in rows {
        let original = f
            .store
            .memory_evidence(&f.lid, s(&row.memory, "memory_id"))?;
        ensure!(row.memory["content"] == original["memory"]["content"]);
    }
    Ok(())
}
