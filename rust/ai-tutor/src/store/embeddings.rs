//! Derived local vectors. Live evidence remains authoritative; similarity is
//! candidate ordering, never evidence grade, factual confidence or acceptance.
use super::*;
use crate::memory_embedding::{EmbeddingProfile, cosine};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct MemoryEmbeddingSnapshot {
    pub namespace: String,
    pub learner_id: String,
    pub memory_id: String,
    pub learner_epoch: i64,
    pub model_key: String,
    pub dimension: usize,
    pub memory_digest: String,
    pub source_digest: String,
    pub content: String,
}
/// A runtime-only refusal receipt for one exact source snapshot; no memory text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EmbeddingExclusion {
    pub memory_id: String,
    pub learner_epoch: i64,
    pub memory_digest: String,
    pub source_digest: String,
}
impl From<&MemoryEmbeddingSnapshot> for EmbeddingExclusion {
    fn from(snapshot: &MemoryEmbeddingSnapshot) -> Self {
        Self {
            memory_id: snapshot.memory_id.clone(),
            learner_epoch: snapshot.learner_epoch,
            memory_digest: snapshot.memory_digest.clone(),
            source_digest: snapshot.source_digest.clone(),
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct EmbeddingCoverage {
    pub eligible: usize,
    pub indexed: usize,
}
pub struct EligibleMemoryEmbedding {
    pub snapshot: MemoryEmbeddingSnapshot,
    pub vector: Vec<f32>,
}
pub struct QueryEmbedding<'a> {
    pub model_key: &'a str,
    pub values: &'a [f32],
}
/// Explicit experimental ordering; neither mode establishes factual relevance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateRanking {
    DenseOnly,
    HybridRrf,
}
#[derive(Debug, Serialize)]
pub struct RecallCandidate {
    pub memory: Value,
    pub lexical_rank: Option<usize>,
    pub semantic_rank: Option<usize>,
    pub cosine: Option<f64>,
    pub ranking_score: f64,
    pub relevance_status: &'static str,
}

fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
fn allowed_memory(memory: &Value) -> bool {
    s(memory, "activity_scope") == "modular_rover"
        && (s(memory, "scope") == "current_activity"
            || (s(memory, "scope") == "cross_session_explicit"
                && s(memory, "kind") == "help_preference"))
}
fn decode_vector(bytes: &[u8], profile: EmbeddingProfile) -> Option<Vec<f32>> {
    if bytes.len() != profile.dimension() * 4 {
        return None;
    }
    let vector: Vec<_> = bytes
        .chunks_exact(4)
        .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
        .collect();
    profile.normalized(&vector).then_some(vector)
}

const CREATE_CACHE: &str = "CREATE TABLE IF NOT EXISTS memory_embeddings(
    namespace TEXT NOT NULL,learner_id TEXT NOT NULL,memory_id TEXT NOT NULL,
    model_key TEXT NOT NULL,learner_epoch INTEGER NOT NULL,
    memory_digest TEXT NOT NULL,source_digest TEXT NOT NULL,
    dimension INTEGER NOT NULL DEFAULT 384 CHECK(dimension IN (384,1024)),
    vector BLOB NOT NULL CHECK(length(vector)=dimension*4),
    PRIMARY KEY(namespace,learner_id,memory_id,model_key));";

const CACHE_TRIGGERS: &[&str] = &[
    "embedding_memory_deleted",
    "embedding_memory_changed",
    "embedding_learner_epoch",
    "embedding_learner_deleted",
    "embedding_link_changed",
    "embedding_link_added",
    "embedding_link_deleted",
    "embedding_source_changed",
    "embedding_source_deleted",
    "embedding_session_deleted",
];

