//! Offline synthetic E5/lexical/RRF measurement. Does not change default recall,
//! load learner databases, download files, start devices, or call a remote model.
use ai_tutor::memory_embedding::{DIM, E5Embedder, Precision, cosine};
use ai_tutor::store::{QueryEmbedding, Store, s, uid};
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

const FIXTURE: &str = include_str!("../../tests/fixtures/e5_retrieval_probe_v1.json");

fn timing(samples: &[f64]) -> Value {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    if sorted.is_empty() {
        return Value::Null;
    }
    let at = |q: f64| {
        sorted[((sorted.len() as f64 * q).ceil() as usize)
            .saturating_sub(1)
            .min(sorted.len() - 1)]
    };
    json!({"samples":sorted.len(),"mean_ms":sorted.iter().sum::<f64>()/sorted.len() as f64,"p50_ms":at(0.5),"p95_ms":at(0.95),"max_ms":sorted[sorted.len()-1]})
}
fn fuse(lexical: &[String], dense: &[(String, f64)]) -> Vec<(String, f64)> {
    let mut scores: HashMap<String, f64> = HashMap::new();
    for (rank, id) in lexical.iter().enumerate() {
        *scores.entry(id.clone()).or_default() += 1.0 / (60.0 + rank as f64 + 1.0);
    }
    for (rank, (id, _)) in dense.iter().take(5).enumerate() {
        *scores.entry(id.clone()).or_default() += 1.0 / (60.0 + rank as f64 + 1.0);
    }
    let mut result: Vec<_> = scores.into_iter().collect();
    result.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    result
}
fn ranking_metrics(rank: &[String], relevant: &[String]) -> Value {
    if relevant.is_empty() {
        return json!({"positive_recall":"not_applicable","abstention":"not_implemented; inspect top semantic score","candidate_returned":!rank.is_empty()});
    }
    let rank_of_first = rank
        .iter()
        .position(|id| relevant.contains(id))
        .map(|index| index + 1);
    let found = relevant
        .iter()
        .filter(|id| rank.iter().take(3).any(|candidate| candidate == *id))
        .count();
    json!({"hit_at_1":rank.first().is_some_and(|id|relevant.contains(id)),"hit_at_3":found>0,"recall_at_3":found as f64/relevant.len() as f64,"all_required_at_3":found==relevant.len(),"reciprocal_rank_full_ranking":rank_of_first.map_or(0.0,|rank|1.0/rank as f64),"reciprocal_rank_at_5":rank_of_first.filter(|rank|*rank<=5).map_or(0.0,|rank|1.0/rank as f64)})
}

fn equal_score_groups(ranking: &[(String, f64)]) -> Vec<Value> {
    let mut result = vec![];
    let mut start = 0;
    while start < ranking.len() {
        let mut end = start + 1;
        while end < ranking.len() && ranking[end].1 == ranking[start].1 {
            end += 1;
        }
        if end - start > 1 {
            result.push(json!({"first_rank":start+1,"last_rank":end,"score":ranking[start].1,"ids":ranking[start..end].iter().map(|row|&row.0).collect::<Vec<_>>()}));
        }
        start = end;
    }
    result
}

struct TemporaryDatabase(PathBuf);
impl Drop for TemporaryDatabase {
    fn drop(&mut self) {
        for name in [
            "baseline.sqlite3",
            "baseline.sqlite3-wal",
            "baseline.sqlite3-shm",
        ] {
            let _ = fs::remove_file(self.0.join(name));
        }
        let _ = fs::remove_dir(&self.0);
    }
}

