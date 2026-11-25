//! Strictly scoped, opt-in synthetic semantic context evaluation.
mod scoped_semantic_vectors;
#[allow(dead_code)]
#[path = "typed48_common.rs"]
mod typed48;
use ai_tutor::{
    context::{build_context, build_context_with_semantic, memory_recall_query},
    kimi::KimiProvider,
    memory_embedding::{EmbeddingProfile, cosine},
    store::{CandidateRanking, QueryEmbedding, Store, empty_state, s, strings, uid},
};
use anyhow::{Result, anyhow, bail, ensure};
use scoped_semantic_vectors::Vectors;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};
use typed48::{Frozen, Projected, hash};
const METHOD: &str = include_str!("scoped_semantic_method_v1.json");
const SOURCE: &[u8] = include_bytes!("scoped_semantic_probe.rs");
const HELPER: &[u8] = include_bytes!("typed48_common.rs");
const VECTOR_HELPER: &[u8] = include_bytes!("scoped_semantic_vectors.rs");
const PROFILE: EmbeddingProfile = EmbeddingProfile::BgeM3NativeFp32;
const POLICY: &str = "prompts/tutor_core.md";
const SCHEMA: &str =
    "implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json";
const HELP: &str = "scoped-semantic-probe --root PATH --output NEW_DIR\nDefault prepares six same-Store pairs (12 requests), no model.\nscoped-semantic-probe --root PATH --prepared DIR --prepared-manifest-sha256 SHA --real-synthetic (--case QUERY_ID_OR_ARM_ID | --batch 1..6 | --all)\nPairs run DenseOnly then HybridRrf. A single Hybrid arm requires a prior Dense attempt; already attempted arms cannot be retried. No delivery/apply. Exit0 is not semantic quality passed.";
fn read(path: &Path) -> Result<Vec<u8>> {
    ensure!(
        fs::metadata(path)?.len() <= 24 * 1024 * 1024,
        "Diagnostic file exceeds bound"
    );
    Ok(fs::read(path)?)
}
fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    let bytes = read(path)?;
    ensure!(
        hash(&bytes) == expected,
        "Frozen file changed: {}",
        path.display()
    );
    Ok(bytes)
}
fn write_new(path: &Path, value: &Value) -> Result<String> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(hash(&bytes))
}
fn ids(method: &Value) -> Vec<String> {
    method["batches"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|b| strings(&b["case_ids"]))
        .collect()
}
fn source_hashes(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut paths = vec![
        "rust/ai-tutor/src/context.rs".to_owned(),
        "rust/ai-tutor/src/store.rs".to_owned(),
        "rust/ai-tutor/src/tutor.rs".to_owned(),
        "rust/ai-tutor/src/kimi.rs".to_owned(),
        "rust/ai-tutor/src/memory_embedding/profile.rs".to_owned(),
        "scripts/memory-lab/typed48_common.rs".to_owned(),
        "scripts/memory-lab/typed48_projection_method_v1.json".to_owned(),
    ];
    for entry in fs::read_dir(root.join("rust/ai-tutor/src"))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if (name.starts_with("memory_") || name.starts_with("kimi_")) && name.ends_with(".rs") {
            paths.push(format!("rust/ai-tutor/src/{name}"));
        }
    }
    for entry in fs::read_dir(root.join("rust/ai-tutor/src/store"))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".rs") {
            paths.push(format!("rust/ai-tutor/src/store/{name}"));
        }
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| Ok((path.clone(), hash(&read(&root.join(path))?))))
        .collect()
}
fn executable() -> Result<Value> {
    let path = std::env::current_exe()?.canonicalize()?;
    // Release executables can exceed the small text-input bound.
    let bytes = fs::read(&path)?;
    Ok(json!({"current_exe":path.to_string_lossy(),"actual_executable_sha256":hash(&bytes)}))
}

