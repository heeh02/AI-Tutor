//! Standalone source draft. Not declared by Cargo during the active build.
//! Executes the frozen production-independent lexical module, not a Python port.
#[path = "../../rust/ai-tutor/src/memory_lexical.rs"]
mod memory_lexical;

use anyhow::{ensure, Result};
use memory_lexical::{rank, Document};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, io::Write, path::PathBuf};

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(args.len() == 2, "usage: lexical-challenge-adapter INPUT.json OUTPUT.json");
    let input = PathBuf::from(&args[0]);
    let output = PathBuf::from(&args[1]);
    ensure!(!output.exists(), "Preserve earlier report");
    let bytes = fs::read(input)?;
    ensure!(bytes.len() <= 2 * 1024 * 1024, "Input exceeds fixed probe budget");
    let request: Value = serde_json::from_slice(&bytes)?;
    ensure!(request["schema_version"] == "lexical-challenge-request-v1", "Wrong request schema");
    let sources = request["sources"].as_array().ok_or_else(|| anyhow::anyhow!("Missing sources"))?;
    let queries = request["queries"].as_array().ok_or_else(|| anyhow::anyhow!("Missing queries"))?;
    ensure!(sources.len() == 60 && queries.len() == 96, "Full challenge is required");
    let documents: Vec<_> = sources.iter().map(|source| {
        Ok(Document {
            id: source["id"].as_str().ok_or_else(|| anyhow::anyhow!("Missing id"))?,
            text: source["text"].as_str().ok_or_else(|| anyhow::anyhow!("Missing text"))?,
        })
    }).collect::<Result<_>>()?;
    let all_ids: BTreeSet<_> = documents.iter().map(|source| source.id).collect();
    ensure!(all_ids.len() == 60, "Duplicate source id");
    let mut cases = Vec::new();
    for query in queries {
        let query_text = query["text"].as_str().ok_or_else(|| anyhow::anyhow!("Missing text"))?;
        let goal = query["goal"].as_str().ok_or_else(|| anyhow::anyhow!("Missing goal"))?;
        let pool: BTreeSet<_> = query["candidate_pool_ids"].as_array().ok_or_else(|| anyhow::anyhow!("Missing pool"))?
            .iter().map(|id| id.as_str().ok_or_else(|| anyhow::anyhow!("Invalid pool id"))).collect::<Result<_>>()?;
        ensure!(pool == all_ids, "No topic-dependent prefilter is allowed");
        let combined = format!("{query_text} {goal} ");
        let current_features = memory_lexical::features(query_text);
        for (condition, text) in [("current_query", query_text), ("query_plus_goal", combined.as_str())] {
            let ranking = rank(text, &documents);
            let candidates: Vec<_> = ranking.candidates.iter().map(|candidate| {
                json!({"id":candidate.id,"score":candidate.score,"matched":candidate.matched,
                    "document_feature_count":candidate.document_feature_count,
                    "relevance_status":candidate.relevance_status,
                    "goal_only_match":condition == "query_plus_goal" &&
                        !candidate.matched.iter().any(|item| current_features.contains(&item.feature))})
            }).collect();
            cases.push(json!({"query_id":query["id"],"condition":condition,
                "query_features":ranking.query_features,"ranking":candidates,
                "eligible_document_count":ranking.eligible_document_count,
                "no_overlap_document_count":ranking.no_overlap_document_count}));
        }
    }
    let result = json!({"schema_version":"lexical-challenge-result-v1",
        "request_sha256":sha(&bytes),"fixture_manifest_sha256":request["fixture_manifest_sha256"],
        "method_version":memory_lexical::METHOD_VERSION,
        "function_word_count":memory_lexical::FUNCTION_WORDS.len(),
        "module_sha256":sha(include_bytes!("../../rust/ai-tutor/src/memory_lexical.rs")),
        "method_sha256":sha(include_bytes!("../../rust/ai-tutor/tests/fixtures/lexical_shadow_method_v1.json")),
        "adapter_sha256":sha(include_bytes!("lexical_challenge_adapter.rs")),
        "cases":cases,"model_executed":false,"learner_db_opened":false,
        "scope_gate":"caller supplies complete eligible synthetic source pool; separate controls are not scored here"});
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(output)?;
    serde_json::to_writer_pretty(&mut file, &result)?;
    file.write_all(b"\n")?;
    println!("{{\"status\":\"completed\",\"cases\":192,\"model_executed\":false}}");
    Ok(())
}
