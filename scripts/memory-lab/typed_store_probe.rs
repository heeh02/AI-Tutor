//! Frozen typed vectors through actual Store provenance, CAS and retrieval.
//! No encoder/model/device/user database. Original experiment files are immutable.
use ai_tutor::{
    memory_embedding::{EmbeddingProfile, cosine},
    store::{MemoryJobLimits, QueryEmbedding, Store, s, uid},
};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

const METHOD: &str = include_str!("typed_store_method_v1.json");
const PROFILE: EmbeddingProfile = EmbeddingProfile::BgeM3NativeFp32;
const CONDITIONS: [&str; 2] = ["current_query", "query_plus_goal"];
const METHODS: [&str; 3] = [
    "store_legacy_lexical",
    "pure_eligible_cosine",
    "actual_store_rrf",
];
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    ensure!(
        fs::metadata(path)?.len() < 20 * 1024 * 1024,
        "Input exceeds bound"
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
struct Frozen {
    decisions: Vec<Value>,
    queries: Vec<Value>,
    labels: HashMap<String, Value>,
    inputs: HashMap<String, Value>,
    vectors: HashMap<String, Vec<f32>>,
    hashes: Value,
}
fn load(root: &Path) -> Result<Frozen> {
    let method: Value = serde_json::from_str(METHOD)?;
    let hashes = method["inputs"].clone();
    let base = root.join("runtime/goal-baselines/20261104");
    let projection: Value = serde_json::from_slice(&checked(
        &base.join("typed-projection-first/report.json"),
        s(&hashes, "projection_report"),
    )?)?;
    let encoding: Value = serde_json::from_slice(&checked(
        &base.join("typed-encode-first/report.json"),
        s(&hashes, "encoding_report"),
    )?)?;
    ensure!(
        projection["status"] == "projection_completed_no_encoding"
            && projection["encoder_inputs"]["sha256"] == hashes["encoder_inputs"]
            && projection["projection"]["encoder_profile"] == PROFILE.metadata(),
        "Projection identity mismatch"
    );
    ensure!(
        encoding["status"] == "encoding_completed_no_retrieval"
            && encoding["projection_report_sha256"] == hashes["projection_report"]
            && encoding["projection_inputs_sha256"] == hashes["encoder_inputs"]
            && encoding["encodings_sha256"] == hashes["vectors"]
            && encoding["profile"] == PROFILE.metadata()
            && encoding["encoded_count"] == 234,
        "Encoding identity mismatch"
    );
    let fixture = root.join("rust/ai-tutor/tests/fixtures/diagnostic_postfreeze_v2");
    let manifest: Value = serde_json::from_slice(&checked(
        &fixture.join("manifest.json"),
        s(&hashes, "fixture_manifest"),
    )?)?;
    let mut fixture_rows = HashMap::new();
    for name in ["sources", "queries", "labels", "scope_controls"] {
        let file = format!("{name}.jsonl");
        fixture_rows.insert(
            name,
            jsonl(&checked(
                &fixture.join(&file),
                s(&manifest["files"][&file], "sha256"),
            )?)?,
        );
    }
    let decisions = projection["projection"]["decisions"]
        .as_array()
        .ok_or_else(|| anyhow!("Missing decisions"))?
        .clone();
    ensure!(
        decisions.len() == 60
            && decisions.iter().filter(|d| d["projected"] == true).count() == 42
            && decisions
                .iter()
                .zip(&fixture_rows["sources"])
                .all(|(d, source)| d["original_source"] == *source),
        "Projection corpus changed"
    );
    let queries = fixture_rows.remove("queries").unwrap();
    let labels = fixture_rows
        .remove("labels")
        .unwrap()
        .into_iter()
        .map(|v| (s(&v, "query_id").to_owned(), v))
        .collect::<HashMap<_, _>>();
    ensure!(
        queries.len() == 96 && labels.len() == 96 && fixture_rows["scope_controls"].len() == 12,
        "Incomplete original labels/queries"
    );
    let input_rows = jsonl(&checked(
        &base.join("typed-projection-first/encoder_inputs.jsonl"),
        s(&hashes, "encoder_inputs"),
    )?)?;
    let inputs = input_rows
        .iter()
        .map(|v| (s(v, "key").to_owned(), v.clone()))
        .collect::<HashMap<_, _>>();
    ensure!(
        input_rows.len() == 234 && inputs.len() == 234,
        "Incomplete/duplicate input keys"
    );
    for d in decisions.iter().filter(|d| d["projected"] == true) {
        let row = &inputs[&format!("source:{}", s(d, "source_id"))];
        ensure!(
            row["text"] == d["encoding_text"]
                && row["text"] == d["memory_evidence"]["memory"]["content"]
                && row["text_sha256"] == hash(s(row, "text").as_bytes()),
            "Projected input mismatch"
        );
    }
    for condition in CONDITIONS {
        for q in &queries {
            let expected = if condition == "current_query" {
                s(q, "text").to_owned()
            } else {
                format!("{} {} ", s(q, "text"), s(q, "goal"))
            };
            let row = &inputs[&format!("{condition}:{}", s(q, "id"))];
            ensure!(
                s(row, "text") == expected && row["text_sha256"] == hash(expected.as_bytes()),
                "Original query rewritten"
            );
        }
    }
    let encoded: Vec<Value> = serde_json::from_slice(&checked(
        &base.join("typed-encode-first/bge_m3-encodings.json"),
        s(&hashes, "vectors"),
    )?)?;
    let mut vectors = HashMap::new();
    for row in &encoded {
        let key = s(row, "key");
        let input = inputs
            .get(key)
            .ok_or_else(|| anyhow!("Unexpected vector key"))?;
        ensure!(
            row["text_sha256"] == input["text_sha256"],
            "Vector text binding changed"
        );
        let vector = row["encoding"]["pooled_vector"]
            .as_array()
            .ok_or_else(|| anyhow!("Missing vector"))?
            .iter()
            .map(|v| {
                v.as_f64()
                    .map(|v| v as f32)
                    .ok_or_else(|| anyhow!("Invalid float"))
            })
            .collect::<Result<Vec<_>>>()?;
        let bytes: Vec<_> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
        ensure!(
            PROFILE.normalized(&vector) && row["encoding"]["pooled_sha256_f32le"] == hash(&bytes),
            "Vector dimension/norm/hash mismatch"
        );
        ensure!(
            vectors.insert(key.to_owned(), vector).is_none(),
            "Duplicate encoded key"
        );
    }
    ensure!(
        encoded.len() == 234 && vectors.len() == 234,
        "Incomplete encoded vectors"
    );
    Ok(Frozen {
        decisions,
        queries,
        labels,
        inputs,
        vectors,
        hashes,
    })
}
struct OwnedDirectory(PathBuf);
impl Drop for OwnedDirectory {
    fn drop(&mut self) {
        for n in ["store.sqlite3", "store.sqlite3-wal", "store.sqlite3-shm"] {
            let _ = fs::remove_file(self.0.join(n));
        }
        let _ = fs::remove_dir(&self.0);
    }
}
struct Fixture {
    store: Store,
    _directory: OwnedDirectory,
    lid: String,
    by_memory: HashMap<String, String>,
    events: HashMap<String, Value>,
    bindings: Vec<Value>,
}
fn same_projection(actual: &Value, expected: &Value) -> Result<()> {
    for field in [
        "content",
        "kind",
        "scope",
        "activity_scope",
        "source_type",
        "status",
        "epoch",
    ] {
        ensure!(
            actual["memory"][field] == expected["memory"][field],
            "Actual memory field changed: {field}"
        );
    }
    ensure!(
        actual["education_metadata"] == expected["education_metadata"]
            && actual["namespace"] == expected["namespace"],
        "Typed grade/support/importance changed"
    );
    ensure!(
        actual["sources"].as_array().is_some_and(|v| v.len() == 1)
            && actual["source_links"]
                .as_array()
                .is_some_and(|v| v.len() == 1),
        "Expected one live source"
    );
    for field in [
        "content",
        "event_kind",
        "event_status",
        "event_version",
        "source_type",
    ] {
        ensure!(
            actual["sources"][0][field] == expected["sources"][0][field],
            "Actual source field changed: {field}"
        );
    }
    ensure!(
        actual["memory"]["source_event_ids"] == json!([actual["sources"][0]["event_id"]])
            && actual["source_links"][0]["event_id"] == actual["sources"][0]["event_id"]
            && actual["source_links"][0]["event_version"] == actual["sources"][0]["event_version"],
        "Live source links differ"
    );
    Ok(())
}
fn promote(
    store: &Store,
    lid: &str,
    sid: &str,
    batch: &[(&Value, Value)],
    clock: i64,
) -> Result<Vec<Value>> {
    let ids: Vec<_> = batch
        .iter()
        .map(|(_, e)| s(e, "event_id").to_owned())
        .collect();
    let limits = MemoryJobLimits::default();
    let job =
        store.enqueue_memory_job(lid, sid, &ids, "synthetic-typed-store-v1", &limits, clock)?;
    let claim = store
        .claim_memory_job("synthetic-typed-store", &limits, clock)?
        .ok_or_else(|| anyhow!("No synthetic claim"))?;
    ensure!(claim["job"]["job_id"] == job["job_id"], "Wrong job");
    let proposals:Vec<_>=batch.iter().map(|(d,event)|{let source=&d["original_source"];let eid=s(event,"event_id");let mut p=json!({"kind":d["projection_kind"],"claim_type":source["claim_type"],"content":source["text"],"scope":"current_activity","source_event_ids":[eid],"source_event_versions":{},"source_quotes":[{"event_id":eid,"quote":source["text"]}],"support_level":source["support_level"],"importance":{"goal_relevance":0,"continuity":0,"novel_change":0,"explicit_request":0}});p["source_event_versions"][eid]=event["event_version"].clone();p}).collect();
    let staged = store.stage_memory_candidates(&claim["receipt"], &proposals, clock + 1)?;
    ensure!(staged.len() == batch.len(), "Candidate count changed");
    let mut result = vec![];
    for ((d, _), candidate) in batch.iter().zip(&staged) {
        let actual = store.apply_memory_candidate(
            lid,
            s(candidate, "candidate_id"),
            &[s(d, "projection_kind")],
            clock + 2,
        )?;
        same_projection(&actual, &d["memory_evidence"])?;
        result.push(actual);
    }
    ensure!(
        store.memory_job_status(lid, s(&job, "job_id"))?["status"] == "applied",
        "Projection job incomplete"
    );
    Ok(result)
}
impl Fixture {
    fn rebuild(frozen: &Frozen) -> Result<Self> {
        let path = std::env::temp_dir().join(uid("ai-tutor-typed-store"));
        fs::create_dir(&path)?;
        let directory = OwnedDirectory(path.clone());
        let store = Store::open(path.join("store.sqlite3"), "simulator")?;
        let lid = s(
            &store.create_learner("SYNTHETIC_TYPED_STORE", "zh", "guided")?,
            "learner_id",
        )
        .to_owned();
        let mut packs = vec![];
        for d in &frozen.decisions {
            let p = s(&d["original_source"], "pack_id");
            if !packs.contains(&p) {
                packs.push(p);
            }
        }
        let mut f = Self {
            store,
            _directory: directory,
            lid,
            by_memory: HashMap::new(),
            events: HashMap::new(),
            bindings: vec![],
        };
        let mut clock = 1_793_721_600_000;
        for pack in packs {
            let sid = s(&f.store.create_session(&f.lid)?, "session_id").to_owned();
            let mut supported = vec![];
            for d in frozen
                .decisions
                .iter()
                .filter(|d| s(&d["original_source"], "pack_id") == pack)
            {
                let source = &d["original_source"];
                let event = f.store.record_education_event(
                    &f.lid,
                    &sid,
                    s(source, "source_type"),
                    s(source, "event_kind"),
                    &d["live_imported_event"]["content"],
                )?;
                ensure!(
                    event["content"] == d["live_imported_event"]["content"]
                        && event["event_version"] == d["event_version"],
                    "Source import drift"
                );
                f.events.insert(s(d, "source_id").into(), event.clone());
                if d["projected"] == true {
                    supported.push((d, event));
                }
            }
            for batch in supported.chunks(5) {
                for ((d, _), evidence) in batch
                    .iter()
                    .zip(promote(&f.store, &f.lid, &sid, batch, clock)?)
                {
                    let mid = s(&evidence["memory"], "memory_id").to_owned();
                    let source = s(d, "source_id").to_owned();
                    f.bindings.push(json!({"source_id":source,"frozen_memory_id":d["memory_id"],"memory_id":mid,"session_id":sid,"event_id":evidence["sources"][0]["event_id"],"event_version":evidence["sources"][0]["event_version"],"text_sha256":hash(s(&evidence["memory"],"content").as_bytes()),"memory_evidence":evidence}));
                    f.by_memory.insert(mid, source);
                }
                clock += 10;
            }
            let state = f.store.session(&sid)?;
            ensure!(
                state["state"]["reported_result"] == "" && state["state"]["attempts"] == json!([]),
                "Projection invented completed work"
            );
        }
        ensure!(
            f.by_memory.len() == 42 && f.store.memories(&f.lid)?.len() == 42,
            "Expected 42 rebuilt typed memories"
        );
        Ok(f)
    }
    fn index(&self, frozen: &Frozen) -> Result<Value> {
        let key = PROFILE.model_key();
        let mut seen = HashSet::new();
        let mut batches = vec![];
        for _ in 0..3 {
            let batch = self.store.embedding_work(&self.lid, &key, 32)?;
            if batch.is_empty() {
                break;
            }
            batches.push(batch.len());
            for work in batch {
                ensure!(seen.insert(work.memory_id.clone()), "Index batch repeated");
                let source = &self.by_memory[&work.memory_id];
                let input = &frozen.inputs[&format!("source:{source}")];
                ensure!(
                    work.content == s(input, "text")
                        && work.dimension == 1024
                        && work.model_key == key,
                    "Index snapshot differs from encoded input"
                );
                ensure!(
                    self.store.commit_memory_embedding(
                        &work,
                        &key,
                        &frozen.vectors[&format!("source:{source}")]
                    )?,
                    "Typed vector CAS rejected"
                );
            }
        }
        ensure!(
            seen.len() == 42
                && batches == [32, 10]
                && self.store.embedding_work(&self.lid, &key, 32)?.is_empty()
                && self
                    .store
                    .eligible_memory_embeddings(&self.lid, &key)?
                    .len()
                    == 42,
            "Incomplete 42-vector Store coverage"
        );
        for cached in self.store.eligible_memory_embeddings(&self.lid, &key)? {
            let input_key = format!("source:{}", self.by_memory[&cached.snapshot.memory_id]);
            ensure!(
                cached.vector == frozen.vectors[&input_key]
                    && cached.snapshot.content == s(&frozen.inputs[&input_key], "text"),
                "Cached vector/text round trip differs from frozen input"
            );
        }
        Ok(
            json!({"batch_sizes":batches,"committed_unique":seen.len(),"eligible":42,"remaining_work":0}),
        )
    }
}
fn evidence_sets(label: &Value) -> Vec<BTreeSet<String>> {
    label["sufficient_evidence_sets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|set| {
            set.as_array()
                .unwrap()
                .iter()
                .map(|id| id.as_str().unwrap().to_owned())
                .collect()
        })
        .collect()
}
fn measure(ids: &[String], sets: &[BTreeSet<String>]) -> Value {
    if sets.is_empty() {
        return json!({"answerable":false,"candidates_returned":ids.len(),"abstention_tested":false});
    }
    let selected: BTreeSet<_> = ids.iter().take(5).cloned().collect();
    let first = ids
        .iter()
        .take(5)
        .position(|id| sets.iter().any(|set| set.contains(id)));
    json!({"answerable":true,"complete_evidence_at_5":sets.iter().any(|set|set.is_subset(&selected)),"reciprocal_rank_at_5":first.map_or(0.0,|i|1.0/(i+1) as f64),"best_set_coverage_at_5":sets.iter().map(|set|set.intersection(&selected).count() as f64/set.len() as f64).fold(0.0,f64::max)})
}
fn whole_selection(ranking: &[Value]) -> Value {
    let mut ids = vec![];
    let mut skipped = vec![];
    let mut used = 0;
    for row in ranking {
        if ids.len() == 5 {
            break;
        }
        let size = row["whole_chars"].as_u64().unwrap() as usize;
        if used + size <= 2400 {
            ids.push(row["source_id"].clone());
            used += size;
        } else {
            skipped.push(row["source_id"].clone());
        }
    }
    json!({"source_ids":ids,"whole_chars":used,"skipped_for_budget":skipped})
}
fn selected_ids(selection: &Value) -> Vec<String> {
    selection["source_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().into())
        .collect()
}
fn ties(rows: &[Value], score: &str) -> Vec<Value> {
    let mut groups = vec![];
    let mut at = 0;
    while at < rows.len() {
        let mut end = at + 1;
        while end < rows.len() && rows[end][score] == rows[at][score] {
            end += 1;
        }
        if end - at > 1 {
            groups.push(json!({"score":rows[at][score],"first_rank":at+1,"source_ids":rows[at..end].iter().map(|r|&r["source_id"]).collect::<Vec<_>>()}));
        }
        at = end;
    }
    groups
}
fn store_rank(
    f: &Fixture,
    text: &str,
    embedding: Option<QueryEmbedding<'_>>,
) -> Result<Vec<Value>> {
    let mut rows = vec![];
    let mut size = 0;
    for c in f.store.recall_candidates(&f.lid, text, embedding, 5)? {
        let mid = s(&c.memory, "memory_id");
        let evidence = f.store.memory_evidence(&f.lid, mid)?;
        ensure!(
            c.memory == evidence["memory"],
            "Returned candidate is not whole live memory"
        );
        let chars = s(&c.memory, "content").chars().count();
        size += chars;
        rows.push(json!({"source_id":f.by_memory[mid],"memory_id":mid,"whole_chars":chars,"lexical_rank":c.lexical_rank,"semantic_rank":c.semantic_rank,"cosine":c.cosine,"ranking_score":c.ranking_score,"relevance_status":c.relevance_status}));
    }
    ensure!(rows.len() <= 5 && size <= 2400, "Store budget exceeded");
    Ok(rows)
}
fn summary(rows: &[&Value], method: &str) -> Value {
    let positive: Vec<_> = rows
        .iter()
        .filter(|r| r[method]["metrics"]["answerable"] == true)
        .collect();
    let available: Vec<_> = positive
        .iter()
        .filter(|r| r["complete_set_available"] == true)
        .collect();
    let negative: Vec<_> = rows
        .iter()
        .filter(|r| r[method]["metrics"]["answerable"] == false)
        .collect();
    let complete = |items: &[&&Value]| {
        items
            .iter()
            .filter(|r| r[method]["metrics"]["complete_evidence_at_5"] == true)
            .count()
    };
    json!({"original_positive_denominator":positive.len(),"available_positive_denominator":available.len(),"complete_original_evidence_at_5":complete(&positive),"complete_available_evidence_at_5":available.iter().filter(|r|r[method]["metrics"]["complete_evidence_at_5"]==true).count(),"mean_reciprocal_rank_at_5_original":if positive.is_empty(){Value::Null}else{json!(positive.iter().map(|r|r[method]["metrics"]["reciprocal_rank_at_5"].as_f64().unwrap()).sum::<f64>()/positive.len() as f64)},"negative_denominator":negative.len(),"negative_cases_with_candidates":negative.iter().filter(|r|r[method]["metrics"]["candidates_returned"].as_u64().unwrap()>0).count(),"abstention_tested":false})
}
fn evaluate(f: &Fixture, frozen: &Frozen) -> Result<Value> {
    let key = PROFILE.model_key();
    let cached = f.store.eligible_memory_embeddings(&f.lid, &key)?;
    let available: BTreeSet<_> = f.by_memory.values().cloned().collect();
    ensure!(cached.len() == 42, "Incomplete eligible cache");
    let mut evaluations = vec![];
    for condition in CONDITIONS {
        let mut cases = vec![];
        for query in &frozen.queries {
            let qid = s(query, "id");
            let input_key = format!("{condition}:{qid}");
            let text = s(&frozen.inputs[&input_key], "text");
            let vector = &frozen.vectors[&input_key];
            let sets = evidence_sets(&frozen.labels[qid]);
            let mut dense: Vec<_> = cached
                .iter()
                .map(|row| (row, cosine(vector, &row.vector)))
                .collect();
            dense.sort_by(|(a, sa), (b, sb)| {
                sb.total_cmp(sa)
                    .then_with(|| a.snapshot.memory_id.cmp(&b.snapshot.memory_id))
            });
            let dense:Vec<_>=dense.iter().map(|(row,score)|json!({"source_id":f.by_memory[&row.snapshot.memory_id],"memory_id":row.snapshot.memory_id,"score":score,"whole_chars":row.snapshot.content.chars().count()})).collect();
            let lexical = store_rank(f, text, None)?;
            let rrf = store_rank(
                f,
                text,
                Some(QueryEmbedding {
                    model_key: &key,
                    values: vector,
                }),
            )?;
            let mut row = json!({"query_id":qid,"mode":query["mode"],"pack_id":query["pack_id"],"query_text_sha256":frozen.inputs[&input_key]["text_sha256"],"original_sufficient_evidence_sets":frozen.labels[qid]["sufficient_evidence_sets"],"complete_set_available":sets.iter().any(|set|set.is_subset(&available)),"unavailable_alternatives":sets.iter().filter(|set|!set.is_subset(&available)).collect::<Vec<_>>()});
            for (method, ranking, score) in [
                (METHODS[0], lexical, "ranking_score"),
                (METHODS[1], dense, "score"),
                (METHODS[2], rrf, "ranking_score"),
            ] {
                let selection = whole_selection(&ranking);
                let ids = selected_ids(&selection);
                row[method] = json!({"selection":selection,"metrics":measure(&ids,&sets),"equal_score_groups_visible":ties(&ranking,score),"ranking":ranking,"ranking_scope":if method==METHODS[1]{"full_42_eligible"}else{"actual_Store_return_max_5"}});
            }
            cases.push(row);
        }
        let all: Vec<_> = cases.iter().collect();
        let cross: Vec<_> = cases
            .iter()
            .filter(|r| matches!(s(r, "mode"), "zh_to_en" | "en_to_zh"))
            .collect();
        let multi: Vec<_> = cases
            .iter()
            .filter(|r| r["mode"] == "mixed_multi")
            .collect();
        ensure!(
            cases.len() == 96 && cross.len() == 24 && multi.len() == 11,
            "Original query strata changed"
        );
        let mut totals = json!({});
        for method in METHODS {
            totals[method] = json!({"overall":summary(&all,method),"cross_language":summary(&cross,method),"mixed_multi":summary(&multi,method)});
            let topics: BTreeSet<_> = cases.iter().map(|r| s(r, "pack_id")).collect();
            for topic in topics {
                let rows = cases
                    .iter()
                    .filter(|r| s(r, "pack_id") == topic)
                    .collect::<Vec<_>>();
                totals[method]["topics"][topic] = summary(&rows, method);
            }
        }
        let rrf_regressions: Vec<_> = cases
            .iter()
            .filter(|r| {
                r[METHODS[1]]["metrics"]["complete_evidence_at_5"] == true
                    && r[METHODS[2]]["metrics"]["complete_evidence_at_5"] == false
            })
            .map(|r| r["query_id"].clone())
            .collect();
        evaluations.push(json!({"condition":condition,"summary":totals,"rrf_complete_evidence_regressions_vs_pure":rrf_regressions,"cases":cases,"quality_gate":"not_inherited; subset diagnosis only"}));
    }
    Ok(json!(evaluations))
}
fn controls(f: &Fixture, frozen: &Frozen) -> Result<Value> {
    let key = PROFILE.model_key();
    let target = frozen
        .decisions
        .iter()
        .find(|d| d["source_id"] == "evaporation_s01")
        .unwrap();
    let source = s(target, "source_id");
    let vector = &frozen.vectors[&format!("source:{source}")];
    let original = f
        .store
        .eligible_memory_embeddings(&f.lid, &key)?
        .into_iter()
        .find(|row| f.by_memory[&row.snapshot.memory_id] == source)
        .unwrap()
        .snapshot;
    let other = s(
        &f.store
            .create_learner("SECOND_NONEMPTY_SYNTHETIC_OWNER", "zh", "guided")?,
        "learner_id",
    )
    .to_owned();
    let sid = s(&f.store.create_session(&other)?, "session_id").to_owned();
    let event = f.store.record_education_event(
        &other,
        &sid,
        "simulated",
        "observation",
        &target["live_imported_event"]["content"],
    )?;
    let duplicate = promote(
        &f.store,
        &other,
        &sid,
        &[(target, event.clone())],
        1_793_721_601_000,
    )?
    .remove(0);
    let other_mid = s(&duplicate["memory"], "memory_id");
    let other_work = f.store.embedding_work(&other, &key, 32)?.remove(0);
    ensure!(
        other_work.content == original.content
            && other_work.memory_id != original.memory_id
            && s(&event, "event_id") != s(&f.events[source], "event_id"),
        "Owners must have independent identical-text evidence"
    );
    ensure!(f.store.commit_memory_embedding(&other_work, &key, vector)?);
    ensure!(
        f.store.eligible_memory_embeddings(&f.lid, &key)?.len() == 42
            && f.store.eligible_memory_embeddings(&other, &key)?.len() == 1,
        "Nonempty owner caches were not isolated"
    );
    let mut forged = original.clone();
    forged.learner_id = other.clone();
    ensure!(!f.store.commit_memory_embedding(&forged, &key, vector)?);
    for lid in [&f.lid, &other] {
        let result = f.store.recall_candidates(
            lid,
            &original.content,
            Some(QueryEmbedding {
                model_key: &key,
                values: vector,
            }),
            5,
        )?;
        ensure!(
            !result.is_empty() && result.iter().all(|row| row.memory["learner_id"] == *lid),
            "Cross-owner candidates"
        );
        if lid == &other {
            ensure!(result.len() == 1 && result[0].memory["memory_id"] == other_mid);
        }
    }
    let mut result = vec![
        json!({"control":"same_namespace_nonempty_owners_identical_text","passed":true,"primary_cached":42,"other_cached":1,"forged_receipt_accepted":false}),
    ];
    let old = &f.events[source];
    f.store.revise_education_event(
        &f.lid,
        s(old, "session_id"),
        s(old, "event_id"),
        "simulated",
        &json!({"text":"合成更正：该条旧观察现已撤销。"}),
    )?;
    ensure!(
        f.store
            .memory_evidence(&f.lid, &original.memory_id)
            .is_err()
            && !f.store.commit_memory_embedding(&original, &key, vector)?
            && f.store.eligible_memory_embeddings(&f.lid, &key)?.is_empty(),
        "Revision failed to revoke old vector"
    );
    ensure!(
        f.store
            .recall_candidates(
                &f.lid,
                &original.content,
                Some(QueryEmbedding {
                    model_key: &key,
                    values: vector
                }),
                5
            )?
            .iter()
            .all(|row| s(&row.memory, "memory_id") != original.memory_id)
            && f.store.eligible_memory_embeddings(&other, &key)?.len() == 1,
        "Revision leaked or crossed owner"
    );
    result.push(json!({"control":"source_revision_api","passed":true,"primary_cache_cleared_by_epoch":true,"other_cache_preserved":1,"late_receipt_accepted":false}));
    let survivor = f.store.embedding_work(&f.lid, &key, 1)?.remove(0);
    let survivor_vector = &frozen.vectors[&format!("source:{}", f.by_memory[&survivor.memory_id])];
    ensure!(
        f.store
            .commit_memory_embedding(&survivor, &key, survivor_vector)?
    );
    f.store.forget(&other, Some(other_mid))?;
    ensure!(
        f.store.memory_evidence(&other, other_mid).is_err()
            && !f.store.commit_memory_embedding(&other_work, &key, vector)?
            && f.store.eligible_memory_embeddings(&other, &key)?.is_empty()
            && f.store
                .recall_candidates(
                    &other,
                    &original.content,
                    Some(QueryEmbedding {
                        model_key: &key,
                        values: vector
                    }),
                    5
                )?
                .is_empty()
            && f.store.eligible_memory_embeddings(&f.lid, &key)?.len() == 1,
        "Forget failed owner/source isolation"
    );
    result.push(json!({"control":"forget_api","passed":true,"forgotten_owner_empty":true,"primary_survivor_cache_preserved":1,"late_receipt_accepted":false}));
    ensure!(
        f.store
            .commit_memory_embedding(&survivor, &key, &survivor_vector[..384])
            .is_err()
    );
    // Valid for the other profile, so rejection cannot be attributed to its
    // dimensionality or norm. This control vector never enters quality scores.
    let other_profile = EmbeddingProfile::E5NativeFp32;
    let mut other_profile_vector = vec![0.0; other_profile.dimension()];
    other_profile_vector[0] = 1.0;
    ensure!(other_profile.normalized(&other_profile_vector));
    ensure!(!f.store.commit_memory_embedding(
        &survivor,
        &other_profile.model_key(),
        &other_profile_vector
    )?);
    ensure!(
        f.store
            .embedding_work(&f.lid, "unknown-profile", 1)
            .is_err()
    );
    let wrong_key = other_profile.model_key();
    ensure!(
        f.store
            .eligible_memory_embeddings(&f.lid, &wrong_key)?
            .is_empty()
            && f.store.eligible_memory_embeddings(&f.lid, &key)?.len() == 1,
        "Wrong-profile control must have a real BGE cache but no E5 cache"
    );
    let fallback = f.store.recall_candidates(
        &f.lid,
        &survivor.content,
        Some(QueryEmbedding {
            model_key: &wrong_key,
            values: &other_profile_vector,
        }),
        5,
    )?;
    ensure!(
        !fallback.is_empty() && fallback.iter().all(|row| row.semantic_rank.is_none()),
        "Wrong profile query was treated as semantic"
    );
    result.push(json!({"control":"wrong_dimension_profile_and_unknown_profile","passed":true,
        "foreign_profile_vector":"synthetic normalized 384-dimensional E5 unit vector, control only",
        "foreign_vector_valid_for_claimed_profile":true,"wrong_profile_receipt_accepted":false,
        "bge_cached_before_query":1,"e5_cached_before_query":0,"foreign_query_semantic_ranks":0}));
    Ok(json!(result))
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 4 && args[0] == "--root" && args[2] == "--output",
        "usage: typed-store-probe --root ROOT --output NEW_REPORT.json"
    );
    let root = PathBuf::from(&args[1]).canonicalize()?;
    let output = PathBuf::from(&args[3]);
    ensure!(!output.exists(), "Preserve previous report");
    let start = Instant::now();
    let frozen = load(&root)?;
    let f = Fixture::rebuild(&frozen)?;
    let indexing = f.index(&frozen)?;
    let evaluations = evaluate(&f, &frozen)?;
    let checks = controls(&f, &frozen)?;
    let report = json!({"status":"completed","method":serde_json::from_str::<Value>(METHOD)?,"method_sha256":hash(METHOD.as_bytes()),"probe_sha256":hash(include_bytes!("typed_store_probe.rs")),"input_hashes":frozen.hashes,"profile":PROFILE.metadata(),"reconstructed_bindings":f.bindings,"excluded_sources":frozen.decisions.iter().filter(|d|d["projected"]==false).collect::<Vec<_>>(),"indexing":indexing,"evaluations":evaluations,"controls":checks,"elapsed_seconds":start.elapsed().as_secs_f64(),"timing_scope":"JSON/hash validation + temporary SQLite + ranking; no model execution","models_executed":false,"devices_used":false,"production_changed":false,"database":"new owned temporary simulator only","quality_gate":"none inherited from raw60/raw18/encoding success"});
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    serde_json::to_writer_pretty(&mut file, &report)?;
    file.write_all(b"\n")?;
    println!(
        "{{\"status\":\"completed\",\"typed_vectors\":42,\"query_conditions\":192,\"models_executed\":false}}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).into()).collect()
    }
    #[test]
    fn unavailable_evidence_is_never_trimmed_out_of_a_required_set() {
        let sets = vec![set(&["a", "missing"])];
        let available = set(&["a"]);
        assert!(!sets.iter().any(|s| s.is_subset(&available)));
        assert_eq!(
            measure(&["a".into()], &sets)["complete_evidence_at_5"],
            false
        );
    }
    #[test]
    fn one_complete_minimal_alternative_is_enough() {
        let sets = vec![set(&["a", "b"]), set(&["c"])];
        assert_eq!(
            measure(&["c".into()], &sets)["complete_evidence_at_5"],
            true
        );
        assert_eq!(
            measure(&["a".into()], &sets)["complete_evidence_at_5"],
            false
        );
    }
    #[test]
    fn candidate_return_on_negative_is_not_abstention() {
        let result = measure(&["unrelated".into()], &[]);
        assert_eq!(result["candidates_returned"], 1);
        assert_eq!(result["abstention_tested"], false);
        assert!(result.get("complete_evidence_at_5").is_none());
    }
    #[test]
    fn whole_memory_budget_skips_without_truncation() {
        let rows: Vec<_> = [800, 800, 700, 800, 50]
            .into_iter()
            .enumerate()
            .map(|(i, n)| json!({"source_id":format!("s{i}"),"whole_chars":n}))
            .collect();
        let chosen = whole_selection(&rows);
        assert_eq!(chosen["source_ids"], json!(["s0", "s1", "s2", "s4"]));
        assert_eq!(chosen["whole_chars"], 2350);
        assert_eq!(chosen["skipped_for_budget"], json!(["s3"]));
    }
}
