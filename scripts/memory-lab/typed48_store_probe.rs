//! New typed48 real cache/recall diagnostic; frozen vectors, no model execution.
mod typed48_common;
use ai_tutor::{
    memory_embedding::{EmbeddingProfile, cosine},
    store::{QueryEmbedding, s},
};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};
use typed48_common::{Frozen, Projected, hash};
const METHOD: &str = include_str!("typed48_store_method_v1.json");
const ENCODE_METHOD: &str = include_str!("typed48_encode_method_v1.json");
const PROFILE: EmbeddingProfile = EmbeddingProfile::BgeM3NativeFp32;

fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    ensure!(
        expected.len() == 64
            && expected
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "Explicit lowercase SHA256 required"
    );
    ensure!(
        fs::metadata(path)?.len() < 24 * 1024 * 1024,
        "Input exceeds diagnostic bound"
    );
    let bytes = fs::read(path)?;
    ensure!(
        hash(&bytes) == expected,
        "Frozen artifact changed: {}",
        path.display()
    );
    Ok(bytes)
}
struct Vectors {
    projection: Value,
    encoding: Value,
    inputs: HashMap<String, Value>,
    values: HashMap<String, Vec<f32>>,
}
fn load_vectors(
    projection_dir: &Path,
    projection_sha: &str,
    encoding_dir: &Path,
    encoding_sha: &str,
) -> Result<Vectors> {
    let projection: Value = serde_json::from_slice(&checked(
        &projection_dir.join("report.json"),
        projection_sha,
    )?)?;
    let encoding: Value =
        serde_json::from_slice(&checked(&encoding_dir.join("report.json"), encoding_sha)?)?;
    let method: Value = serde_json::from_str(METHOD)?;
    ensure!(
        projection["status"] == "projection_completed_no_encoding"
            && projection["method_sha256"] == method["projection_method_sha256"]
            && projection["method"] == serde_json::from_str::<Value>(typed48_common::METHOD)?
            && projection["probe_sha256"] == hash(include_bytes!("typed48_projection_probe.rs"))
            && projection["common_sha256"] == hash(include_bytes!("typed48_common.rs"))
            && projection["fixture_manifest_sha256"] == typed48_common::FIXTURE_SHA,
        "Projection provenance mismatch"
    );
    ensure!(
        encoding["status"] == "encoding_completed_no_retrieval"
            && encoding["model_executed"] == true
            && encoding["ranking_executed"] == false
            && encoding["store_vector_imported"] == false
            && encoding["projection_report_sha256"] == projection_sha
            && encoding["projection_inputs_sha256"] == projection["encoder_inputs"]["sha256"]
            && encoding["method_sha256"] == method["encoding_method_sha256"]
            && encoding["method"] == serde_json::from_str::<Value>(ENCODE_METHOD)?
            && encoding["script_sha256"] == method["encoding_script_sha256"]
            && encoding["profile"] == PROFILE.metadata()
            && encoding["encoded_count"] == 240
            && encoding["input_count"] == 240,
        "Encoding provenance mismatch"
    );
    ensure!(
        projection["encoder_inputs"]["file"] == "encoder_inputs.jsonl"
            && projection["encoder_inputs"]["records"] == 240,
        "Wrong input artifact"
    );
    let bytes = checked(
        &projection_dir.join("encoder_inputs.jsonl"),
        s(&projection["encoder_inputs"], "sha256"),
    )?;
    let mut inputs = HashMap::new();
    for line in std::str::from_utf8(&bytes)?.lines() {
        let row: Value = serde_json::from_str(line)?;
        ensure!(
            row["text_sha256"] == hash(s(&row, "text").as_bytes()),
            "Text hash mismatch"
        );
        ensure!(
            inputs.insert(s(&row, "key").to_owned(), row).is_none(),
            "Duplicate input key"
        );
    }
    let rows: Vec<Value> = serde_json::from_slice(&checked(
        &encoding_dir.join("bge_m3-encodings.json"),
        s(&encoding, "encodings_sha256"),
    )?)?;
    let mut values = HashMap::new();
    for row in rows {
        let key = s(&row, "key");
        let input = inputs
            .get(key)
            .ok_or_else(|| anyhow!("Unknown encoded key"))?;
        ensure!(
            input["text_sha256"] == row["text_sha256"],
            "Vector text binding changed"
        );
        let vector: Vec<f32> = serde_json::from_value(row["encoding"]["pooled_vector"].clone())?;
        ensure!(PROFILE.normalized(&vector), "Invalid normalized BGE vector");
        let bytes: Vec<_> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
        ensure!(
            row["encoding"]["pooled_sha256_f32le"] == hash(&bytes),
            "Vector bytes changed"
        );
        ensure!(
            values.insert(key.to_owned(), vector).is_none(),
            "Duplicate vector key"
        );
    }
    ensure!(
        inputs.len() == 240 && values.len() == 240,
        "All240 inputs/vectors required"
    );
    Ok(Vectors {
        projection,
        encoding,
        inputs,
        values,
    })
}
fn compare_projection(
    p: &Projected,
    frozen: &Frozen,
    encoded: &Vectors,
) -> Result<HashMap<String, String>> {
    ensure!(
        encoded.projection["original_fixture_files"] == frozen.files,
        "Original fixture binding changed"
    );
    let rows = encoded.projection["projection"]["decisions"]
        .as_array()
        .ok_or_else(|| anyhow!("Missing decisions"))?;
    ensure!(rows.len() == p.decisions.len(), "Decision count changed");
    let mut by_memory = HashMap::new();
    for (new, old) in p.decisions.iter().zip(rows) {
        ensure!(
            new["original_source"] == old["original_source"]
                && new["source_id"] == old["source_id"]
                && new["projection_kind"] == old["projection_kind"]
                && new["projected"] == old["projected"]
                && new["exclusion_reason"] == old["exclusion_reason"],
            "Projection source decision changed"
        );
        if new["projected"] != true {
            continue;
        }
        let (a, b) = (&new["memory_evidence"], &old["memory_evidence"]);
        for field in [
            "content",
            "kind",
            "scope",
            "activity_scope",
            "status",
            "source_type",
            "epoch",
        ] {
            ensure!(
                a["memory"][field] == b["memory"][field],
                "Actual memory differs from encoded projection"
            );
        }
        ensure!(
            a["education_metadata"] == b["education_metadata"],
            "Typed metadata changed"
        );
        for field in [
            "content",
            "event_kind",
            "event_version",
            "event_status",
            "source_type",
        ] {
            ensure!(
                a["sources"][0][field] == b["sources"][0][field],
                "Actual source differs from encoded projection"
            );
        }
        by_memory.insert(
            s(new, "memory_id").to_owned(),
            s(new, "source_id").to_owned(),
        );
    }
    for input in &p.inputs {
        ensure!(
            encoded.inputs.get(s(input, "key")) == Some(input),
            "Actual encoding input changed"
        );
    }
    ensure!(by_memory.len() == 48, "Expected48 reconstructed memories");
    p.verify_live()?;
    Ok(by_memory)
}
fn index(p: &Projected, v: &Vectors, by_memory: &HashMap<String, String>) -> Result<Value> {
    let key = PROFILE.model_key();
    let mut seen = HashSet::new();
    let mut batches = vec![];
    for _ in 0..3 {
        let work = p.store.embedding_work(&p.learner_id, &key, 32)?;
        if work.is_empty() {
            break;
        }
        batches.push(work.len());
        for snapshot in work {
            ensure!(
                seen.insert(snapshot.memory_id.clone()),
                "Index repeated a memory"
            );
            let input_key = format!("source:{}", by_memory[&snapshot.memory_id]);
            ensure!(
                snapshot.content == s(&v.inputs[&input_key], "text")
                    && snapshot.dimension == 1024
                    && snapshot.model_key == key,
                "Work snapshot differs"
            );
            ensure!(
                p.store
                    .commit_memory_embedding(&snapshot, &key, &v.values[&input_key])?,
                "Vector CAS rejected"
            );
        }
    }
    let cached = p.store.eligible_memory_embeddings(&p.learner_id, &key)?;
    ensure!(
        batches == [32, 16]
            && seen.len() == 48
            && cached.len() == 48
            && p.store.embedding_work(&p.learner_id, &key, 32)?.is_empty(),
        "Incomplete48-vector index"
    );
    for row in cached {
        let input_key = format!("source:{}", by_memory[&row.snapshot.memory_id]);
        ensure!(
            row.vector == v.values[&input_key]
                && row.snapshot.content == s(&v.inputs[&input_key], "text"),
            "Cache round trip differs"
        );
    }
    Ok(
        json!({"batch_sizes":batches,"unique_committed":48,"eligible":48,"remaining_work":0,"exact_vector_and_text_roundtrip":48}),
    )
}
fn select(rows: &[Value]) -> Value {
    let mut ids = vec![];
    let mut skipped = vec![];
    let mut chars = 0u64;
    for r in rows {
        if ids.len() == 5 {
            break;
        }
        let size = r["whole_chars"].as_u64().unwrap();
        if chars + size > 2400 {
            skipped.push(r["source_id"].clone());
            continue;
        }
        chars += size;
        ids.push(r["source_id"].clone());
    }
    json!({"source_ids":ids,"whole_chars":chars,"skipped_for_budget":skipped})
}
fn measure(selection: &Value, alternatives: &Value) -> Value {
    let ids: Vec<_> = selection["source_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let sets: Vec<BTreeSet<&str>> = alternatives
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            v.as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect()
        })
        .collect();
    if sets.is_empty() {
        return json!({"answerable":false,"candidates_returned":ids.len(),"abstention_tested":false});
    }
    let picked: BTreeSet<_> = ids.iter().copied().collect();
    let first = ids
        .iter()
        .position(|id| sets.iter().any(|set| set.contains(id)));
    json!({"answerable":true,"complete_evidence_at_5":sets.iter().any(|set|set.is_subset(&picked)),
        "reciprocal_rank_at_5":first.map_or(0.0,|i|1.0/(i+1) as f64),
        "best_set_coverage_at_5":sets.iter().map(|set|set.intersection(&picked).count() as f64/set.len() as f64).fold(0.0,f64::max)})
}
fn ranking(rows: Vec<Value>, alternatives: &Value, scope: &str) -> Value {
    let selected = select(&rows);
    let mut ties = vec![];
    let mut start = 0;
    while start < rows.len() {
        let mut end = start + 1;
        while end < rows.len() && rows[start]["score"] == rows[end]["score"] {
            end += 1;
        }
        if end - start > 1 {
            ties.push(json!({"first_rank":start+1,"score":rows[start]["score"],"source_ids":rows[start..end].iter().map(|v|&v["source_id"]).collect::<Vec<_>>()}));
        }
        start = end;
    }
    json!({"ranking":rows,"ranking_scope":scope,"equal_score_groups_visible":ties,"selection":selected,"metrics":measure(&selected,alternatives)})
}
fn store_rows(
    p: &Projected,
    by_memory: &HashMap<String, String>,
    text: &str,
    query: Option<QueryEmbedding<'_>>,
) -> Result<Vec<Value>> {
    let mut rows = vec![];
    let mut chars = 0;
    for c in p.store.recall_candidates(&p.learner_id, text, query, 5)? {
        let mid = s(&c.memory, "memory_id");
        let live = p.store.memory_evidence(&p.learner_id, mid)?;
        ensure!(
            c.memory == live["memory"] && c.memory["learner_id"] == p.learner_id,
            "Candidate is not current owner evidence"
        );
        let size = s(&c.memory, "content").chars().count();
        chars += size;
        rows.push(json!({"source_id":by_memory[mid],"memory_id":mid,"whole_chars":size,"score":c.ranking_score,
            "cosine":c.cosine,"semantic_rank":c.semantic_rank,"lexical_rank":c.lexical_rank,"relevance_status":c.relevance_status}));
    }
    ensure!(rows.len() <= 5 && chars <= 2400, "Store exceeded budget");
    Ok(rows)
}
fn summary(cases: &[&Value], method: &str) -> Value {
    let positive: Vec<_> = cases
        .iter()
        .filter(|c| c[method]["metrics"]["answerable"] == true)
        .collect();
    let negative: Vec<_> = cases
        .iter()
        .filter(|c| c[method]["metrics"]["answerable"] == false)
        .collect();
    json!({"original_positive_denominator":positive.len(),"available_positive_denominator":positive.iter().filter(|c|c["complete_set_available"]==true).count(),
        "complete_evidence_at_5":positive.iter().filter(|c|c[method]["metrics"]["complete_evidence_at_5"]==true).count(),
        "complete_available_evidence_at_5":positive.iter().filter(|c|c["complete_set_available"]==true && c[method]["metrics"]["complete_evidence_at_5"]==true).count(),
        "mrr_at_5_original_positive":if positive.is_empty(){Value::Null}else{json!(positive.iter().map(|c|c[method]["metrics"]["reciprocal_rank_at_5"].as_f64().unwrap()).sum::<f64>()/positive.len() as f64)},
        "negative_denominator":negative.len(),"negative_with_candidates":negative.iter().filter(|c|c[method]["metrics"]["candidates_returned"].as_u64().unwrap()>0).count(),"abstention_tested":false})
}
fn evaluate(
    p: &Projected,
    frozen: &Frozen,
    v: &Vectors,
    by_memory: &HashMap<String, String>,
) -> Result<Value> {
    let key = PROFILE.model_key();
    let cached = p.store.eligible_memory_embeddings(&p.learner_id, &key)?;
    ensure!(cached.len() == 48, "Incomplete eligible cache");
    let available: HashSet<_> = by_memory.values().cloned().collect();
    let mut results = vec![];
    for condition in ["current_query", "query_plus_goal"] {
        let mut cases = vec![];
        for query in &frozen.queries {
            let qid = s(query, "id");
            let input_key = format!("{condition}:{qid}");
            let text = s(&v.inputs[&input_key], "text");
            let vector = &v.values[&input_key];
            let label = frozen.labels.iter().find(|v| v["query_id"] == qid).unwrap();
            let sets = &label["sufficient_evidence_sets"];
            let mut dense: Vec<_> = cached
                .iter()
                .map(|c| (cosine(vector, &c.vector), &c.snapshot))
                .collect();
            dense.sort_by(|a, b| {
                b.0.total_cmp(&a.0)
                    .then_with(|| a.1.memory_id.cmp(&b.1.memory_id))
            });
            let dense:Vec<_>=dense.into_iter().map(|(score,snapshot)|json!({"source_id":by_memory[&snapshot.memory_id],"memory_id":snapshot.memory_id,"score":score,"whole_chars":snapshot.content.chars().count()})).collect();
            cases.push(json!({"query_id":qid,"mode":query["mode"],"pack_id":query["pack_id"],"query_text_sha256":v.inputs[&input_key]["text_sha256"],
                "original_sufficient_evidence_sets":sets,"complete_set_available":typed48_common::complete_set_available(sets,&available),
                "legacy_lexical":ranking(store_rows(p,by_memory,text,None)?,sets,"actual_Store_return_max5"),
                "pure_bge":ranking(dense,sets,"all48_actual_eligible"),
                "actual_rrf":ranking(store_rows(p,by_memory,text,Some(QueryEmbedding{model_key:&key,values:vector}))?,sets,"actual_Store_return_max5")}));
        }
        let all: Vec<_> = cases.iter().collect();
        let cross: Vec<_> = cases
            .iter()
            .filter(|v| ["zh_to_en", "en_to_zh"].contains(&s(v, "mode")))
            .collect();
        let multi: Vec<_> = cases
            .iter()
            .filter(|v| v["mode"] == "mixed_multi")
            .collect();
        let mut summaries = json!({});
        for method in ["legacy_lexical", "pure_bge", "actual_rrf"] {
            let mut topics = json!({});
            for topic in [
                "germination",
                "paper_bridge",
                "fractions",
                "reading",
                "loops",
                "evaporation",
            ] {
                topics[topic] = summary(
                    &cases
                        .iter()
                        .filter(|v| v["pack_id"] == topic)
                        .collect::<Vec<_>>(),
                    method,
                );
            }
            summaries[method] = json!({"overall":summary(&all,method),"cross_language":summary(&cross,method),"mixed_multi":summary(&multi,method),"topics":topics});
        }
        results.push(json!({"condition":condition,"summary":summaries,"rrf_complete_regressions_vs_pure":cases.iter().filter(|c|c["pure_bge"]["metrics"]["complete_evidence_at_5"]==true && c["actual_rrf"]["metrics"]["complete_evidence_at_5"]!=true).map(|c|&c["query_id"]).collect::<Vec<_>>(),"cases":cases}));
    }
    Ok(json!(results))
}
fn source_control(p: &Projected, v: &Vectors) -> Result<Value> {
    let key = PROFILE.model_key();
    let d = p
        .decisions
        .iter()
        .find(|d| d["source_id"] == "evaporation_s03")
        .unwrap();
    ensure!(
        d["projection_kind"] == "context_constraint",
        "Control source changed"
    );
    let snapshot = p
        .store
        .eligible_memory_embeddings(&p.learner_id, &key)?
        .into_iter()
        .find(|c| c.snapshot.memory_id == s(d, "memory_id"))
        .unwrap()
        .snapshot;
    let event = &d["live_imported_event"];
    p.store.revise_education_event(&p.learner_id,s(event,"session_id"),s(event,"event_id"),"simulated",&json!({"text":"Synthetic correction: the earlier evaporation conditions are withdrawn; no new trial or result is asserted."}))?;
    let stale =
        p.store
            .commit_memory_embedding(&snapshot, &key, &v.values["source:evaporation_s03"])?;
    ensure!(
        !stale
            && p.store
                .memory_evidence(&p.learner_id, &snapshot.memory_id)
                .is_err()
            && p.store
                .eligible_memory_embeddings(&p.learner_id, &key)?
                .is_empty(),
        "Revised condition remained eligible"
    );
    let rows = p.store.recall_candidates(
        &p.learner_id,
        "evaporation conditions",
        Some(QueryEmbedding {
            model_key: &key,
            values: &v.values["current_query:evaporation_q02"],
        }),
        5,
    )?;
    ensure!(
        rows.iter()
            .all(|c| s(&c.memory, "memory_id") != snapshot.memory_id),
        "Revised condition recalled"
    );
    Ok(
        json!({"control":"new_context_constraint_revision_real_Store","source_id":"evaporation_s03","passed":true,
        "old_evidence_invalid":true,"late_vector_commit_accepted":false,"owner_cache_cleared_by_epoch":true,
        "other_owner_or_scope_controls_executed":false,"forget_control_executed":false}),
    )
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let flags = [
        "--root",
        "--projection",
        "--projection-report-sha256",
        "--encoding",
        "--encoding-report-sha256",
        "--output",
    ];
    ensure!(
        args.len() == 12 && flags.iter().enumerate().all(|(i, f)| args[2 * i] == *f),
        "usage: typed48-store-probe --root ROOT --projection DIR --projection-report-sha256 SHA --encoding DIR --encoding-report-sha256 SHA --output NEW_REPORT"
    );
    let root = PathBuf::from(&args[1]).canonicalize()?;
    let output = PathBuf::from(&args[11]);
    ensure!(!output.exists(), "Use a new report");
    let projection_sha = args[5]
        .to_str()
        .ok_or_else(|| anyhow!("Invalid projection SHA"))?;
    let encoding_sha = args[9]
        .to_str()
        .ok_or_else(|| anyhow!("Invalid encoding SHA"))?;
    let started = Instant::now();
    let frozen = typed48_common::load(&root)?;
    let v = load_vectors(
        Path::new(&args[3]),
        projection_sha,
        Path::new(&args[7]),
        encoding_sha,
    )?;
    let p = typed48_common::project(&frozen)?;
    let by_memory = compare_projection(&p, &frozen, &v)?;
    let indexing = index(&p, &v, &by_memory)?;
    let evaluations = evaluate(&p, &frozen, &v, &by_memory)?;
    let control = source_control(&p, &v)?;
    let report = json!({"status":"completed","method":serde_json::from_str::<Value>(METHOD)?,"method_sha256":hash(METHOD.as_bytes()),
        "probe_sha256":hash(include_bytes!("typed48_store_probe.rs")),"common_sha256":hash(include_bytes!("typed48_common.rs")),
        "projection_report_sha256":projection_sha,"encoding_report_sha256":encoding_sha,"vectors_sha256":v.encoding["encodings_sha256"],
        "encoder_inputs_sha256":v.projection["encoder_inputs"]["sha256"],"fixture_manifest_sha256":typed48_common::FIXTURE_SHA,
        "profile":PROFILE.metadata(),"projection_availability":p.summary["availability"],"reconstructed_decisions":p.decisions,
        "snapshot_scope":"reconstructed decisions, index counts and rankings were captured before the subsequent source-revision control",
        "indexing":indexing,"evaluations":evaluations,"controls":[control],"quality_gate":"none inherited; available-subset diagnosis only",
        "models_executed":false,"production_changed":false,"devices_used":false,"database":"owned disposable synthetic Store",
        "elapsed_seconds":started.elapsed().as_secs_f64(),"timing_scope":"input verification, Store reconstruction/CAS/recall, ranking and source control; no model encoding"});
    fs::create_dir_all(output.parent().unwrap_or(Path::new(".")))?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)?
        .write_all(&serde_json::to_vec_pretty(&report)?)?;
    println!(
        "{{\"status\":\"completed\",\"indexed\":48,\"query_rows\":192,\"models_executed\":false}}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn whole_budget_never_truncates_or_counts_partial_evidence_as_complete() {
        let rows = vec![
            json!({"source_id":"large","whole_chars":2401}),
            json!({"source_id":"a","whole_chars":10}),
        ];
        let selected = select(&rows);
        assert_eq!(selected["source_ids"], json!(["a"]));
        assert_eq!(
            measure(&selected, &json!([["a", "b"]]))["complete_evidence_at_5"],
            false
        );
        assert_eq!(
            measure(&selected, &json!([["a", "b"], ["a"]]))["complete_evidence_at_5"],
            true
        );
        assert_eq!(measure(&selected, &json!([]))["abstention_tested"], false);
    }
}