impl Store {
    pub(super) fn migrate_memory_embeddings(&self) -> Result<()> {
        // Store::open already owns one migration transaction. Rebuild only this
        // derivative table; no memory/evidence rows are rewritten or backfilled.
        let columns = self.rows("PRAGMA table_info(memory_embeddings)", [])?;
        if !columns.is_empty() && !columns.iter().any(|row| s(row, "name") == "dimension") {
            // SQLite renames trigger references along with tables. Remove our
            // known triggers first, then recreate them against the final name.
            for name in CACHE_TRIGGERS {
                self.db
                    .execute_batch(&format!("DROP TRIGGER IF EXISTS {name}"))?;
            }
            self.db.execute_batch(
                "ALTER TABLE memory_embeddings RENAME TO memory_embeddings_384_previous",
            )?;
            self.db.execute_batch(CREATE_CACHE)?;
            self.db.execute_batch("INSERT INTO memory_embeddings(namespace,learner_id,memory_id,model_key,learner_epoch,memory_digest,source_digest,dimension,vector)
                SELECT namespace,learner_id,memory_id,model_key,learner_epoch,memory_digest,source_digest,384,vector FROM memory_embeddings_384_previous;
                DROP TABLE memory_embeddings_384_previous;")?;
        } else {
            self.db.execute_batch(CREATE_CACHE)?;
        }
        self.db.execute_batch("
            CREATE TRIGGER IF NOT EXISTS embedding_memory_deleted AFTER DELETE ON memories BEGIN
                DELETE FROM memory_embeddings WHERE memory_id=OLD.memory_id;
            END;
            CREATE TRIGGER IF NOT EXISTS embedding_memory_changed AFTER UPDATE ON memories BEGIN
                DELETE FROM memory_embeddings WHERE memory_id=OLD.memory_id;
            END;
            CREATE TRIGGER IF NOT EXISTS embedding_learner_epoch AFTER UPDATE OF epoch ON learners
            WHEN NEW.epoch!=OLD.epoch BEGIN
                DELETE FROM memory_embeddings WHERE learner_id=OLD.learner_id;
            END;
            CREATE TRIGGER IF NOT EXISTS embedding_learner_deleted AFTER DELETE ON learners BEGIN
                DELETE FROM memory_embeddings WHERE learner_id=OLD.learner_id;
            END;
            CREATE TRIGGER IF NOT EXISTS embedding_link_changed AFTER UPDATE ON memory_source_links BEGIN
                DELETE FROM memory_embeddings WHERE memory_id IN(OLD.memory_id,NEW.memory_id);
            END;
            CREATE TRIGGER IF NOT EXISTS embedding_link_added AFTER INSERT ON memory_source_links BEGIN
                DELETE FROM memory_embeddings WHERE memory_id=NEW.memory_id;
            END;
            CREATE TRIGGER IF NOT EXISTS embedding_link_deleted AFTER DELETE ON memory_source_links BEGIN
                DELETE FROM memory_embeddings WHERE memory_id=OLD.memory_id;
            END;
            CREATE TRIGGER IF NOT EXISTS embedding_source_changed AFTER UPDATE ON education_event_meta BEGIN
                DELETE FROM memory_embeddings WHERE memory_id IN
                    (SELECT memory_id FROM memory_source_links WHERE event_id=OLD.event_id);
            END;
            CREATE TRIGGER IF NOT EXISTS embedding_source_deleted AFTER DELETE ON education_event_meta BEGIN
                DELETE FROM memory_embeddings WHERE memory_id IN
                    (SELECT memory_id FROM memory_source_links WHERE event_id=OLD.event_id);
            END;
            CREATE TRIGGER IF NOT EXISTS embedding_session_deleted AFTER DELETE ON sessions BEGIN
                DELETE FROM memory_embeddings WHERE memory_id IN
                    (SELECT l.memory_id FROM memory_source_links l JOIN events e ON e.event_id=l.event_id WHERE e.session_id=OLD.session_id);
            END;
        ")?;
        let wrong: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM memory_embeddings WHERE namespace!=?",
            [&self.namespace],
            |row| row.get(0),
        )?;
        ensure!(wrong == 0, "向量缓存空间不一致");
        Ok(())
    }

    fn memory_embedding_snapshot(
        &self,
        lid: &str,
        mid: &str,
        model_key: &str,
    ) -> Result<Option<MemoryEmbeddingSnapshot>> {
        let Some(profile) = EmbeddingProfile::from_model_key(model_key) else {
            return Ok(None);
        };
        let Some(learner) = self.one("SELECT epoch FROM learners WHERE learner_id=?", [lid])?
        else {
            return Ok(None);
        };
        let evidence = match self.memory_evidence(lid, mid) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        let memory = &evidence["memory"];
        if !allowed_memory(memory) || evidence["comparison_context"]["status"] == "unavailable" {
            return Ok(None);
        }
        let mut links = evidence["source_links"].as_array().unwrap().clone();
        links.sort_by(|a, b| s(a, "event_id").cmp(s(b, "event_id")));
        Ok(Some(MemoryEmbeddingSnapshot {
            namespace: self.namespace.clone(),
            learner_id: lid.to_owned(),
            memory_id: mid.to_owned(),
            learner_epoch: learner["epoch"].as_i64().unwrap(),
            model_key: model_key.to_owned(),
            dimension: profile.dimension(),
            memory_digest: digest(memory),
            source_digest: digest(&json!(links)),
            content: s(memory, "content").to_owned(),
        }))
    }

    /// Take this small receipt under the Store lock, then encode its content
    /// after releasing the lock. No model is loaded or run by Store methods.
    pub fn embedding_work(
        &self,
        lid: &str,
        model_key: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEmbeddingSnapshot>> {
        self.embedding_work_excluding(lid, model_key, limit, &[])
    }

    /// Only an unchanged snapshot can remain excluded after a confirmed
    /// TooLong result. Skipped rows do not consume the requested batch limit.
    pub fn embedding_work_excluding(
        &self,
        lid: &str,
        model_key: &str,
        limit: usize,
        exclusions: &[EmbeddingExclusion],
    ) -> Result<Vec<MemoryEmbeddingSnapshot>> {
        ensure!(
            EmbeddingProfile::from_model_key(model_key).is_some(),
            "向量模型profile未注册"
        );
        ensure!((1..=32).contains(&limit), "每批向量索引数量必须为1至32");
        ensure!(exclusions.len() <= 128, "向量暂缓快照最多128条");
        self.learner(lid)?;
        let current: HashSet<_> = self
            .eligible_memory_embeddings(lid, model_key)?
            .into_iter()
            .map(|row| row.snapshot.memory_id)
            .collect();
        let mut work = vec![];
        for memory in self.active_memories(lid)? {
            let mid = s(&memory, "memory_id");
            if !current.contains(mid) {
                if let Some(snapshot) = self.memory_embedding_snapshot(lid, mid, model_key)? {
                    if !exclusions.contains(&EmbeddingExclusion::from(&snapshot)) {
                        work.push(snapshot)
                    }
                }
            }
            if work.len() >= limit {
                break;
            }
        }
        Ok(work)
    }

    /// Count the exact teaching recall scope, including selected ancestors and
    /// separately authorized preferences. TooLong remains eligible but unindexed.
    pub fn embedding_coverage_for_session(&self, sid: &str, model_key: &str) -> Result<EmbeddingCoverage> {
        ensure!(EmbeddingProfile::from_model_key(model_key).is_some(), "向量模型profile未注册");
        self.tx(|| {
            let (lid, activities) = self.session_memory_activities(sid)?;
            let indexed: HashSet<_> = self.eligible_memory_embeddings(&lid, model_key)?
                .into_iter().map(|row| row.snapshot.memory_id).collect();
            let mut coverage = EmbeddingCoverage::default();
            for memory in self.active_memories(&lid)? {
                if !allowed_memory(&memory) || !self.memory_in_activity(&lid, &memory, &activities)? {
                    continue;
                }
                let mid = s(&memory, "memory_id");
                if self.memory_embedding_snapshot(&lid, mid, model_key)?.is_some() {
                    coverage.eligible += 1;
                    coverage.indexed += usize::from(indexed.contains(mid));
                }
            }
            Ok(coverage)
        })
    }

    /// Explicit long-term help preferences are instructions about presentation,
    /// not facts competing for cosine rank. Filter scope before the bounded list.
    pub fn explicit_help_preferences_for_session(&self, sid: &str) -> Result<Vec<Value>> {
        self.tx(|| {
            let (lid, activities) = self.session_memory_activities(sid)?;
            let mut preferences = Vec::new();
            for memory in self.active_memories(&lid)? {
                if memory["kind"] != "help_preference" || memory["scope"] != "cross_session_explicit"
                    || !self.memory_in_activity(&lid, &memory, &activities)? {
                    continue;
                }
                // Recheck live source links and source grades just as the final
                // context does; never promote a stored summary's asserted grade.
                if self.memory_evidence(&lid, s(&memory,"memory_id")).is_ok() {
                    preferences.push(memory);
                }
            }
            preferences.sort_by(|a,b| s(b,"updated_at").cmp(s(a,"updated_at"))
                .then_with(||s(a,"memory_id").cmp(s(b,"memory_id"))));
            let mut result = Vec::new();
            let mut remaining = 2400usize;
            for memory in preferences {
                if result.len() == 5 { break; }
                let size = s(&memory,"content").chars().count();
                if size <= remaining {
                    remaining -= size;
                    result.push(memory);
                }
            }
            Ok(result)
        })
    }

    /// CAS against live evidence in the same transaction as the insert.
    /// A result computed before deletion/correction/content change is stale.
    pub fn commit_memory_embedding(
        &self,
        snapshot: &MemoryEmbeddingSnapshot,
        model_key: &str,
        vector: &[f32],
    ) -> Result<bool> {
        if snapshot.namespace != self.namespace || snapshot.model_key != model_key {
            return Ok(false);
        }
        let profile = EmbeddingProfile::from_model_key(model_key)
            .ok_or_else(|| anyhow!("向量模型profile未注册"))?;
        ensure!(
            profile.normalized(vector),
            "向量维度与profile不符，或不是有限的L2归一化值"
        );
        if snapshot.dimension != profile.dimension() {
            return Ok(false);
        }
        self.tx(|| {
            let Some(current)=self.memory_embedding_snapshot(&snapshot.learner_id,&snapshot.memory_id,model_key)? else {return Ok(false)};
            if current.learner_epoch!=snapshot.learner_epoch || current.memory_digest!=snapshot.memory_digest
                || current.source_digest!=snapshot.source_digest || current.content!=snapshot.content {return Ok(false)}
            let bytes:Vec<u8>=vector.iter().flat_map(|value|value.to_le_bytes()).collect();
            self.db.execute("INSERT INTO memory_embeddings(namespace,learner_id,memory_id,model_key,learner_epoch,memory_digest,source_digest,dimension,vector) VALUES(?,?,?,?,?,?,?,?,?)
                ON CONFLICT(namespace,learner_id,memory_id,model_key) DO UPDATE SET learner_epoch=excluded.learner_epoch,memory_digest=excluded.memory_digest,source_digest=excluded.source_digest,dimension=excluded.dimension,vector=excluded.vector",
                params![self.namespace,snapshot.learner_id,snapshot.memory_id,model_key,snapshot.learner_epoch,snapshot.memory_digest,snapshot.source_digest,profile.dimension(),bytes])?;
            Ok(true)
        })
    }

    /// The vector table is never authoritative. Re-check every returned vector
    /// against current owner, epoch, scope, content and active source versions.
    pub fn eligible_memory_embeddings(
        &self,
        lid: &str,
        model_key: &str,
    ) -> Result<Vec<EligibleMemoryEmbedding>> {
        self.learner(lid)?;
        let Some(profile) = EmbeddingProfile::from_model_key(model_key) else {
            return Ok(vec![]);
        };
        let mut statement=self.db.prepare("SELECT memory_id,learner_epoch,memory_digest,source_digest,dimension,vector FROM memory_embeddings WHERE namespace=? AND learner_id=? AND model_key=?")?;
        let rows = statement
            .query_map(params![self.namespace, lid, model_key], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, usize>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut valid = vec![];
        for (mid, epoch, memory_digest, source_digest, dimension, bytes) in rows {
            let Some(snapshot) = self.memory_embedding_snapshot(lid, &mid, model_key)? else {
                continue;
            };
            if snapshot.learner_epoch != epoch
                || snapshot.memory_digest != memory_digest
                || snapshot.source_digest != source_digest
                || dimension != profile.dimension()
            {
                continue;
            }
            if let Some(vector) = decode_vector(&bytes, profile) {
                valid.push(EligibleMemoryEmbedding { snapshot, vector })
            }
        }
        Ok(valid)
    }

    /// Opt-in candidate experiment only. No default context uses this method.
    /// All results need relevance review; cosine is not a confidence threshold.
    pub fn recall_candidates(
        &self,
        lid: &str,
        query: &str,
        embedding: Option<QueryEmbedding<'_>>,
        top_k: usize,
    ) -> Result<Vec<RecallCandidate>> {
        self.recall_candidates_in_activity(
            lid,
            query,
            embedding,
            top_k,
            None,
            CandidateRanking::HybridRrf,
        )
    }

    /// Session-bound counterpart for teaching. Scope is checked before either
    /// top-five list is made, so unrelated activities cannot consume its budget.
    pub fn recall_candidates_for_session(
        &self,
        sid: &str,
        query: &str,
        embedding: Option<QueryEmbedding<'_>>,
        top_k: usize,
    ) -> Result<Vec<RecallCandidate>> {
        self.tx(|| {
            let (lid, activities) = self.session_memory_activities(sid)?;
            self.recall_candidates_in_activity(
                &lid,
                query,
                embedding,
                top_k,
                Some(&activities),
                CandidateRanking::HybridRrf,
            )
        })
    }

    /// Strict, opt-in semantic entry. An invalid query never silently becomes
    /// lexical retrieval, including when this session has no cached vectors.
    pub fn recall_candidates_for_session_ranked(
        &self,
        sid: &str,
        query: &str,
        embedding: QueryEmbedding<'_>,
        top_k: usize,
        ranking: CandidateRanking,
    ) -> Result<Vec<RecallCandidate>> {
        ensure!(
            EmbeddingProfile::from_model_key(embedding.model_key)
                .is_some_and(|profile| profile.normalized(embedding.values)),
            "Semantic query must match a registered normalized embedding profile"
        );
        self.tx(|| {
            let (lid, activities) = self.session_memory_activities(sid)?;
            self.recall_candidates_in_activity(
                &lid,
                query,
                Some(embedding),
                top_k,
                Some(&activities),
                ranking,
            )
        })
    }

    fn recall_candidates_in_activity(
        &self,
        lid: &str,
        query: &str,
        embedding: Option<QueryEmbedding<'_>>,
        top_k: usize,
        activity: Option<&HashSet<String>>,
        ranking: CandidateRanking,
    ) -> Result<Vec<RecallCandidate>> {
        // A short local transaction keeps ranking and final evidence checks on
        // one snapshot. The query vector was computed before entering here.
        self.tx(|| {
            let lexical: Vec<_> = if ranking == CandidateRanking::HybridRrf {
                self.recall_in_activity(lid, query, 5, activity)?
                    .into_iter()
                    .filter(allowed_memory)
                    .filter(|memory| self.memory_evidence(lid, s(memory, "memory_id")).is_ok())
                    .collect()
            } else {
                Vec::new()
            };
            let mut candidates: HashMap<String, RecallCandidate> = HashMap::new();
            for (rank, memory) in lexical.into_iter().enumerate() {
                candidates.insert(
                    s(&memory, "memory_id").to_owned(),
                    RecallCandidate {
                        memory,
                        lexical_rank: Some(rank + 1),
                        semantic_rank: None,
                        cosine: None,
                        ranking_score: 1.0 / (61.0 + rank as f64),
                        relevance_status: "candidate_requires_relevance_review",
                    },
                );
            }
            if let Some(query) = embedding.filter(|query| {
                EmbeddingProfile::from_model_key(query.model_key)
                    .is_some_and(|profile| profile.normalized(query.values))
            }) {
                let mut dense = Vec::new();
                for row in self.eligible_memory_embeddings(lid, query.model_key)? {
                    let evidence = self.memory_evidence(lid, &row.snapshot.memory_id)?;
                    if !allowed_memory(&evidence["memory"]) {
                        continue;
                    }
                    if let Some(activity) = activity {
                        if !self.memory_in_activity(lid, &evidence["memory"], activity)? {
                            continue;
                        }
                    }
                    dense.push((cosine(query.values, &row.vector), row.snapshot.memory_id));
                }
                dense.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
                for (rank, (similarity, mid)) in dense.into_iter().take(5).enumerate() {
                    let Ok(evidence) = self.memory_evidence(lid, &mid) else {
                        continue;
                    };
                    let candidate = candidates.entry(mid).or_insert_with(|| RecallCandidate {
                        memory: evidence["memory"].clone(),
                        lexical_rank: None,
                        semantic_rank: None,
                        cosine: None,
                        ranking_score: 0.0,
                        relevance_status: "candidate_requires_relevance_review",
                    });
                    candidate.semantic_rank = Some(rank + 1);
                    candidate.cosine = Some(similarity);
                    candidate.ranking_score = match ranking {
                        CandidateRanking::DenseOnly => similarity,
                        CandidateRanking::HybridRrf => {
                            candidate.ranking_score + 1.0 / (61.0 + rank as f64)
                        }
                    };
                }
            }
            let mut ranked: Vec<_> = candidates.into_values().collect();
            ranked.sort_by(|a, b| {
                b.ranking_score
                    .total_cmp(&a.ranking_score)
                    .then_with(|| s(&a.memory, "memory_id").cmp(s(&b.memory, "memory_id")))
            });
            let mut result = vec![];
            let mut remaining = 2400usize;
            for candidate in ranked {
                if result.len() >= top_k.min(5) {
                    break;
                }
                let size = s(&candidate.memory, "content").chars().count();
                if size <= remaining
                    && self
                        .memory_evidence(lid, s(&candidate.memory, "memory_id"))
                        .is_ok()
                {
                    remaining -= size;
                    result.push(candidate);
                }
            }
            Ok(result)
        })
    }
}