struct EmbeddedText {
    vector: Vec<f32>,
    tokens: usize,
}
struct Evaluation {
    report: Value,
    embeddings: HashMap<String, EmbeddedText>,
}
fn evaluate(
    root: &Path,
    threads: usize,
    repeats: usize,
    precision: Precision,
    fixture_text: &str,
) -> Result<Evaluation> {
    let fixture: Value = serde_json::from_str(fixture_text)?;
    ensure!(
        fixture["synthetic_only"] == true,
        "Only an explicit synthetic fixture is supported"
    );
    let mut encoder = E5Embedder::load(root, threads, precision)?;
    let mut embeddings = HashMap::new();
    let mut vectors = HashMap::new();
    let mut first = Value::Null;
    let mut doc_times = vec![];
    for document in fixture["documents"].as_array().unwrap() {
        let encoded = encoder.embed_document(s(document, "text"), None)?;
        if first.is_null() {
            first = json!({"tokens":encoded.tokens,"tokenize_ms":encoded.tokenize_ms,"inference_ms":encoded.inference_ms,"total_ms":encoded.total_ms});
        }
        doc_times.push(encoded.total_ms);
        embeddings.insert(
            format!("passage:{}", s(document, "id")),
            EmbeddedText {
                vector: encoded.vector.clone(),
                tokens: encoded.tokens,
            },
        );
        vectors.insert(s(document, "id").to_owned(), encoded.vector);
    }
    let path = std::env::temp_dir().join(uid("ai-tutor-e5-baseline"));
    fs::create_dir(&path)?;
    let _cleanup = TemporaryDatabase(path.clone());
    let store = Store::open(path.join("baseline.sqlite3"), "simulator")?;
    let mut reports = vec![];
    let mut total_times = vec![];
    let mut inference_times = vec![];
    let mut tokenize_times = vec![];
    for case in fixture["cases"].as_array().unwrap() {
        let learner = store.create_learner("SYNTHETIC_E5_CASE", "en", "guided")?;
        let lid = s(&learner, "learner_id");
        let session = store.create_session(lid)?;
        let sid = s(&session, "session_id");
        let ids = ai_tutor::store::strings(if case["candidates"].is_array() {
            &case["candidates"]
        } else {
            &fixture["candidate_pool"]
        });
        ensure!(
            !ids.is_empty() && ids.len() <= 32,
            "Fixture candidate pool must contain 1 to 32 documents"
        );
        let mut memory_ids = HashMap::new();
        for id in &ids {
            let document = fixture["documents"]
                .as_array()
                .unwrap()
                .iter()
                .find(|document| document["id"] == *id)
                .ok_or_else(|| anyhow!("Fixture document missing"))?;
            let event_kind = document["event_kind"].as_str().unwrap_or("observation");
            let kind = document["memory_kind"]
                .as_str()
                .unwrap_or("reported_progress");
            let mut event_content = json!({"text":document["text"],"synthetic_fixture":true});
            if event_kind == "plan" {
                event_content["accepted"] = json!(true);
            }
            let event =
                store.record_education_event(lid, sid, "simulated", event_kind, &event_content)?;
            let memory=store.add_memory(lid,&json!({"kind":kind,"scope":"current_activity","content":document["text"],"source_event_ids":[event["event_id"]],"source_event_versions":{s(&event,"event_id"):event["event_version"]}}),0)?.ok_or_else(||anyhow!("Fixture memory rejected"))?;
            memory_ids.insert(memory, id.clone());
        }
        for snapshot in store.embedding_work(lid, encoder.model_key(), 32)? {
            ensure!(
                store.commit_memory_embedding(
                    &snapshot,
                    encoder.model_key(),
                    &vectors[&memory_ids[&snapshot.memory_id]]
                )?,
                "Synthetic cache commit was rejected"
            );
        }
        ensure!(
            store
                .eligible_memory_embeddings(lid, encoder.model_key())?
                .len()
                == ids.len(),
            "Synthetic cache is incomplete"
        );
        let lexical: Vec<_> = store
            .recall(lid, s(case, "query"), 5)?
            .iter()
            .map(|memory| memory_ids[s(memory, "memory_id")].clone())
            .collect();
        let mut query = None;
        let mut case_times = vec![];
        for _ in 0..repeats {
            let encoded = encoder.embed_query(s(case, "query"), None)?;
            case_times.push(encoded.total_ms);
            total_times.push(encoded.total_ms);
            inference_times.push(encoded.inference_ms);
            tokenize_times.push(encoded.tokenize_ms);
            query = Some(encoded);
        }
        let query = query.unwrap();
        let mut dense: Vec<_> = ids
            .iter()
            .map(|id| (id.clone(), cosine(&query.vector, &vectors[id])))
            .collect();
        dense.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let hybrid = fuse(&lexical, &dense);
        let relevant = ai_tutor::store::strings(&case["relevant"]);
        let dense_ids: Vec<_> = dense.iter().map(|(id, _)| id.clone()).collect();
        let hybrid_ids: Vec<_> = hybrid.iter().map(|(id, _)| id.clone()).collect();
        let store_started = std::time::Instant::now();
        let stored = store.recall_candidates(
            lid,
            s(case, "query"),
            Some(QueryEmbedding {
                model_key: encoder.model_key(),
                values: &query.vector,
            }),
            5,
        )?;
        let store_recall_ms = store_started.elapsed().as_secs_f64() * 1000.0;
        ensure!(
            stored
                .iter()
                .all(|candidate| candidate.memory["source_type"] == "simulated"),
            "Synthetic recall returned an unexpected evidence grade"
        );
        let stored_ids: Vec<_> = stored
            .iter()
            .map(|candidate| memory_ids[s(&candidate.memory, "memory_id")].clone())
            .collect();
        let stored_ranking: Vec<_> = stored.iter().map(|candidate|json!({"id":memory_ids[s(&candidate.memory,"memory_id")],"kind":candidate.memory["kind"],"content":candidate.memory["content"],"source_type":candidate.memory["source_type"],"lexical_rank":candidate.lexical_rank,"semantic_rank":candidate.semantic_rank,"cosine":candidate.cosine,"ranking_score":candidate.ranking_score,"relevance_status":candidate.relevance_status})).collect();
        let stored_scores: Vec<_> = stored
            .iter()
            .map(|candidate| {
                (
                    memory_ids[s(&candidate.memory, "memory_id")].clone(),
                    candidate.ranking_score,
                )
            })
            .collect();
        embeddings.insert(
            format!("query:{}", s(case, "id")),
            EmbeddedText {
                vector: query.vector.clone(),
                tokens: query.tokens,
            },
        );
        reports.push(json!({"id":case["id"],"group":case["group"],"query":case["query"],"relevant":relevant,"query_tokens":query.tokens,
            "lexical":{"ranked_ids":lexical,"metrics":ranking_metrics(&lexical,&relevant)},
            "dense":{"ranking":dense.iter().map(|(id,score)|json!({"id":id,"cosine":score})).collect::<Vec<_>>(),"metrics":ranking_metrics(&dense_ids,&relevant)},
            "hybrid_rrf":{"ranking":hybrid.iter().map(|(id,score)|json!({"id":id,"score":score})).collect::<Vec<_>>(),"metrics":ranking_metrics(&hybrid_ids,&relevant),"tie_break":"fixture_id","equal_score_groups":equal_score_groups(&hybrid)},
            "store_hybrid":{"ranking":stored_ranking,"metrics":ranking_metrics(&stored_ids,&relevant),"recall_with_evidence_checks_ms":store_recall_ms,"tie_break":"random_store_memory_id; tied positions are not reproducible across databases","equal_score_groups_within_returned_window":equal_score_groups(&stored_scores),"offline_top5_order_matches_store":hybrid_ids.iter().take(5).eq(stored_ids.iter()),"returned_scores_match_offline":stored_scores.iter().all(|(id,score)|hybrid.iter().any(|(other,offline)|id==other && score==offline))},
            "warm_query_latency":timing(&case_times),"semantic_acceptance_threshold":"not_calibrated"}));
    }
    let mut aggregate = json!({});
    for method in ["lexical", "dense", "hybrid_rrf", "store_hybrid"] {
        let positives: Vec<_> = reports
            .iter()
            .filter(|row| !row["relevant"].as_array().unwrap().is_empty())
            .collect();
        aggregate[method] = json!({"positive_cases":positives.len(),"hit_at_1":positives.iter().filter(|row|row[method]["metrics"]["hit_at_1"]==true).count(),"hit_at_3":positives.iter().filter(|row|row[method]["metrics"]["hit_at_3"]==true).count(),"mrr_at_5":positives.iter().map(|row|row[method]["metrics"]["reciprocal_rank_at_5"].as_f64().unwrap()).sum::<f64>()/positives.len() as f64,"mrr_full_ranking_not_comparable_across_window_sizes":positives.iter().map(|row|row[method]["metrics"]["reciprocal_rank_full_ranking"].as_f64().unwrap()).sum::<f64>()/positives.len() as f64,"all_required_at_3":positives.iter().filter(|row|row[method]["metrics"]["all_required_at_3"]==true).count()});
        let negatives: Vec<_> = reports
            .iter()
            .filter(|row| row["relevant"].as_array().unwrap().is_empty())
            .collect();
        aggregate[method]["negative_cases"] = json!(negatives.len());
        aggregate[method]["negatives_with_candidates"] = json!(
            negatives
                .iter()
                .filter(|row| row[method]["metrics"]["candidate_returned"] == true)
                .count()
        );
    }
    Ok(Evaluation {
        report: json!({"probe":"offline_e5_lexical_hybrid","fixture_version":fixture["fixture_version"],"fixture_sha256":format!("{:x}",Sha256::digest(fixture_text.as_bytes())),"synthetic_only":true,"network_used":false,"production_recall_changed":false,"store_cache_and_live_evidence_used":true,"warm_query_timing_scope":"embedding_only; Store recall timing is recorded separately per case","group_label_amendment":if fixture["fixture_version"]=="e5-education-recall-v2-preregistered-20261104" {json!({"q01":"English query and English relevant document: monolingual_control; original frozen group label retained"})}else{Value::Null},
        "model":encoder.metadata(),"first_document_embedding":first,"all_document_latency":timing(&doc_times),"warm_query_total":timing(&total_times),"warm_query_inference":timing(&inference_times),"warm_query_tokenize":timing(&tokenize_times),
        "repeats_per_query":repeats,"aggregate":aggregate,"cases":reports,"notes":fixture["evaluation_notes"],"default_enable_recommendation":"not_decided_by_this_small_probe"}),
        embeddings,
    })
}

