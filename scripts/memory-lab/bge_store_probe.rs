//! Offline Store diagnosis using existing frozen vectors; never loads an encoder.
use ai_tutor::{
    memory_embedding::{EmbeddingProfile, cosine},
    store::{MemoryEmbeddingSnapshot, QueryEmbedding, Store, s, uid},
};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const FIXTURE_SHA: &str = "858ee969827d54d1687d1a3966e726576a66024f9b462683323732dde94fbb81";
const REPORT_SHA: &str = "b614aa9fcad3d12fb0ff6f4f3319eb0b73fbceac903a5aa967f7b76442a09cae";
const E5_SHA: &str = "fa6d14450772b7ab4bc402cfcc90f9136db6f76b80ee69d63d9d7896228a3634";
const BGE_SHA: &str = "a8fd7529581098579f28941d57099ed59b1de5f4bec4ecda2a9b89bf0839ed24";
const METHOD: &str = include_str!("bge_store_method_v1.json");
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    ensure!(
        fs::metadata(path)?.len() <= 20 * 1024 * 1024,
        "Diagnostic input exceeds bound"
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
fn text_for(query: &Value, condition: &str) -> String {
    if condition == "current_query" {
        s(query, "text").into()
    } else {
        format!("{} {} ", s(query, "text"), s(query, "goal"))
    }
}
struct Frozen {
    sources: Vec<Value>,
    queries: Vec<Value>,
    labels: HashMap<String, Value>,
    vectors: Vec<(EmbeddingProfile, HashMap<String, Vec<f32>>)>,
}
fn load(root: &Path) -> Result<Frozen> {
    let base = root.join("rust/ai-tutor/tests/fixtures/diagnostic_postfreeze_v2");
    let manifest: Value =
        serde_json::from_slice(&checked(&base.join("manifest.json"), FIXTURE_SHA)?)?;
    let sources = jsonl(&checked(
        &base.join("sources.jsonl"),
        s(&manifest["files"]["sources.jsonl"], "sha256"),
    )?)?;
    let queries = jsonl(&checked(
        &base.join("queries.jsonl"),
        s(&manifest["files"]["queries.jsonl"], "sha256"),
    )?)?;
    let labels = jsonl(&checked(
        &base.join("labels.jsonl"),
        s(&manifest["files"]["labels.jsonl"], "sha256"),
    )?)?
    .into_iter()
    .map(|row| (s(&row, "query_id").to_owned(), row))
    .collect::<HashMap<_, _>>();
    ensure!(
        sources.len() == 60 && queries.len() == 96 && labels.len() == 96,
        "Full frozen challenge required"
    );
    let report_dir = root.join("runtime/goal-baselines/20261104/bge-challenge-first");
    let report: Value =
        serde_json::from_slice(&checked(&report_dir.join("report.json"), REPORT_SHA)?)?;
    ensure!(
        report["status"] == "completed" && report["fixture_manifest_sha256"] == FIXTURE_SHA,
        "Unverified source vector run"
    );
    let mut vectors = vec![];
    for (name, profile, sha, weight) in [
        (
            "e5_fp32",
            EmbeddingProfile::E5NativeFp32,
            E5_SHA,
            "model.safetensors",
        ),
        (
            "bge_m3",
            EmbeddingProfile::BgeM3NativeFp32,
            BGE_SHA,
            "pytorch_model.bin",
        ),
    ] {
        ensure!(
            report["model_assets"][name][weight] == profile.weight_sha256()
                && report["model_assets"][name]["tokenizer.json"] == profile.tokenizer_sha256(),
            "Profile asset mismatch"
        );
        let rows: Vec<Value> = serde_json::from_slice(&checked(
            &report_dir.join(format!("{name}-encodings.json")),
            sha,
        )?)?;
        let bykey: HashMap<_, _> = rows.iter().map(|row| (s(row, "key"), row)).collect();
        ensure!(
            rows.len() == 252 && bykey.len() == 252,
            "Incomplete encoding file"
        );
        let mut expected: Vec<_> = sources
            .iter()
            .map(|row| {
                (
                    format!("source:{}", s(row, "id")),
                    format!("{}{}", profile.document_prefix(), s(row, "text")),
                )
            })
            .collect();
        for condition in ["current_query", "query_plus_goal"] {
            for query in &queries {
                expected.push((
                    format!("{condition}:{}", s(query, "id")),
                    format!("{}{}", profile.query_prefix(), text_for(query, condition)),
                ));
            }
        }
        let mut map = HashMap::new();
        for (key, text) in expected {
            let row = bykey
                .get(key.as_str())
                .ok_or_else(|| anyhow!("Encoding missing"))?;
            ensure!(
                row["text_sha256"] == hash(text.as_bytes()),
                "Vector input is not the unchanged source/query"
            );
            let vector: Vec<f32> = row["encoding"]["pooled_vector"]
                .as_array()
                .ok_or_else(|| anyhow!("Missing vector"))?
                .iter()
                .map(|v| {
                    v.as_f64()
                        .map(|n| n as f32)
                        .ok_or_else(|| anyhow!("Invalid vector"))
                })
                .collect::<Result<_>>()?;
            ensure!(profile.normalized(&vector), "Vector does not match profile");
            let bytes: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
            ensure!(
                row["encoding"]["pooled_sha256_f32le"] == hash(&bytes),
                "Serialized vector changed"
            );
            map.insert(key, vector);
        }
        vectors.push((profile, map));
    }
    Ok(Frozen {
        sources,
        queries,
        labels,
        vectors,
    })
}

struct OwnedDirectory(PathBuf);
impl Drop for OwnedDirectory {
    fn drop(&mut self) {
        for name in ["memory.sqlite3", "memory.sqlite3-wal", "memory.sqlite3-shm"] {
            let _ = fs::remove_file(self.0.join(name));
        }
        let _ = fs::remove_dir(&self.0);
    }
}
struct Fixture {
    store: Store,
    _directory: OwnedDirectory,
    lid: String,
    sid: String,
    by_memory: HashMap<String, String>,
    events: HashMap<String, Value>,
    admission: Vec<Value>,
}
fn admitted_kind(source: &Value) -> Option<&'static str> {
    match (s(source, "claim_type"), s(source, "event_kind")) {
        ("reported_observation", "observation") => Some("reported_progress"),
        ("accepted_plan", "plan") if source["accepted"] == true => Some("pending_experiment"),
        _ => None,
    }
}
impl Fixture {
    fn new(sources: &[Value], namespace: &str) -> Result<Self> {
        let directory = std::env::temp_dir().join(uid("ai-tutor-frozen-store"));
        fs::create_dir(&directory)?;
        let owned = OwnedDirectory(directory.clone());
        let store = Store::open(directory.join("memory.sqlite3"), namespace)?;
        let lid = s(
            &store.create_learner("SYNTHETIC_VECTOR_STORE", "zh", "guided")?,
            "learner_id",
        )
        .to_owned();
        let sid = s(&store.create_session(&lid)?, "session_id").to_owned();
        let mut by_memory = HashMap::new();
        let mut events = HashMap::new();
        let mut admission = vec![];
        for source in sources {
            let id = s(source, "id");
            let mut content = json!({"text":source["text"],"synthetic_fixture":true,"fixture_source_id":id,"claim_type":source["claim_type"],"support_level":source["support_level"]});
            if s(source, "event_kind") == "plan" {
                content["accepted"] = source["accepted"].clone();
            }
            let event = store.record_education_event(
                &lid,
                &sid,
                if namespace == "simulator" {
                    "simulated"
                } else {
                    "learner_report"
                },
                s(source, "event_kind"),
                &content,
            )?;
            events.insert(id.to_owned(), event.clone());
            if let Some(kind) = admitted_kind(source) {
                let candidate = json!({"kind":kind,"scope":"current_activity","content":source["text"],"source_event_ids":[event["event_id"]],"source_event_versions":{s(&event,"event_id"):event["event_version"]}});
                let mid = store
                    .add_memory(&lid, &candidate, 0)?
                    .ok_or_else(|| anyhow!("Allowed original source rejected"))?;
                ensure!(
                    store.memory_evidence(&lid, &mid)?["memory"]["content"] == source["text"],
                    "Promotion changed the frozen input"
                );
                by_memory.insert(mid.clone(), id.to_owned());
                admission.push(json!({"source_id":id,"event_kind":source["event_kind"],"claim_type":source["claim_type"],"indexed":true,"memory_kind":kind,"memory_id":mid,"text_unchanged":true}));
            } else {
                let reason = match s(source, "claim_type") {
                    "unaccepted_plan" => "unaccepted plan is not an accepted pending experiment",
                    "temporary_request" => "temporary request is not a persistent help preference",
                    _ => {
                        "typed promotion is a distinct path and changes the text prefix; frozen raw-source vector must not impersonate that input"
                    }
                };
                admission.push(json!({"source_id":id,"event_kind":source["event_kind"],"claim_type":source["claim_type"],"indexed":false,"reason":reason}));
            }
        }
        Ok(Self {
            store,
            _directory: owned,
            lid,
            sid,
            by_memory,
            events,
            admission,
        })
    }
    fn index(
        &self,
        profile: EmbeddingProfile,
        vectors: &HashMap<String, Vec<f32>>,
    ) -> Result<usize> {
        let key = profile.model_key();
        let mut count = 0;
        loop {
            let work = self.store.embedding_work(&self.lid, &key, 32)?;
            if work.is_empty() {
                break;
            }
            for snapshot in work {
                let source = &self.by_memory[&snapshot.memory_id];
                ensure!(
                    self.store.commit_memory_embedding(
                        &snapshot,
                        &key,
                        &vectors[&format!("source:{source}")]
                    )?,
                    "Frozen vector CAS rejected"
                );
                count += 1;
            }
        }
        ensure!(count == self.by_memory.len(), "Index count differs");
        Ok(count)
    }
    fn snapshot(&self, profile: EmbeddingProfile) -> Result<MemoryEmbeddingSnapshot> {
        Ok(self
            .store
            .embedding_work(&self.lid, &profile.model_key(), 1)?
            .remove(0))
    }
}

