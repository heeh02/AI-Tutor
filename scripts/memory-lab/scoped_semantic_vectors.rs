//! Frozen-vector import only; this module never encodes or calls a provider.
use super::*;
const METHOD: &str = include_str!("typed48_store_method_v1.json");
const ENCODE_METHOD: &str = include_str!("typed48_encode_method_v1.json");
pub(super) struct Vectors {
    pub projection: Value,
    pub encoding: Value,
    pub inputs: HashMap<String, Value>,
    pub values: HashMap<String, Vec<f32>>,
}
pub(super) fn load_vectors(
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
            && projection["method"] == serde_json::from_str::<Value>(typed48::METHOD)?
            && projection["probe_sha256"] == hash(include_bytes!("typed48_projection_probe.rs"))
            && projection["common_sha256"] == hash(include_bytes!("typed48_common.rs"))
            && projection["fixture_manifest_sha256"] == typed48::FIXTURE_SHA,
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
pub(super) fn compare_projection(
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
pub(super) fn index(
    p: &Projected,
    v: &Vectors,
    by_memory: &HashMap<String, String>,
) -> Result<Value> {
    let key = PROFILE.model_key();
    let mut seen = HashSet::new();
    let mut batches = vec![];
    let mut commits = vec![];
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
            commits.push(json!({"memory_id":snapshot.memory_id,"source_alias":by_memory[&snapshot.memory_id],
                "namespace":snapshot.namespace,"learner_id":snapshot.learner_id,"learner_epoch":snapshot.learner_epoch,
                "model_key":snapshot.model_key,"dimension":snapshot.dimension,"memory_digest":snapshot.memory_digest,
                "source_digest":snapshot.source_digest,"content_sha256":hash(snapshot.content.as_bytes()),
                "vector_sha256_f32le":hash(&v.values[&input_key].iter().flat_map(|v|v.to_le_bytes()).collect::<Vec<_>>()),"cas_committed":true}));
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
        json!({"batch_sizes":batches,"unique_committed":48,"eligible":48,"remaining_work":0,"exact_vector_and_text_roundtrip":48,"commits":commits}),
    )
}