fn value_distribution(values: &[f64]) -> Value {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return Value::Null;
    }
    let percentile = |p: f64| {
        values[((values.len() as f64 * p).ceil() as usize)
            .saturating_sub(1)
            .min(values.len() - 1)]
    };
    json!({"count":values.len(),"min":values[0],"p50":percentile(0.5),"p95":percentile(0.95),"max":values[values.len()-1],"mean":values.iter().sum::<f64>()/values.len() as f64})
}
fn vector_difference(int8: &[f32], fp32: &[f32]) -> Result<Value> {
    ensure!(
        int8.len() == DIM && fp32.len() == DIM,
        "Parity requires matching 384-dimensional vectors"
    );
    let errors: Vec<_> = int8
        .iter()
        .zip(fp32)
        .map(|(a, b)| (*a as f64 - *b as f64).abs())
        .collect();
    Ok(
        json!({"cosine":cosine(int8,fp32),"max_abs_component_error":errors.iter().copied().fold(0.0,f64::max),
        "component_rmse":(errors.iter().map(|error|error*error).sum::<f64>()/DIM as f64).sqrt()}),
    )
}
fn compare(int8: Evaluation, fp32: Evaluation) -> Result<Value> {
    for key in [
        "model",
        "revision",
        "tokenizer_sha256",
        "runtime_dll_sha256",
        "pooling",
        "query_prefix",
        "document_prefix",
        "dimension",
        "intra_threads",
        "inter_threads",
    ] {
        ensure!(
            int8.report["model"][key] == fp32.report["model"][key],
            "Precision comparison pipeline mismatch: {key}"
        );
    }
    ensure!(
        int8.report["fixture_version"] == fp32.report["fixture_version"]
            && int8.embeddings.len() == fp32.embeddings.len(),
        "Precision comparison inputs differ"
    );
    let mut item_ids: Vec<_> = int8.embeddings.keys().collect();
    item_ids.sort();
    let mut items = vec![];
    let mut cosines = vec![];
    let mut component_rmse = vec![];
    for id in item_ids {
        let left = &int8.embeddings[id];
        let right = fp32
            .embeddings
            .get(id)
            .ok_or_else(|| anyhow!("Missing FP32 item"))?;
        ensure!(
            left.tokens == right.tokens,
            "Precision comparison token lengths differ"
        );
        let mut delta = vector_difference(&left.vector, &right.vector)?;
        cosines.push(delta["cosine"].as_f64().unwrap());
        component_rmse.push(delta["component_rmse"].as_f64().unwrap());
        delta["id"] = json!(id);
        delta["tokens"] = json!(left.tokens);
        items.push(delta);
    }
    let mut rankings = vec![];
    let mut pair_deltas = vec![];
    for left in int8.report["cases"].as_array().unwrap() {
        let right = fp32.report["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == left["id"])
            .ok_or_else(|| anyhow!("Missing FP32 query"))?;
        let left_rank = left["dense"]["ranking"].as_array().unwrap();
        let right_rank = right["dense"]["ranking"].as_array().unwrap();
        let ids = |rows: &[Value]| rows.iter().map(|row| row["id"].clone()).collect::<Vec<_>>();
        let mut pairs = vec![];
        for row in left_rank {
            let other = right_rank
                .iter()
                .find(|other| other["id"] == row["id"])
                .ok_or_else(|| anyhow!("Missing FP32 candidate"))?;
            let delta = row["cosine"].as_f64().unwrap() - other["cosine"].as_f64().unwrap();
            pair_deltas.push(delta.abs());
            pairs.push(json!({"id":row["id"],"int8_cosine":row["cosine"],"fp32_cosine":other["cosine"],"int8_minus_fp32":delta}));
        }
        let left_ids = ids(left_rank);
        let right_ids = ids(right_rank);
        rankings.push(json!({"id":left["id"],"group":left["group"],"relevant":left["relevant"],
            "int8_ranked_ids":left_ids,"fp32_ranked_ids":right_ids,"order_changed":left_ids != right_ids,
            "top1_changed":left_ids.first() != right_ids.first(),"pairs":pairs,
            "int8_metrics":left["dense"]["metrics"],"fp32_metrics":right["dense"]["metrics"]}));
    }
    Ok(
        json!({"probe":"offline_e5_precision_comparison","synthetic_only":true,"network_used":false,"production_recall_changed":false,
        "execution_order":["int8","fp32"],"sessions_loaded_sequentially":true,"matching_revision_tokenizer_pooling":true,
        "embedding_cosine_distribution":value_distribution(&cosines),"embedding_rmse_distribution":value_distribution(&component_rmse),
        "retrieval_pair_abs_cosine_delta_distribution":value_distribution(&pair_deltas),"per_item":items,"rank_comparison":rankings,
        "top1_changed_queries":rankings.iter().filter(|row|row["top1_changed"]==true).count(),
        "int8":int8.report,"fp32":fp32.report,
        "interpretation":"A paired small-fixture diagnostic, not a general quantization-equivalence or retrieval-quality pass. No acceptance threshold is tuned or enabled. FP32 loads second into a warm process; cold-load timing is not a randomized comparison."}),
    )
}

fn main() -> Result<()> {
    let mut root = std::env::current_dir()?;
    let mut threads = 2usize;
    let mut repeats = 5usize;
    let mut precision = "int8".to_owned();
    let mut fixture_text = FIXTURE.to_owned();
    let mut explicit = false;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--help" | "-h" => {
                println!(
                    "e5-embedding-probe --local-synthetic [--root PATH] [--threads 1..2] [--repeats 1..10] [--precision int8|fp32|compare] [--fixture JSON]\nOffline fixed-hash E5 CPU benchmark and lexical/RRF retrieval fixture, including actual Store vector commits and evidence-checked candidate retrieval. Does not download files or enable production retrieval."
                );
                return Ok(());
            }
            "--local-synthetic" => explicit = true,
            "--root" => root = PathBuf::from(args.next().ok_or_else(|| anyhow!("Missing root"))?),
            "--precision" => precision = args.next().ok_or_else(|| anyhow!("Missing precision"))?,
            "--fixture" => {
                fixture_text =
                    fs::read_to_string(args.next().ok_or_else(|| anyhow!("Missing fixture path"))?)?
            }
            "--threads" => {
                threads = args
                    .next()
                    .ok_or_else(|| anyhow!("Missing thread count"))?
                    .parse()?
            }
            "--repeats" => {
                repeats = args
                    .next()
                    .ok_or_else(|| anyhow!("Missing repeat count"))?
                    .parse()?
            }
            _ => bail!("Unknown argument"),
        }
    }
    ensure!(
        explicit && (1..=2).contains(&threads) && (1..=10).contains(&repeats),
        "Explicit local synthetic mode and bounded thread/repeat counts are required"
    );
    let report = match precision.as_str() {
        "int8" => evaluate(&root, threads, repeats, Precision::Int8, &fixture_text)?.report,
        "fp32" => evaluate(&root, threads, repeats, Precision::Fp32, &fixture_text)?.report,
        "compare" => {
            let int8 = evaluate(&root, threads, repeats, Precision::Int8, &fixture_text)?;
            let fp32 = evaluate(&root, threads, repeats, Precision::Fp32, &fixture_text)?;
            compare(int8, fp32)?
        }
        _ => bail!("Precision must be int8, fp32 or compare"),
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_tutor::memory_embedding::TOKENIZER_SHA;
    #[test]
    fn multi_fact_queries_and_absent_evidence_have_separate_metrics() {
        let rank = vec!["first".into(), "unrelated".into(), "other".into()];
        let partial = ranking_metrics(&rank, &["first".into(), "second".into()]);
        assert_eq!(partial["hit_at_3"], true);
        assert_eq!(partial["recall_at_3"], 0.5);
        assert_eq!(partial["all_required_at_3"], false);
        assert_eq!(ranking_metrics(&rank, &[])["candidate_returned"], true);
        assert_eq!(ranking_metrics(&[], &[])["candidate_returned"], false);
        let long_rank = vec![
            "a".into(),
            "b".into(),
            "c".into(),
            "d".into(),
            "e".into(),
            "f".into(),
        ];
        let beyond = ranking_metrics(&long_rank, &["f".into()]);
        assert_eq!(beyond["reciprocal_rank_at_5"], 0.0);
        assert!(beyond["reciprocal_rank_full_ranking"].as_f64().unwrap() > 0.0);
    }
    #[test]
    fn precision_metrics_compare_the_same_embedding_coordinates() -> Result<()> {
        let mut first = vec![0.0; DIM];
        let mut other = vec![0.0; DIM];
        first[0] = 1.0;
        other[1] = 1.0;
        let identical = vector_difference(&first, &first)?;
        ensure!(identical["cosine"] == 1.0 && identical["component_rmse"] == 0.0);
        let orthogonal = vector_difference(&first, &other)?;
        ensure!(orthogonal["cosine"] == 0.0 && orthogonal["max_abs_component_error"] == 1.0);
        ensure!(
            (orthogonal["component_rmse"].as_f64().unwrap() - (2.0 / DIM as f64).sqrt()).abs()
                < 1e-12
        );
        ensure!(vector_difference(&first[..DIM - 1], &other).is_err());
        Ok(())
    }
    #[test]
    fn precision_comparison_rejects_different_tokenizer_provenance() {
        let evaluation = |hash: &str| Evaluation {
            report: json!({"model":{"tokenizer_sha256":hash},"fixture_version":"same","cases":[]}),
            embeddings: HashMap::new(),
        };
        assert!(
            compare(
                evaluation(TOKENIZER_SHA),
                evaluation("different-tokenizer-sha")
            )
            .is_err()
        );
    }
    #[test]
    fn rank_fusion_keeps_semantic_only_candidate_without_inventing_lexical_overlap() {
        let dense = vec![("cross-language".into(), 0.91), ("distractor".into(), 0.82)];
        assert_eq!(fuse(&[], &dense)[0].0, "cross-language");
        assert_eq!(fuse(&["distractor".into()], &dense)[0].0, "distractor");
        assert_eq!(
            ranking_metrics(&["cross-language".into()], &[])["abstention"],
            "not_implemented; inspect top semantic score"
        );
    }
    #[test]
    fn original_cross_language_questions_and_negative_controls_are_preserved() -> Result<()> {
        let fixture: Value = serde_json::from_str(FIXTURE)?;
        let cases = fixture["cases"].as_array().unwrap();
        ensure!(cases.iter().any(|case| case["id"] == "original_en_to_zh"
            && case["query"] == "What happened on the second attempt?"));
        ensure!(
            cases.iter().any(|case| case["id"] == "original_zh_to_en"
                && case["query"] == "第二次尝试有什么变化？")
        );
        ensure!(
            cases
                .iter()
                .filter(|case| case["relevant"] == json!([]))
                .count()
                == 2
        );
        Ok(())
    }
}
