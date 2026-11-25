//! Synthetic downstream grounding diagnostic; frozen retrieval, no live Store.
use ai_tutor::{
    context::{MAX_CONTEXT_CHARS, memory_recall_query},
    kimi::KimiProvider,
    store::{s, strings, uid},
};
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

const METHOD: &str = include_str!("tutor_evidence_method_v1.json");
const SOURCE: &[u8] = include_bytes!("tutor_evidence_probe.rs");
const SNAPSHOT: &str = "runtime/goal-baselines/20261104/typed48-store-first";
const FIXTURE: &str = "rust/ai-tutor/tests/fixtures/diagnostic_postfreeze_v2";
const POLICY: &str = "prompts/tutor_core.md";
const SCHEMA: &str =
    "implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json";
const HELP: &str = "tutor-evidence-probe --root PATH --output NEW_DIR\n  Prepare36 only, no model or learner DB.\ntutor-evidence-probe --root PATH --prepared DIR --prepared-manifest-sha256 SHA --real-synthetic (--case QUERY_ID | --batch 1..6 | --all)\n  One attempt per case; production generate; retains synthetic replies for semantic review. No delivery/apply. Exit0 is NOT semantic quality passed.";

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read(path: &Path) -> Result<Vec<u8>> {
    ensure!(
        fs::metadata(path)?.len() < 24 * 1024 * 1024,
        "Diagnostic input exceeds bound"
    );
    Ok(fs::read(path)?)
}
fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    let bytes = read(path)?;
    ensure!(
        hash(&bytes) == expected,
        "Frozen diagnostic input changed: {}",
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
fn records(bytes: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(bytes)?
        .lines()
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}
fn unique(rows: Vec<Value>, key: &str) -> Result<BTreeMap<String, Value>> {
    let count = rows.len();
    let map: BTreeMap<_, _> = rows
        .into_iter()
        .map(|row| (s(&row, key).to_owned(), row))
        .collect();
    ensure!(
        map.len() == count && !map.contains_key(""),
        "Missing or duplicate fixture identity"
    );
    Ok(map)
}
fn ordered_ids(method: &Value) -> Vec<String> {
    method["batches"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|b| strings(&b["query_ids"]))
        .collect()
}

struct Suite {
    requests: Vec<Value>,
    review: Vec<Value>,
    inputs: Value,
}

// No labels or expected claims enter this function or the model request.
fn request(query: &Value, selected: &[&Value]) -> Result<Value> {
    ensure!(selected.len() == 5, "Expected frozen top five");
    let owner = s(&selected[0]["memory_evidence"]["memory"], "learner_id");
    let session = uid("session_evidence_probe");
    let current = uid("event_evidence_probe");
    let created = chrono::Utc::now().to_rfc3339();
    let mut evidence = vec![
        json!({"event_id":current,"source_type":"simulated","event_kind":"learner_statement",
        "event_version":1,"event_status":"active","created_at":created,"historical":false,
        "original_content_included":false,"content_reference":{"path":"user_input","verbatim":true}}),
    ];
    let mut memories = Vec::new();
    let mut chars = 0;
    for decision in selected {
        let live = &decision["memory_evidence"];
        let memory = &live["memory"];
        ensure!(
            s(memory, "learner_id") == owner && live["namespace"] == "simulator",
            "Snapshot owner mismatch"
        );
        chars += s(memory, "content").chars().count();
        let mut compact = json!({"historical":true,"education_metadata":live["education_metadata"],"source_event_versions":{}});
        for key in [
            "memory_id",
            "kind",
            "content",
            "source_type",
            "source_event_ids",
            "scope",
            "status",
            "activity_scope",
        ] {
            compact[key] = memory[key].clone();
        }
        for link in live["source_links"].as_array().unwrap() {
            compact["source_event_versions"][s(link, "event_id")] = link["event_version"].clone();
        }
        for source in live["sources"].as_array().unwrap() {
            let mut entry = json!({"historical":true,"original_content_included":false,
                "content_reference":{"memory_id":memory["memory_id"],"verbatim":false,"meaning":"historical summary, not current hardware"}});
            for key in [
                "event_id",
                "source_type",
                "event_kind",
                "event_version",
                "event_status",
                "created_at",
            ] {
                entry[key] = source[key].clone();
            }
            evidence.push(entry);
        }
        memories.push(compact);
    }
    ensure!(chars <= 2400, "Whole-record budget changed");
    let context = json!({"language":if query["language"]=="en" {"en"} else {"zh"},"help_style":"guided",
        "reading_level":"standard","reading_guidance":"reading_level只控制表达复杂度：plain使用短句、少术语；standard常规解释；technical可以使用术语并解释必要概念。不得据此推断年龄、能力、人格或是否已掌握知识。界面语言不决定教学语言。",
        "namespace":"simulator","activity":"modular_rover",
        "task":{"goal":query["goal"],"goal_source":"synthetic_fixture_context","stage":"goal_setting","reported_result":"","pending_step":"","modules":null,
            "modules_source":"unknown","requires_inventory_confirmation":true,"session_preferences":{},"last_delivered_hint":"","field_evidence":{},"attempts":[]},
        "user_input":query["text"],"current_user_event_id":current,"slow_memory":memories,"recent_delivered_dialogue":[],
        "evidence":evidence,"vision":{"selected_status":"unknown","selected_track_key":null,"frame_age_ms":null},"current_hardware_status":"unknown",
        "rules":"本轮输入与必要事实完整保留。引用是标注的报告或历史摘要，不是传感器确认。历史配置不是当前；建议不是完成；成功后改进仍为成功。没有情绪、积木视觉或执行能力。持久更正必须同时引用被更正的旧来源ID和当前明确更正来源ID，不确定时先澄清。",
        "context_selection":{"max_chars":MAX_CONTEXT_CHARS,"selection":"whole_records; current_facts_not_truncated"}});
    ensure!(
        context.to_string().chars().count() <= MAX_CONTEXT_CHARS - 256,
        "Snapshot context exceeds production budget"
    );
    Ok(
        json!({"request_id":uid("request_evidence_probe"),"learner_id":owner,"session_id":session,"generation":1,"context":context}),
    )
}

fn suite(root: &Path) -> Result<Suite> {
    let method: Value = serde_json::from_str(METHOD)?;
    let snapshot: Value = serde_json::from_slice(&checked(
        &root.join(SNAPSHOT),
        s(&method, "snapshot_sha256"),
    )?)?;
    let base = root.join(FIXTURE);
    let manifest: Value = serde_json::from_slice(&checked(
        &base.join("manifest.json"),
        s(&method, "fixture_manifest_sha256"),
    )?)?;
    let mut files = BTreeMap::new();
    for name in [
        "sources.jsonl",
        "queries.jsonl",
        "labels.jsonl",
        "scope_controls.jsonl",
    ] {
        files.insert(
            name,
            records(&checked(
                &base.join(name),
                s(&manifest["files"][name], "sha256"),
            )?)?,
        );
    }
    let sources = unique(files.remove("sources.jsonl").unwrap(), "id")?;
    let queries = unique(files.remove("queries.jsonl").unwrap(), "id")?;
    let labels = unique(files.remove("labels.jsonl").unwrap(), "query_id")?;
    ensure!(
        sources.len() == 60
            && queries.len() == 96
            && labels.len() == 96
            && files["scope_controls.jsonl"].len() == 12,
        "Frozen corpus count changed"
    );
    ensure!(
        snapshot["profile"]["model_key"]
            == "e8ea69aa701cae2e76b2e742053839773a19bbee66e53278864485d98b61bbf0",
        "Wrong vector profile"
    );
    let decisions = unique(
        snapshot["reconstructed_decisions"]
            .as_array()
            .ok_or_else(|| anyhow!("Missing snapshot decisions"))?
            .clone(),
        "source_id",
    )?;
    let mut projected = 0;
    for (sid, d) in &decisions {
        ensure!(
            sources.get(sid) == Some(&d["original_source"]),
            "Original source changed"
        );
        if d["projected"] != true {
            continue;
        }
        projected += 1;
        let ev = &d["memory_evidence"];
        let memory = &ev["memory"];
        let srcs = ev["sources"]
            .as_array()
            .ok_or_else(|| anyhow!("Missing sources"))?;
        let links = ev["source_links"]
            .as_array()
            .ok_or_else(|| anyhow!("Missing source links"))?;
        ensure!(
            srcs.len() == 1 && links.len() == 1,
            "This fixed projection requires one complete source per memory"
        );
        let source = &srcs[0];
        ensure!(
            ev["namespace"] == "simulator"
                && memory["source_type"] == "simulated"
                && source["source_type"] == "simulated"
                && memory["status"] == "active"
                && source["event_status"] == "active"
                && memory["learner_id"] == source["learner_id"]
                && memory["content"] == d["encoding_text"]
                && hash(s(memory, "content").as_bytes()) == s(d, "encoding_text_sha256")
                && source["content"]["text"] == sources[sid]["text"]
                && source["event_kind"] == sources[sid]["event_kind"]
                && source["event_version"] == sources[sid]["version"]
                && links[0]["event_id"] == source["event_id"]
                && links[0]["event_version"] == source["event_version"]
                && memory["source_event_ids"] == json!([source["event_id"]]),
            "Snapshot text/provenance mismatch"
        );
    }
    ensure!(
        projected == 48 && decisions.len() == 60,
        "Wrong projected pool"
    );
    let evaluation = snapshot["evaluations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["condition"] == "current_query")
        .ok_or_else(|| anyhow!("Missing current-query ranking"))?;
    let cases = unique(evaluation["cases"].as_array().unwrap().clone(), "query_id")?;
    let mut requests = Vec::new();
    let mut review = Vec::new();
    let ids = ordered_ids(&method);
    ensure!(
        ids.len() == 36 && ids.iter().collect::<HashSet<_>>().len() == 36,
        "Wrong diagnostic order"
    );
    let mut counts = [0, 0, 0];
    for (index, qid) in ids.iter().enumerate() {
        let query = &queries[qid];
        let label = &labels[qid];
        let case = &cases[qid];
        ensure!(
            case["query_text_sha256"] == hash(s(query, "text").as_bytes())
                && case["original_sufficient_evidence_sets"] == label["sufficient_evidence_sets"],
            "Query or labels changed"
        );
        ensure!(
            memory_recall_query(s(query, "text"), s(query, "goal")) == Some(s(query, "text")),
            "This diagnostic must use concrete current questions"
        );
        let selected_ids = strings(&case["pure_bge"]["selection"]["source_ids"]);
        let selected: Vec<_> = selected_ids
            .iter()
            .map(|sid| {
                decisions
                    .get(sid)
                    .ok_or_else(|| anyhow!("Missing selected memory"))
            })
            .collect::<Result<_>>()?;
        ensure!(
            selected.iter().all(|d| d["projected"] == true),
            "Excluded source selected"
        );
        let ranking = case["pure_bge"]["ranking"].as_array().unwrap();
        ensure!(
            selected_ids
                == ranking
                    .iter()
                    .take(5)
                    .map(|r| s(r, "source_id").to_owned())
                    .collect::<Vec<_>>(),
            "Frozen top5 order changed"
        );
        let request = request(query, &selected)?;
        let selected_set: HashSet<_> = selected_ids.iter().map(String::as_str).collect();
        let sufficient = label["sufficient_evidence_sets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|set| {
                strings(set)
                    .iter()
                    .all(|id| selected_set.contains(id.as_str()))
            });
        let group = if label["answerable"] == false {
            counts[0] += 1;
            "negative_original_label"
        } else if sufficient {
            counts[1] += 1;
            "positive_sufficient_retrieved"
        } else {
            counts[2] += 1;
            "positive_insufficient_retrieved"
        };
        let mappings: Vec<_> = selected.iter().map(|d|json!({"source_id":d["source_id"],"memory_id":d["memory_id"],"event_id":d["memory_evidence"]["sources"][0]["event_id"],"event_version":d["memory_evidence"]["sources"][0]["event_version"]})).collect();
        requests.push(json!({"query_id":qid,"request":request}));
        review.push(json!({"query_id":qid,"batch":index/6+1,"group":group,"original_language":query["language"],"mode":query["mode"],
            "retrieved_source_ids":selected_ids,"source_mapping":mappings,"original_label":label,"retrieved_complete_set":sufficient,
            "semantic_review":{"status":"pending_review","supported_history":null,"complete_correct_answer":null,"support_status_units_preserved":null,"unsupported_updates":null,"notes":null},
            "quality_pass":null}));
    }
    ensure!(counts == [24, 9, 3], "Diagnostic strata changed");
    Ok(Suite {
        requests,
        review,
        inputs: json!({"snapshot_sha256":method["snapshot_sha256"],"fixture_manifest_sha256":method["fixture_manifest_sha256"],"fixture_files":manifest["files"]}),
    })
}

fn prepare(root: &Path, output: &Path) -> Result<()> {
    let suite = suite(root)?;
    let policy = read(&root.join(POLICY))?;
    let schema: Value = serde_json::from_slice(&read(&root.join(SCHEMA))?)?;
    for row in &suite.requests {
        let prompt =
            ai_tutor::kimi::build_prompt(std::str::from_utf8(&policy)?, &schema, &row["request"])?;
        ensure!(!prompt.is_empty(), "Empty teacher prompt");
    }
    fs::create_dir(output)?; // Refuse an existing preparation; never overwrite evidence.
    let method: Value = serde_json::from_str(METHOD)?;
    let requests_sha = write_new(&output.join("requests.json"), &json!(suite.requests))?;
    let review_sha = write_new(&output.join("review-cases.json"), &json!(suite.review))?;
    write_new(&output.join("method.json"), &method)?;
    write_new(
        &output.join("manifest.json"),
        &json!({"status":"prepared_no_model","method_sha256":hash(METHOD.as_bytes()),
        "compiled_probe_sha256":hash(SOURCE),"inputs":suite.inputs,"requests_sha256":requests_sha,"review_cases_sha256":review_sha,
        "policy_sha256":hash(&policy),"schema_sha256":hash(&read(&root.join(SCHEMA))?),"case_count":36,
        "model_called":false,"learner_database_opened":false,"delivered":false,"candidates_applied":false,"semantic_quality_pass":null}),
    )?;
    println!(
        "Prepared 36 frozen synthetic requests at {}",
        output.display()
    );
    Ok(())
}

fn citation_check(reply: &Value, request: &Value, review: &Value) -> Value {
    let used: HashSet<_> = strings(&reply["used_evidence_ids"]).into_iter().collect();
    let cited: Vec<_> = review["source_mapping"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| used.contains(s(m, "event_id")))
        .map(|m| s(m, "source_id").to_owned())
        .collect();
    let complete = review["original_label"]["sufficient_evidence_sets"]
        .as_array()
        .unwrap()
        .iter()
        .any(|set| strings(set).iter().all(|id| cited.contains(id)));
    json!({"cited_retrieved_source_ids":cited,"cites_current_question":used.contains(s(&request["context"],"current_user_event_id")),
        "citations_cover_complete_original_set":if review["original_label"]["answerable"]==true {json!(complete)} else {Value::Null},
        "task_candidate_count":reply["task_update_candidates"].as_array().unwrap().len(),"memory_candidate_count":reply["memory_candidates"].as_array().unwrap().len(),
        "semantic_review_required":true,"meaning":"ID availability and set coverage do not establish entailment, answer correctness or abstention"})
}

enum Selection {
    Case(String),
    Batch(usize),
    All,
}
fn selected_indices(method: &Value, selection: &Selection) -> Result<Vec<usize>> {
    let ids = ordered_ids(method);
    match selection {
        Selection::Case(id) => Ok(vec![
            ids.iter()
                .position(|v| v == id)
                .ok_or_else(|| anyhow!("Case is not in frozen36"))?,
        ]),
        Selection::Batch(n) => {
            ensure!((1..=6).contains(n), "Batch must be 1..6");
            Ok(((n - 1) * 6..n * 6).collect())
        }
        Selection::All => Ok((0..36).collect()),
    }
}

async fn run(root: &Path, prepared: &Path, manifest_sha: &str, selection: Selection) -> Result<()> {
    let manifest: Value =
        serde_json::from_slice(&checked(&prepared.join("manifest.json"), manifest_sha)?)?;
    ensure!(
        manifest["method_sha256"] == hash(METHOD.as_bytes())
            && manifest["compiled_probe_sha256"] == hash(SOURCE),
        "Prepared method/probe changed"
    );
    let method: Value = serde_json::from_slice(&read(&prepared.join("method.json"))?)?;
    ensure!(
        method == serde_json::from_str::<Value>(METHOD)?,
        "Prepared method changed"
    );
    checked(&root.join(POLICY), s(&manifest, "policy_sha256"))?;
    checked(&root.join(SCHEMA), s(&manifest, "schema_sha256"))?;
    checked(
        &root.join(SNAPSHOT),
        s(&manifest["inputs"], "snapshot_sha256"),
    )?;
    let rows: Vec<Value> = serde_json::from_slice(&checked(
        &prepared.join("requests.json"),
        s(&manifest, "requests_sha256"),
    )?)?;
    let reviews: Vec<Value> = serde_json::from_slice(&checked(
        &prepared.join("review-cases.json"),
        s(&manifest, "review_cases_sha256"),
    )?)?;
    let ids = ordered_ids(&method);
    ensure!(
        rows.len() == 36
            && reviews.len() == 36
            && rows.iter().zip(&ids).all(|(r, id)| s(r, "query_id") == id)
            && reviews
                .iter()
                .zip(&ids)
                .all(|(r, id)| s(r, "query_id") == id),
        "Prepared order changed"
    );
    let indices = selected_indices(&method, &selection)?;
    for &i in &indices {
        ensure!(
            !prepared.join(format!("attempt-{}.json", ids[i])).exists(),
            "Case already attempted: {}. No implicit retry.",
            ids[i]
        );
    }
    let provider = KimiProvider::new(root)?;
    let run_id = uid("run");
    let started = Instant::now();
    let mut outcomes = Vec::new();
    let operation: Result<()> = async {
        let audit = provider.probe().await?;
        ensure!(audit["available"]==true,"Existing verified tool-free provider is required");
        let identity = json!({"version":audit["version"],"binary_sha256":audit["binary_sha256"],"agent_sha256":audit["agent_sha256"],"tools":audit["tools"],"subagents":audit["subagents"]});
        write_new(&prepared.join(format!("{run_id}-start.json")),&json!({"provider":identity,"query_ids":indices.iter().map(|&i|&ids[i]).collect::<Vec<_>>(),"method_sha256":hash(METHOD.as_bytes()),"retries":0}))?;
        for &i in &indices {
            let qid = &ids[i];
            let request = &rows[i]["request"];
            // The marker remains even if the process dies before a reply. Never
            // mistake an ambiguous prior submission for permission to retry.
            write_new(&prepared.join(format!("attempt-{qid}.json")),&json!({"query_id":qid,"request_id":request["request_id"],"run_id":run_id,"request_sha256":hash(&serde_json::to_vec(request)?),"attempted_at":chrono::Utc::now().to_rfc3339(),"retries":0}))?;
            let call_started = Instant::now();
            let result = provider.generate(request).await;
            let mut record = json!({"query_id":qid,"run_id":run_id,"elapsed_ms":call_started.elapsed().as_secs_f64()*1000.0,
                "provider":identity,"reply_validated":false,"reply":null,"error":null,"semantic_review_status":"pending_review",
                "semantic_quality_pass":null,"delivered":false,"candidates_applied":false});
            match result {
                Ok(reply) => {
                    record["citation_checks"] = citation_check(&reply,request,&reviews[i]);
                    record["reply_validated"] = json!(true);
                    record["reply"] = reply;
                    write_new(&prepared.join(format!("result-{qid}.json")),&record)?;
                    outcomes.push(json!({"query_id":qid,"status":"validated_pending_semantic_review"}));
                }
                Err(error) => {
                    record["error"] = json!(error.to_string());
                    write_new(&prepared.join(format!("result-{qid}.json")),&record)?;
                    outcomes.push(json!({"query_id":qid,"status":"provider_or_protocol_failed"}));
                    return Err(error);
                }
            }
        }
        Ok(())
    }.await;
    let closed = provider.close().await;
    let status = provider.status();
    write_new(
        &prepared.join(format!("{run_id}-summary.json")),
        &json!({"requested_query_ids":indices.iter().map(|&i|&ids[i]).collect::<Vec<_>>(),
        "outcomes":outcomes,"operation_error":operation.as_ref().err().map(ToString::to_string),"provider_close_ok":closed.is_ok(),
        "provider_close_error":closed.as_ref().err().map(ToString::to_string),"provider_running_after_close":status["running"],
        "elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"semantic_quality_pass":null,"delivered":false,"candidates_applied":false,
        "cleanup_scope":"Existing provider close result and running count; no independently invented remote-cancel or per-process cleanup receipt"}),
    )?;
    operation?;
    closed?;
    println!("Synthetic replies retained for semantic review; no quality decision was made.");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut root = std::env::current_dir()?;
    let mut output = None;
    let mut prepared = None;
    let mut manifest_sha = None;
    let mut real = false;
    let mut selection = None;
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
                ))
            }
            "--prepared" => {
                ensure!(prepared.is_none(), "Duplicate preparation");
                prepared = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow!("Missing prepared path"))?,
                ))
            }
            "--prepared-manifest-sha256" => {
                ensure!(manifest_sha.is_none(), "Duplicate prepared manifest hash");
                manifest_sha = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("Missing prepared manifest hash"))?,
                );
            }
            "--real-synthetic" => {
                ensure!(!real, "Duplicate real flag");
                real = true
            }
            "--case" | "--batch" | "--all" => {
                ensure!(
                    selection.is_none(),
                    "Choose one case, batch or all selector"
                );
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
        ensure!(output.is_none(), "Real run uses an existing preparation");
        run(
            &root,
            &prepared.ok_or_else(|| anyhow!("--prepared required"))?,
            &manifest_sha.ok_or_else(|| anyhow!("--prepared-manifest-sha256 required"))?,
            selection.ok_or_else(|| anyhow!("Explicit case/batch/all required"))?,
        )
        .await
    } else {
        ensure!(
            prepared.is_none() && selection.is_none() && manifest_sha.is_none(),
            "--real-synthetic required for execution; no model called"
        );
        prepare(
            &root,
            &output.ok_or_else(|| anyhow!("--output NEW_DIR required"))?,
        )
    }
}

#[cfg(test)]
#[path = "tutor_evidence_tests.rs"]
mod tests;