fn ids(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}
fn evidence_sets(label: &Value) -> Vec<BTreeSet<String>> {
    label["sufficient_evidence_sets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| ids(row).into_iter().collect())
        .collect()
}
fn measure(selected: &[String], sets: &[BTreeSet<String>]) -> Value {
    if sets.is_empty() {
        return json!({"original_answerable":false,"candidates_returned":!selected.is_empty(),"abstention_tested":false});
    }
    let top: BTreeSet<_> = selected.iter().cloned().collect();
    let first = selected
        .iter()
        .position(|id| sets.iter().any(|set| set.contains(id)))
        .map(|i| i + 1);
    json!({"original_answerable":true,"complete_evidence_at_5":sets.iter().any(|set|set.is_subset(&top)),
        "best_set_coverage_at_5":sets.iter().map(|set|set.intersection(&top).count() as f64/set.len() as f64).fold(0.0,f64::max),
        "reciprocal_rank_at_5":first.map_or(0.0,|rank|1.0/rank as f64)})
}
fn selected_dense(ranking: &[(String, f64)], memories: &HashMap<String, Value>) -> Vec<String> {
    let mut chosen = vec![];
    let mut remaining = 2400;
    for (mid, _) in ranking {
        if chosen.len() == 5 {
            break;
        }
        let size = s(&memories[mid], "content").chars().count();
        if size <= remaining {
            chosen.push(mid.clone());
            remaining -= size;
        }
    }
    chosen
}
fn store_rows(
    f: &Fixture,
    query: &str,
    embedding: Option<QueryEmbedding<'_>>,
) -> Result<(Vec<String>, Vec<Value>)> {
    let rows = f.store.recall_candidates(&f.lid, query, embedding, 5)?;
    let mut size = 0;
    let mut ids = vec![];
    let mut report = vec![];
    for row in rows {
        let mid = s(&row.memory, "memory_id");
        let evidence = f.store.memory_evidence(&f.lid, mid)?;
        ensure!(
            row.memory == evidence["memory"],
            "Candidate is not current whole memory"
        );
        size += s(&row.memory, "content").chars().count();
        let source = f
            .by_memory
            .get(mid)
            .ok_or_else(|| anyhow!("Unknown returned source"))?;
        ids.push(source.clone());
        report.push(json!({"source_id":source,"memory_id":mid,"lexical_rank":row.lexical_rank,"semantic_rank":row.semantic_rank,"cosine":row.cosine,"ranking_score":row.ranking_score,"relevance_status":row.relevance_status,"whole_chars":s(&row.memory,"content").chars().count()}));
    }
    ensure!(
        size <= 2400 && ids.len() <= 5,
        "Actual Store budget exceeded"
    );
    Ok((ids, report))
}
fn summarize(rows: &[Value], method: &str) -> Value {
    let positives: Vec<_> = rows
        .iter()
        .filter(|row| row[method]["metrics"]["original_answerable"] == true)
        .collect();
    let available: Vec<_> = positives
        .iter()
        .filter(|row| row["complete_set_available_in_store"] == true)
        .collect();
    let negatives: Vec<_> = rows
        .iter()
        .filter(|row| row[method]["metrics"]["original_answerable"] == false)
        .collect();
    json!({"original_positive_denominator":positives.len(),"store_available_positive_denominator":available.len(),
        "complete_original_evidence_at_5":positives.iter().filter(|row|row[method]["metrics"]["complete_evidence_at_5"]==true).count(),
        "complete_available_evidence_at_5":available.iter().filter(|row|row[method]["metrics"]["complete_evidence_at_5"]==true).count(),
        "negative_denominator":negatives.len(),"negatives_with_candidates":negatives.iter().filter(|row|row[method]["metrics"]["candidates_returned"]==true).count(),
        "quality_gate":"not_inherited_from_60_source_experiment","candidate_status":"candidate_requires_relevance_review"})
}
fn visible_ties(rows: &[Value], score: &str) -> Vec<Value> {
    let mut groups = vec![];
    let mut at = 0;
    while at < rows.len() {
        let mut end = at + 1;
        while end < rows.len() && rows[end][score] == rows[at][score] {
            end += 1;
        }
        if end - at > 1 {
            groups.push(json!({"score":rows[at][score],"first_rank":at+1,"source_ids":rows[at..end].iter().map(|row|&row["source_id"]).collect::<Vec<_>>()}));
        }
        at = end;
    }
    groups
}
fn evaluate(f: &Fixture, frozen: &Frozen) -> Result<Value> {
    let available: BTreeSet<_> = f.by_memory.values().cloned().collect();
    let mut output = vec![];
    for (profile, vectors) in &frozen.vectors {
        let key = profile.model_key();
        let cached = f.store.eligible_memory_embeddings(&f.lid, &key)?;
        ensure!(
            cached.len() == 18,
            "Only declared admitted subset is evaluated"
        );
        let memories: HashMap<_, _> = cached
            .iter()
            .map(|row| {
                Ok((
                    row.snapshot.memory_id.clone(),
                    f.store.memory_evidence(&f.lid, &row.snapshot.memory_id)?["memory"].clone(),
                ))
            })
            .collect::<Result<_>>()?;
        for condition in ["current_query", "query_plus_goal"] {
            let mut rows = vec![];
            for query in &frozen.queries {
                let qid = s(query, "id");
                let text = text_for(query, condition);
                let q = &vectors[&format!("{condition}:{qid}")];
                let sets = evidence_sets(&frozen.labels[qid]);
                let mut dense: Vec<_> = cached
                    .iter()
                    .map(|row| (row.snapshot.memory_id.clone(), cosine(q, &row.vector)))
                    .collect();
                dense.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                let dense_ids: Vec<_> = selected_dense(&dense, &memories)
                    .iter()
                    .map(|mid| f.by_memory[mid].clone())
                    .collect();
                let (lex_ids, lex) = store_rows(f, &text, None)?;
                let (rrf_ids, rrf) = store_rows(
                    f,
                    &text,
                    Some(QueryEmbedding {
                        model_key: &key,
                        values: q,
                    }),
                )?;
                let dense_rows:Vec<_>=dense.iter().map(|(mid,score)|json!({"memory_id":mid,"source_id":f.by_memory[mid],"score":score})).collect();
                rows.push(json!({"query_id":qid,"mode":query["mode"],"pack_id":query["pack_id"],"original_sufficient_evidence_sets":frozen.labels[qid]["sufficient_evidence_sets"],
                    "complete_set_available_in_store":sets.iter().any(|set|set.is_subset(&available)),
                    "unavailable_alternatives":sets.iter().filter(|set|!set.is_subset(&available)).collect::<Vec<_>>(),
                    "store_legacy_lexical":{"selected":lex_ids,"ranking":lex,"metrics":measure(&lex_ids,&sets)},
                    "pure_eligible_cosine":{"selected":dense_ids,"equal_score_groups":visible_ties(&dense_rows,"score"),"full_ranking":dense_rows,"metrics":measure(&dense_ids,&sets)},
                    "actual_store_rrf":{"selected":rrf_ids,"visible_equal_score_groups":visible_ties(&rrf,"ranking_score"),"ranking":rrf,"metrics":measure(&rrf_ids,&sets)}}));
            }
            output.push(json!({"profile":profile.metadata(),"condition":condition,"admitted_count":available.len(),
                "summary":{"store_legacy_lexical":summarize(&rows,"store_legacy_lexical"),"pure_eligible_cosine":summarize(&rows,"pure_eligible_cosine"),"actual_store_rrf":summarize(&rows,"actual_store_rrf")},"cases":rows}));
        }
    }
    Ok(json!(output))
}

fn control_checks(frozen: &Frozen) -> Result<Value> {
    let source = frozen
        .sources
        .iter()
        .find(|source| admitted_kind(source) == Some("reported_progress"))
        .unwrap();
    let source_id = s(source, "id");
    let (profile, vectors) = &frozen.vectors[1];
    let key = profile.model_key();
    let value = &vectors[&format!("source:{source_id}")];
    let mut outcomes = vec![];
    for change in [
        "source_revision_api",
        "source_version_sql",
        "source_delete_sql",
        "epoch_sql",
        "forget_api",
        "session_delete_sql",
        "learner_delete_sql",
    ] {
        let f = Fixture::new(std::slice::from_ref(source), "simulator")?;
        let snapshot = f.snapshot(*profile)?;
        ensure!(
            f.store.commit_memory_embedding(&snapshot, &key, value)?,
            "Control initial commit failed"
        );
        let eid = s(&f.events[source_id], "event_id");
        match change {
            "source_revision_api" => {
                f.store.revise_education_event(
                    &f.lid,
                    &f.sid,
                    eid,
                    "simulated",
                    &json!({"correction":"合成更正：先前报告不能作为当前事实。"}),
                )?;
            }
            "source_version_sql" => {
                f.store.db.execute(
                    "UPDATE education_event_meta SET version=version+1 WHERE event_id=?",
                    [eid],
                )?;
            }
            "source_delete_sql" => {
                f.store
                    .db
                    .execute("DELETE FROM events WHERE event_id=?", [eid])?;
            }
            "epoch_sql" => {
                f.store.db.execute(
                    "UPDATE learners SET epoch=epoch+1 WHERE learner_id=?",
                    [&f.lid],
                )?;
            }
            "forget_api" => {
                f.store.forget(&f.lid, Some(&snapshot.memory_id))?;
            }
            "session_delete_sql" => {
                f.store
                    .db
                    .execute("DELETE FROM sessions WHERE session_id=?", [&f.sid])?;
            }
            "learner_delete_sql" => {
                f.store
                    .db
                    .execute("DELETE FROM learners WHERE learner_id=?", [&f.lid])?;
            }
            _ => unreachable!(),
        }
        let remaining =
            f.store
                .db
                .query_row("SELECT COUNT(*) FROM memory_embeddings", [], |row| {
                    row.get::<_, i64>(0)
                })?;
        let late = f.store.commit_memory_embedding(&snapshot, &key, value)?;
        ensure!(
            remaining == 0 && !late,
            "Invalidation control failed: {change}"
        );
        outcomes.push(json!({"control":change,"cached_rows_after":remaining,"late_commit_accepted":late,"passed":true}));
    }
    let f = Fixture::new(std::slice::from_ref(source), "simulator")?;
    let snapshot = f.snapshot(*profile)?;
    ensure!(f.store.commit_memory_embedding(&snapshot, &key, value)?);
    let other = s(
        &f.store.create_learner("OTHER_SYNTHETIC", "en", "guided")?,
        "learner_id",
    )
    .to_owned();
    let mut forged = snapshot.clone();
    forged.learner_id = other.clone();
    ensure!(!f.store.commit_memory_embedding(&forged, &key, value)?);
    ensure!(f.store.eligible_memory_embeddings(&other, &key)?.is_empty());
    ensure!(
        f.store
            .recall_candidates(
                &other,
                s(source, "text"),
                Some(QueryEmbedding {
                    model_key: &key,
                    values: value
                }),
                5
            )?
            .is_empty()
    );
    outcomes.push(json!({"control":"cross_learner_read_and_forged_receipt","passed":true}));
    let real = Fixture::new(&[], "real")?;
    ensure!(!real.store.commit_memory_embedding(&snapshot, &key, value)?);
    outcomes.push(json!({"control":"cross_namespace_receipt","passed":true}));
    let e5 = &frozen.vectors[0].1[&format!("source:{source_id}")];
    ensure!(
        f.store
            .commit_memory_embedding(&snapshot, &key, e5)
            .is_err()
    );
    ensure!(!f.store.commit_memory_embedding(
        &snapshot,
        &EmbeddingProfile::E5NativeFp32.model_key(),
        e5
    )?);
    ensure!(
        f.store
            .embedding_work(&f.lid, "unknown-profile", 1)
            .is_err()
    );
    outcomes.push(json!({"control":"wrong_dimension_profile_and_unknown_profile","passed":true}));
    Ok(json!(outcomes))
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 4 && args[0] == "--root" && args[2] == "--output",
        "usage: bge-store-probe --root ROOT --output NEW_REPORT.json"
    );
    let root = PathBuf::from(&args[1]).canonicalize()?;
    let output = PathBuf::from(&args[3]);
    ensure!(!output.exists(), "Preserve previous report");
    let frozen = load(&root)?;
    let f = Fixture::new(&frozen.sources, "simulator")?;
    ensure!(f.by_memory.len() == 18, "Admission contract changed");
    let mut counts = vec![];
    for (profile, vectors) in &frozen.vectors {
        counts.push(json!({"profile":profile.metadata(),"committed":f.index(*profile,vectors)?}));
    }
    let evaluation = evaluate(&f, &frozen)?;
    let controls = control_checks(&frozen)?;
    let report = json!({"status":"completed","method":serde_json::from_str::<Value>(METHOD)?,"method_sha256":hash(METHOD.as_bytes()),"probe_source_sha256":hash(include_bytes!("bge_store_probe.rs")),
        "fixture_manifest_sha256":FIXTURE_SHA,"source_report_sha256":REPORT_SHA,"encoding_hashes":{"e5_fp32":E5_SHA,"bge_m3":BGE_SHA},
        "admission":f.admission,"indexing":counts,"evaluations":evaluation,"controls":controls,
        "database":"new temporary synthetic database only","default_recall_changed":false,"models_executed":false,"devices_used":false,
        "limitations":["18 admitted raw sources are not the original 60-source quality experiment","Unindexed types are not reclassified as observed/completed/persistent preferences","Actual Store uses legacy lexical plus RRF, not frozen lexical or pure BGE","Generated memory IDs are the actual Store tie-break; IDs are recorded and labels never break ties","Natural short inputs may not hit the hard budget boundary; separate fake-vector integration test covers whole-source overflow","SQL mutation controls exercise actual schema triggers and CAS but do not imply an API endpoint exists"]});
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)?;
    serde_json::to_writer_pretty(&mut file, &report)?;
    file.write_all(b"\n")?;
    println!("{{\"status\":\"completed\",\"admitted_sources\":18,\"models_executed\":false}}");
    Ok(())
}