fn decision<'a>(p: &'a Projected, alias: &str) -> Result<&'a Value> {
    p.decisions
        .iter()
        .find(|d| s(d, "source_id") == alias)
        .ok_or_else(|| anyhow!("Missing fixture source"))
}
fn pack_session(p: &Projected, pack: &str) -> Result<String> {
    let rows: Vec<_> = p
        .decisions
        .iter()
        .filter(|d| s(&d["original_source"], "pack_id") == pack)
        .collect();
    ensure!(rows.len() == 10, "Pack needs all ten actual source events");
    let sid = s(&rows[0]["live_imported_event"], "session_id").to_owned();
    ensure!(
        !sid.is_empty()
            && rows
                .iter()
                .all(|d| s(&d["live_imported_event"], "session_id") == sid),
        "Pack/session mapping differs"
    );
    Ok(sid)
}
fn seed_history(p: &Projected) -> Result<()> {
    let packs: HashSet<_> = p
        .decisions
        .iter()
        .map(|d| s(&d["original_source"], "pack_id"))
        .collect();
    ensure!(packs.len() == 6, "Expected six activities");
    // Finish before rebuilding historical state, keeping the helper's48 memories
    // unchanged. This fixture reconstructs sourced fields, not an LLM extraction.
    for pack in &packs {
        p.store.finish_session(&pack_session(p, pack)?)?;
    }
    for pack in packs {
        let sid = pack_session(p, pack)?;
        let goal = decision(p, &format!("{pack}_s10"))?;
        let plan = decision(p, &format!("{pack}_s07"))?;
        ensure!(
            goal["original_source"]["claim_type"] == "stated_goal"
                && plan["original_source"]["accepted"] == true,
            "Historical field source changed"
        );
        let mut state = p.store.session(&sid)?["state"].clone();
        for (field, d) in [("goal", goal), ("pending_step", plan)] {
            state[field] = d["original_source"]["text"].clone();
            state["field_evidence"][field] = json!({"source_event_ids":[d["event_id"]],"source_event_versions":{(s(d,"event_id")):1}});
        }
        p.store.checkpoint(&sid, &state)?;
    }
    ensure!(
        p.store.memories(&p.learner_id)?.len() == 48,
        "History reconstruction changed memory pool"
    );
    Ok(())
}
fn continue_from(store: &Store, lid: &str, source: &str, current: &str) -> Result<String> {
    let generation = store.session(current)?["generation"]
        .as_i64()
        .ok_or_else(|| anyhow!("Missing generation"))?;
    let nonce = uid("choice");
    ensure!(
        store
            .continuation_replay(lid, source, &nonce, current, generation)?
            .is_none(),
        "Fresh selection reused receipt"
    );
    let after = store.bump(current, "idle")?;
    let new = store.continue_finished_session(lid, source, &nonce, current, generation, after)?;
    ensure!(
        new["session"]["state"] == empty_state(),
        "Continuation copied old current state"
    );
    Ok(s(&new, "session_id").to_owned())
}
// Fail rather than sanitizing a production request. Natural words such as
// "reading" are allowed inside prose, but identity fields/aliases are not.
fn assert_no_leakage(value: &Value, frozen: &Frozen) -> Result<()> {
    const KEYS: &[&str] = &[
        "fixture_source_id",
        "synthetic_fixture",
        "pack_id",
        "source_alias",
        "query_id",
        "candidate_pool_ids",
        "sufficient_evidence_sets",
        "answerable",
        "expected",
        "original_label",
        "semantic_review",
        "expected_answer",
        "label",
        "labels",
    ];
    fn walk(v: &Value, f: &Frozen) -> Result<()> {
        match v {
            Value::Object(map) => {
                for (key, value) in map {
                    ensure!(
                        !KEYS.contains(&key.as_str()),
                        "Fixture/review key leaked into production request: {key}"
                    );
                    walk(value, f)?;
                }
            }
            Value::Array(rows) => {
                for v in rows {
                    walk(v, f)?;
                }
            }
            Value::String(text) => {
                ensure!(
                    !f.sources.iter().any(|v| text.contains(s(v, "id")))
                        && !f.queries.iter().any(|v| text.contains(s(v, "id"))),
                    "Fixture alias leaked into production request"
                );
                ensure!(
                    !f.sources.iter().any(|v| [
                        s(v, "learner_id"),
                        s(v, "session_id"),
                        s(v, "activity_id")
                    ]
                    .contains(&text.as_str())),
                    "Fixture identity leaked into request"
                );
            }
            _ => {}
        }
        Ok(())
    }
    walk(value, frozen)
}
fn available_aliases(context: &Value, mapping: &[Value]) -> HashSet<String> {
    let visible: HashSet<_> = context["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| s(e, "event_id"))
        .collect();
    mapping
        .iter()
        .filter(|m| !s(m, "source_alias").is_empty() && visible.contains(s(m, "event_id")))
        .map(|m| s(m, "source_alias").to_owned())
        .collect()
}

fn load_vectors(root: &Path, method: &Value) -> Result<Vectors> {
    let spec = &method["frozen_vectors"];
    let projection = root.join(s(spec, "projection_report"));
    let encoding = root.join(s(spec, "encoding_report"));
    let vectors = scoped_semantic_vectors::load_vectors(
        projection.parent().unwrap(),
        s(spec, "projection_report_sha256"),
        encoding.parent().unwrap(),
        s(spec, "encoding_report_sha256"),
    )?;
    ensure!(
        vectors.projection["encoder_inputs"]["sha256"] == spec["projection_inputs_sha256"]
            && vectors.encoding["encodings_sha256"] == spec["encodings_sha256"],
        "Fixed input/vector identity changed"
    );
    Ok(vectors)
}

fn coverage(label: &Value, aliases: &HashSet<String>) -> Value {
    if label["answerable"] == true {
        json!({"original_answerable":true,"complete_original_set":typed48::complete_set_available(&label["sufficient_evidence_sets"],aliases),"source_aliases":aliases})
    } else {
        json!({"original_answerable":false,"complete_original_set":null,"source_aliases":aliases,"abstention_requires_semantic_review":true})
    }
}

fn combinations(group: &[Value], count: usize) -> Vec<Vec<Value>> {
    fn choose(
        group: &[Value],
        count: usize,
        at: usize,
        prefix: &mut Vec<Value>,
        out: &mut Vec<Vec<Value>>,
    ) {
        if prefix.len() == count {
            out.push(prefix.clone());
            return;
        }
        for index in at..group.len() {
            prefix.push(group[index].clone());
            choose(group, count, index + 1, prefix, out);
            prefix.pop();
        }
    }
    let mut out = vec![];
    choose(group, count, 0, &mut vec![], &mut out);
    out
}

fn tie_audit(rows: &[Value], label: &Value) -> Result<Value> {
    // Actual fixture records are small. Refuse, rather than misreport, a new
    // case where a character-budget skip would require a different analysis.
    let mut sizes: Vec<_> = rows
        .iter()
        .map(|r| r["whole_chars"].as_u64().unwrap())
        .collect();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    ensure!(
        sizes.iter().take(5).sum::<u64>() <= 2400,
        "Tie analysis requires every legal top5 to fit the record budget"
    );
    let mut groups = vec![];
    let mut cutoff = Value::Null;
    let mut start = 0;
    while start < rows.len() {
        let mut end = start + 1;
        while end < rows.len() && rows[end]["score"] == rows[start]["score"] {
            end += 1;
        }
        if end - start > 1 {
            groups.push(json!({"first_rank":start+1,"last_rank":end,"score":rows[start]["score"],"members":&rows[start..end]}));
            if start < 5 && end > 5 {
                let fixed: HashSet<String> = rows[..start]
                    .iter()
                    .map(|r| s(r, "source_alias").to_owned())
                    .collect();
                let alternatives: Vec<_> = combinations(&rows[start..end], 5 - start)
                    .into_iter()
                    .map(|selection| {
                        let mut aliases = fixed.clone();
                        aliases.extend(selection.iter().map(|r| s(r, "source_alias").to_owned()));
                        json!({"members":selection,"candidate_coverage":coverage(label,&aliases)})
                    })
                    .collect();
                cutoff = json!({"first_rank":start+1,"last_rank":end,"slots":5-start,"alternatives":alternatives,"chosen_by":"production memory_id ascending; no label selection","final_context_for_hypothetical_choices_executed":false});
            }
        }
        start = end;
    }
    Ok(
        json!({"equal_score_groups":groups,"fifth_place_cut_group":cutoff,"every_legal_top5_fits_record_budget":true}),
    )
}

fn ranking_audit(
    p: &Projected,
    v: &Vectors,
    by_memory: &HashMap<String, String>,
    sid: &str,
    source_sid: &str,
    text: &str,
    query: &[f32],
    label: &Value,
) -> Result<Value> {
    let key = PROFILE.model_key();
    let session = p.store.session(sid)?;
    ensure!(
        session["continuation"]["source_session_id"] == source_sid
            && session["continuation"]["ancestor_count"] == 1,
        "Audit fixture must have exactly one real selected ancestor"
    );
    let allowed: HashSet<_> = [
        session["activity_id"].clone(),
        p.store.session(source_sid)?["activity_id"].clone(),
    ]
    .into_iter()
    .map(|v| v.as_str().unwrap().to_owned())
    .collect();
    let mut pool = vec![];
    for row in p.store.eligible_memory_embeddings(&p.learner_id, &key)? {
        let evidence = p
            .store
            .memory_evidence(&p.learner_id, &row.snapshot.memory_id)?;
        if !allowed.contains(s(&evidence["activity_binding"], "activity_id")) {
            continue;
        }
        let alias = &by_memory[&row.snapshot.memory_id];
        ensure!(
            row.vector == v.values[&format!("source:{alias}")],
            "Audit cache vector changed"
        );
        pool.push(json!({"source_alias":alias,"memory_id":row.snapshot.memory_id,"score":cosine(query,&row.vector),"whole_chars":row.snapshot.content.chars().count(),"memory":evidence["memory"],"source_links":evidence["source_links"]}));
    }
    ensure!(
        pool.len() == 8,
        "Original selected activity must contribute exactly eight typed memories"
    );
    pool.sort_by(|a, b| {
        b["score"]
            .as_f64()
            .unwrap()
            .total_cmp(&a["score"].as_f64().unwrap())
            .then_with(|| s(a, "memory_id").cmp(s(b, "memory_id")))
    });
    let lexical = p.store.recall_for_session(sid, text, 5)?;
    let mut union: HashMap<String, Value> = HashMap::new();
    for (i, memory) in lexical.iter().enumerate() {
        let mid = s(memory, "memory_id");
        ensure!(
            pool.iter().any(|r| s(r, "memory_id") == mid),
            "Lexical candidate outside actual scoped typed pool"
        );
        union.insert(mid.into(),json!({"memory_id":mid,"source_alias":by_memory[mid],"score":1.0/(61.0+i as f64),"lexical_rank":i+1,"semantic_rank":null,"whole_chars":s(memory,"content").chars().count()}));
    }
    for (i, row) in pool.iter().take(5).enumerate() {
        let mid = s(row, "memory_id");
        let entry=union.entry(mid.into()).or_insert_with(||json!({"memory_id":mid,"source_alias":row["source_alias"],"score":0.0,"lexical_rank":null,"whole_chars":row["whole_chars"]}));
        entry["score"] = json!(entry["score"].as_f64().unwrap() + 1.0 / (61.0 + i as f64));
        entry["semantic_rank"] = json!(i + 1);
    }
    let mut hybrid: Vec<_> = union.into_values().collect();
    hybrid.sort_by(|a, b| {
        b["score"]
            .as_f64()
            .unwrap()
            .total_cmp(&a["score"].as_f64().unwrap())
            .then_with(|| s(a, "memory_id").cmp(s(b, "memory_id")))
    });
    for (mode, expected) in [
        (CandidateRanking::DenseOnly, &pool),
        (CandidateRanking::HybridRrf, &hybrid),
    ] {
        let actual = p.store.recall_candidates_for_session_ranked(
            sid,
            text,
            QueryEmbedding {
                model_key: &key,
                values: query,
            },
            5,
            mode,
        )?;
        ensure!(
            actual
                .iter()
                .map(|c| s(&c.memory, "memory_id"))
                .collect::<Vec<_>>()
                == expected
                    .iter()
                    .take(5)
                    .map(|r| s(r, "memory_id"))
                    .collect::<Vec<_>>(),
            "Independent scoped audit disagrees with production selection"
        );
        ensure!(
            actual
                .iter()
                .zip(expected)
                .all(|(a, b)| a.ranking_score == b["score"].as_f64().unwrap()),
            "Independent score differs from production"
        );
    }
    let aliases = pool
        .iter()
        .map(|r| s(r, "source_alias").to_owned())
        .collect();
    Ok(
        json!({"scope_basis":"actual explicit continuation identities; asserted against production rankings","allowed_activity_ids":allowed,"eligible_scoped_pool":pool,"pool_coverage":coverage(label,&aliases),"lexical_top5":lexical,"hybrid_full_union":hybrid,"dense_ties":tie_audit(&pool,label)?,"hybrid_ties":tie_audit(&hybrid,label)?}),
    )
}

fn build_pair(frozen: &Frozen, v: &Vectors, id: &str) -> Result<(Vec<Value>, Vec<Value>, Value)> {
    let p = typed48::project(frozen)?;
    let by_memory = scoped_semantic_vectors::compare_projection(&p, frozen, v)?;
    seed_history(&p)?;
    let cache = scoped_semantic_vectors::index(&p, v, &by_memory)?;
    let query = frozen
        .queries
        .iter()
        .find(|q| s(q, "id") == id)
        .ok_or_else(|| anyhow!("Unknown original query"))?;
    let label = frozen
        .labels
        .iter()
        .find(|l| s(l, "query_id") == id)
        .ok_or_else(|| anyhow!("Missing original label"))?;
    let text = s(query, "text");
    let vector_key = format!("current_query:{id}");
    ensure!(
        v.inputs[&vector_key]["text"] == text
            && v.inputs[&vector_key]["text_sha256"] == hash(text.as_bytes()),
        "Exact frozen query text/hash mismatch"
    );
    let values = &v.values[&vector_key];
    let key = PROFILE.model_key();
    let source_sid = pack_session(&p, s(query, "pack_id"))?;
    let sid = continue_from(&p.store, &p.learner_id, &source_sid, &source_sid)?;
    let generation = p.store.bump(&sid, "generating")?;
    let event = p.store.record_education_event(
        &p.learner_id,
        &sid,
        "simulated",
        "learner_statement",
        &json!({"text":text}),
    )?;
    let eid = s(&event, "event_id");
    let turn = p.store.add_turn(
        &sid,
        &uid("fixture_turn"),
        generation,
        "user",
        text,
        "shown",
        &[eid.to_owned()],
    )?;
    let mut state = p.store.session(&sid)?["state"].clone();
    state["latest_input"] = json!(text);
    p.store.checkpoint(&sid, &state)?;
    let session = p.store.session(&sid)?;
    let learner = p.store.learner(&p.learner_id)?;
    ensure!(
        memory_recall_query(text, s(&session["state"], "goal")) == Some(text),
        "Query routing changed frozen input"
    );
    let vision = json!({"selected_status":"unknown","selected_track_key":null,"frame_age_ms":null});
    let default_context = build_context(&p.store, &session, &learner, text, eid, &vision)?;
    assert_no_leakage(&default_context, frozen)?;
    let ranking = ranking_audit(&p, v, &by_memory, &sid, &source_sid, text, values, label)?;
    let mut mapping:Vec<_>=p.decisions.iter().map(|d|json!({"source_alias":d["source_id"],"event_id":d["event_id"],"session_id":d["live_imported_event"]["session_id"],"original_text":d["original_source"]["text"],"memory_id":d["memory_id"],"projected":d["projected"]})).collect();
    mapping.push(json!({"source_alias":null,"event_id":event["event_id"],"session_id":sid,"source_role":"current_question","original_content":event["content"]}));
    let selection_id = s(&session["continuation"], "selection_event_id");
    mapping.push(json!({"source_alias":null,"event_id":selection_id,"session_id":sid,"source_role":"explicit_activity_choice"}));
    let mut requests = vec![];
    let mut reviews = vec![];
    for (arm, mode) in [
        ("dense_only", CandidateRanking::DenseOnly),
        ("hybrid_rrf", CandidateRanking::HybridRrf),
    ] {
        let context = build_context_with_semantic(
            &p.store,
            &session,
            &learner,
            text,
            eid,
            &vision,
            text,
            QueryEmbedding {
                model_key: &key,
                values,
            },
            mode,
        )?;
        let rid = uid("request");
        let case_id = format!("{id}-{arm}");
        let request = json!({"request_id":rid,"learner_id":p.learner_id,"session_id":sid,"generation":generation,"context":context});
        assert_no_leakage(&request, frozen)?;
        let visible_ids: Vec<_> = context["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| s(e, "event_id").to_owned())
            .collect();
        let live = p.store.source_events(&p.learner_id, &visible_ids)?;
        ensure!(
            live.len() == visible_ids.len()
                && live
                    .iter()
                    .all(|e| [sid.as_str(), source_sid.as_str()].contains(&s(e, "session_id"))),
            "Context source outside actual choice"
        );
        let mut arm_mapping = mapping.clone();
        for row in &mut arm_mapping {
            row["visible_in_context"] = json!(visible_ids.iter().any(|v| v == s(row, "event_id")));
        }
        let aliases = available_aliases(&context, &mapping);
        let candidates = p.store.recall_candidates_for_session_ranked(
            &sid,
            text,
            QueryEmbedding {
                model_key: &key,
                values,
            },
            5,
            mode,
        )?;
        let candidate_aliases = candidates
            .iter()
            .map(|c| by_memory[s(&c.memory, "memory_id")].clone())
            .collect();
        reviews.push(json!({"case_id":case_id,"query_id":id,"arm":arm,"original_query":query,"original_label":label,"source_mapping":arm_mapping,"actual_context_live_sources":live,"actual_session":session,"actual_user_turn_id":turn,"candidate_top5":candidates,"candidate_coverage":coverage(label,&candidate_aliases),"actual_context_coverage":coverage(label,&aliases),"actual_context_complete_original_set":coverage(label,&aliases)["complete_original_set"],"temporary_store_only":true,"context_unmodified_after_build":true,"semantic_quality_pass":null}));
        requests.push(json!({"case_id":case_id,"query_id":id,"arm":arm,"request":request}));
    }
    ensure!(
        p.store.embedding_work(&p.learner_id, &key, 32)?.is_empty(),
        "Pair unexpectedly changed cache eligibility"
    );
    let default_aliases = available_aliases(&default_context, &mapping);
    Ok((
        requests,
        reviews,
        json!({"query_id":id,"cache_import":cache,"query_vector_key":vector_key,"query_text_sha256":hash(text.as_bytes()),"query_vector_sha256_f32le":hash(&values.iter().flat_map(|v|v.to_le_bytes()).collect::<Vec<_>>()),"ranking":ranking,"default_lexical_context":default_context,"default_lexical_coverage":coverage(label,&default_aliases),"same_store_same_ids_same_snapshot":true,"source_mapping":mapping,"actual_session":session}),
    ))
}

fn prepare(root: &Path, output: &Path) -> Result<()> {
    for (name, bytes) in [
        ("scoped_semantic_probe.rs", SOURCE),
        ("scoped_semantic_vectors.rs", VECTOR_HELPER),
        ("typed48_common.rs", HELPER),
        ("scoped_semantic_method_v1.json", METHOD.as_bytes()),
    ] {
        checked(&root.join("scripts/memory-lab").join(name), &hash(bytes))?;
    }
    let method: Value = serde_json::from_str(METHOD)?;
    let frozen = typed48::load(root)?;
    let vectors = load_vectors(root, &method)?;
    let policy = read(&root.join(POLICY))?;
    let schema_bytes = read(&root.join(SCHEMA))?;
    let schema: Value = serde_json::from_slice(&schema_bytes)?;
    let mut requests = vec![];
    let mut reviews = vec![];
    let mut audits = vec![];
    for batch in method["batches"].as_array().unwrap() {
        let (pair, review, audit) = build_pair(&frozen, &vectors, s(batch, "query_id"))?;
        for row in &pair {
            ai_tutor::kimi::build_prompt(std::str::from_utf8(&policy)?, &schema, &row["request"])?;
        }
        requests.extend(pair);
        reviews.extend(review);
        audits.push(audit);
    }
    ensure!(
        requests.len() == 12
            && requests
                .iter()
                .map(|r| s(r, "case_id"))
                .eq(ids(&method).iter().map(String::as_str)),
        "Prepared order changed"
    );
    fs::create_dir(output)?;
    let requests_sha = write_new(&output.join("requests.json"), &json!(requests))?;
    let review_sha = write_new(&output.join("review-cases.json"), &json!(reviews))?;
    let audit_sha = write_new(&output.join("store-audits.json"), &json!(audits))?;
    let method_sha = write_new(&output.join("method.json"), &method)?;
    write_new(
        &output.join("manifest.json"),
        &json!({"status":"prepared_no_model","method_source_sha256":hash(METHOD.as_bytes()),"method_file_sha256":method_sha,"probe_source_sha256":hash(SOURCE),"vector_helper_source_sha256":hash(VECTOR_HELPER),"helper_source_sha256":hash(HELPER),"executable":executable()?,"production_sources":source_hashes(root)?,"fixture_manifest_sha256":typed48::FIXTURE_SHA,"fixture_files":frozen.files,"frozen_vectors":method["frozen_vectors"],"profile":PROFILE.metadata(),"requests_sha256":requests_sha,"review_cases_sha256":review_sha,"store_audits_sha256":audit_sha,"policy_sha256":hash(&policy),"schema_sha256":hash(&schema_bytes),"case_count":12,"temporary_store_count":6,"positive_denominator_per_arm":5,"negative_denominator_per_arm":1,"production_database_opened":false,"provider_constructed":false,"model_called":false,"delivered":false,"candidates_applied":false,"semantic_quality_pass":null}),
    )?;
    println!("Prepared six same-Store pairs (12 requests); no model called.");
    Ok(())
}

enum Selection {
    Case(String),
    Batch(usize),
    All,
}
fn selected(method: &Value, selection: &Selection) -> Result<Vec<usize>> {
    let index = match selection {
        Selection::Case(id) => {
            if let Some(index) = ids(method).iter().position(|arm| arm == id) {
                return Ok(vec![index]);
            }
            Some(
                method["batches"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .position(|b| s(b, "query_id") == id)
                    .ok_or_else(|| anyhow!("Unknown original query or arm"))?,
            )
        }
        Selection::Batch(n) => {
            ensure!((1..=6).contains(n), "Batch must be1..6");
            Some(n - 1)
        }
        Selection::All => None,
    };
    Ok(match index {
        Some(i) => vec![i * 2, i * 2 + 1],
        None => (0..12).collect(),
    })
}
fn validate_attempt_selection(prepared: &Path, order: &[String], indices: &[usize]) -> Result<()> {
    for &i in indices {
        ensure!(
            !prepared.join(format!("attempt-{}.json", order[i])).exists(),
            "Arm already attempted; implicit retry refused"
        );
        if i % 2 == 1 && !indices.contains(&(i - 1)) {
            ensure!(
                prepared
                    .join(format!("attempt-{}.json", order[i - 1]))
                    .exists(),
                "A single Hybrid arm requires the prior Dense attempt"
            );
        }
    }
    Ok(())
}

fn citation_check(reply: &Value, review: &Value) -> Value {
    let used: HashSet<_> = strings(&reply["used_evidence_ids"]).into_iter().collect();
    let aliases: HashSet<String> = review["source_mapping"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| {
            !s(m, "source_alias").is_empty()
                && m["visible_in_context"] == true
                && used.contains(s(m, "event_id"))
        })
        .map(|m| s(m, "source_alias").to_owned())
        .collect();
    let label = &review["original_label"];
    let cited_actual_sources:Vec<_>=review["actual_context_live_sources"].as_array().unwrap().iter().filter(|e|used.contains(s(e,"event_id"))).map(|e|json!({"event_id":e["event_id"],"event_version":e["event_version"],"session_id":e["session_id"],"activity_id":e["activity_id"]})).collect();
    json!({"cited_actual_context_sources":cited_actual_sources,"cited_visible_source_aliases":aliases,"complete_original_set_cited":if label["answerable"]==true {json!(typed48::complete_set_available(&label["sufficient_evidence_sets"],&aliases))}else{Value::Null},
        "task_candidate_count":reply["task_update_candidates"].as_array().map(Vec::len),"memory_candidate_count":reply["memory_candidates"].as_array().map(Vec::len),
        "semantic_review_required":true,"meaning":"ID coverage is not entailment; actual continuation/resolution sources are included, hidden sources cannot count"})
}
async fn run(root: &Path, prepared: &Path, manifest_sha: &str, selection: Selection) -> Result<()> {
    let manifest: Value =
        serde_json::from_slice(&checked(&prepared.join("manifest.json"), manifest_sha)?)?;
    ensure!(
        manifest["probe_source_sha256"] == hash(SOURCE)
            && manifest["helper_source_sha256"] == hash(HELPER)
            && manifest["vector_helper_source_sha256"] == hash(VECTOR_HELPER)
            && manifest["method_source_sha256"] == hash(METHOD.as_bytes()),
        "Prepared diagnostic code changed"
    );
    ensure!(
        manifest["executable"] == executable()?,
        "Actual executable/path changed; prepare a new experiment"
    );
    ensure!(
        manifest["production_sources"] == json!(source_hashes(root)?),
        "Production source changed; prepare a new experiment"
    );
    checked(&root.join(POLICY), s(&manifest, "policy_sha256"))?;
    checked(&root.join(SCHEMA), s(&manifest, "schema_sha256"))?;
    let frozen = typed48::load(root)?;
    ensure!(
        manifest["fixture_files"] == frozen.files,
        "Frozen fixture files changed"
    );
    let method: Value = serde_json::from_slice(&checked(
        &prepared.join("method.json"),
        s(&manifest, "method_file_sha256"),
    )?)?;
    ensure!(
        method == serde_json::from_str::<Value>(METHOD)?,
        "Method changed"
    );
    let requests: Vec<Value> = serde_json::from_slice(&checked(
        &prepared.join("requests.json"),
        s(&manifest, "requests_sha256"),
    )?)?;
    let reviews: Vec<Value> = serde_json::from_slice(&checked(
        &prepared.join("review-cases.json"),
        s(&manifest, "review_cases_sha256"),
    )?)?;
    checked(
        &prepared.join("store-audits.json"),
        s(&manifest, "store_audits_sha256"),
    )?;
    // Recheck the exact fixed encoding artifacts; no encoding is performed.
    load_vectors(root, &method)?;
    let order = ids(&method);
    ensure!(
        requests.len() == 12
            && reviews.len() == 12
            && requests
                .iter()
                .zip(&order)
                .all(|(r, id)| s(r, "case_id") == id)
            && reviews
                .iter()
                .zip(&order)
                .all(|(r, id)| s(r, "case_id") == id),
        "Prepared order changed"
    );
    for row in &requests {
        assert_no_leakage(&row["request"], &frozen)?;
    }
    let indices = selected(&method, &selection)?;
    validate_attempt_selection(prepared, &order, &indices)?;
    let provider = KimiProvider::new(root)?;
    let run_id = uid("run");
    let start = Instant::now();
    let mut outcomes = vec![];
    let operation:Result<()>=async {
        let audit=provider.probe().await?;
        ensure!(audit["available"]==true,"Verified existing tool-free provider required");
        let identity=json!({"version":audit["version"],"binary_sha256":audit["binary_sha256"],"agent_sha256":audit["agent_sha256"],"tools":audit["tools"],"subagents":audit["subagents"],"model_identity_independently_frozen":false,"sampling_independently_frozen":false});
        write_new(&prepared.join(format!("{run_id}-start.json")),&json!({"provider":identity,"case_ids":indices.iter().map(|&i|&order[i]).collect::<Vec<_>>(),"prepared_manifest_sha256":manifest_sha,"retries":0}))?;
        for &i in &indices {
            let id=&order[i];
            let request=&requests[i]["request"];
            write_new(&prepared.join(format!("attempt-{id}.json")),&json!({"case_id":id,"request_id":request["request_id"],"request_sha256":hash(&serde_json::to_vec(request)?),"run_id":run_id,"attempted_at":chrono::Utc::now().to_rfc3339(),"retries":0}))?;
            let began=Instant::now();
            let reply=provider.generate(request).await;
            let mut record=json!({"case_id":id,"run_id":run_id,"elapsed_ms":began.elapsed().as_secs_f64()*1000.0,"provider":identity,"reply_validated":false,"reply":null,"error":null,"semantic_review_status":"pending_review","semantic_quality_pass":null,"delivered":false,"candidates_applied":false});
            match reply {
                Ok(reply)=>{record["citation_checks"]=citation_check(&reply,&reviews[i]);record["reply_validated"]=json!(true);record["reply"]=reply;write_new(&prepared.join(format!("result-{id}.json")),&record)?;outcomes.push(json!({"case_id":id,"status":"validated_pending_semantic_review"}));},
                Err(error)=>{record["error"]=json!(error.to_string());write_new(&prepared.join(format!("result-{id}.json")),&record)?;outcomes.push(json!({"case_id":id,"status":"provider_or_protocol_failed"}));return Err(error);}
            }
        }
        Ok(())
    }.await;
    let closed = provider.close().await;
    write_new(
        &prepared.join(format!("{run_id}-summary.json")),
        &json!({"requested_case_ids":indices.iter().map(|&i|&order[i]).collect::<Vec<_>>(),"outcomes":outcomes,
        "operation_error":operation.as_ref().err().map(ToString::to_string),"provider_close_ok":closed.is_ok(),"provider_close_error":closed.as_ref().err().map(ToString::to_string),"provider_running_after_close":provider.status()["running"],"elapsed_ms":start.elapsed().as_secs_f64()*1000.0,
        "cleanup_scope":"Existing provider close/running count only; no invented remote cancellation acknowledgment","semantic_quality_pass":null,"delivered":false,"candidates_applied":false}),
    )?;
    operation?;
    closed?;
    println!("Synthetic results saved for semantic review; no quality decision made.");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut root = std::env::current_dir()?;
    let (mut output, mut prepared, mut manifest_sha, mut selection) = (None, None, None, None);
    let mut real = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                return Ok(());
            }
            "--root" => root = PathBuf::from(args.next().ok_or_else(|| anyhow!("Missing root"))?),
            "--output" => {
                ensure!(output.is_none(), "Duplicate output");
                output = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow!("Missing output"))?,
                ));
            }
            "--prepared" => {
                ensure!(prepared.is_none(), "Duplicate prepared");
                prepared = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow!("Missing prepared"))?,
                ));
            }
            "--prepared-manifest-sha256" => {
                ensure!(manifest_sha.is_none(), "Duplicate hash");
                manifest_sha = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("Missing manifest hash"))?,
                );
            }
            "--real-synthetic" => {
                ensure!(!real, "Duplicate real flag");
                real = true;
            }
            "--case" | "--batch" | "--all" => {
                ensure!(selection.is_none(), "Choose one selector");
                selection = Some(match arg.as_str() {
                    "--case" => {
                        Selection::Case(args.next().ok_or_else(|| anyhow!("Missing case"))?)
                    }
                    "--batch" => Selection::Batch(
                        args.next()
                            .ok_or_else(|| anyhow!("Missing batch"))?
                            .parse()?,
                    ),
                    _ => Selection::All,
                });
            }
            _ => bail!("Unknown diagnostic argument"),
        }
    }
    let root = root.canonicalize()?;
    if real {
        ensure!(output.is_none(), "Real execution uses frozen preparation");
        run(
            &root,
            &prepared.ok_or_else(|| anyhow!("--prepared required"))?,
            &manifest_sha.ok_or_else(|| anyhow!("--prepared-manifest-sha256 required"))?,
            selection.ok_or_else(|| anyhow!("Explicit selector required"))?,
        )
        .await
    } else {
        ensure!(
            prepared.is_none() && manifest_sha.is_none() && selection.is_none(),
            "--real-synthetic required; no model called"
        );
        prepare(
            &root,
            &output.ok_or_else(|| anyhow!("--output NEW_DIR required"))?,
        )
    }
}

#[cfg(test)]
#[path = "scoped_semantic_tests.rs"]
mod tests;
