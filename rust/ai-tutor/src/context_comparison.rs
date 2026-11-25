//! Share each complete recorded comparison once across current facts and recalled
//! summaries. The caller admits or omits the whole enclosing record by budget.
use super::{append_evidence, fetch, source};
use crate::store::{Store, s};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::collections::HashMap;

pub(super) fn policy(context: &Value) -> Option<&'static str> {
    (context["comparison_contexts"]
        .as_array()
        .is_some_and(|items| !items.is_empty())
        || context["task"]["comparison_context_status"] == "unavailable"
        || context["continuation"]["comparison_context_status"] == "unavailable")
        .then_some(include_str!("../../../prompts/comparison_context.md"))
}

pub(super) fn attach(
    store: &Store,
    lid: &str,
    sid: &str,
    context: &mut Value,
    projection: &Value,
    session_depths: &HashMap<String, usize>,
) -> Result<Value> {
    let status = s(projection, "status");
    ensure!(
        matches!(status, "none" | "complete" | "unavailable"),
        "比较上下文状态无效"
    );
    let mut ids = Vec::new();
    if status == "complete" {
        for item in projection["items"]
            .as_array()
            .ok_or_else(|| anyhow!("比较上下文缺失"))?
        {
            ensure!(
                item["learner_id"] == lid && item["namespace"] == store.namespace,
                "比较上下文不属于当前档案"
            );
            let comparison_id = s(item, "comparison_id");
            ensure!(!comparison_id.is_empty(), "比较记录标识缺失");
            let links = item["source_links"]
                .as_array()
                .ok_or_else(|| anyhow!("比较来源缺失"))?;
            let source_ids: Vec<_> = links
                .iter()
                .map(|link| s(link, "event_id").to_owned())
                .collect();
            let rows = fetch(store, lid, &source_ids)?;
            ensure!(rows.len() == source_ids.len(), "比较来源已变化");
            for link in links {
                let row = &rows[s(link, "event_id")];
                ensure!(
                    row["event_version"] == link["event_version"]
                        && row["event_kind"] == link["event_kind"]
                        && row["source_type"] == link["source_type"]
                        && row["session_id"] == item["session_id"],
                    "比较来源版本已变化"
                );
                append_evidence(
                    context,
                    source(
                        row,
                        Some(json!({
                            "path":"comparison_contexts","comparison_id":comparison_id,"role":link["role"],
                            "verbatim":false,"meaning":"recorded field values and derived record-status flags; not current hardware"
                        })),
                        s(row, "session_id") != sid,
                        session_depths,
                    ),
                );
                // A source can already support a current task field or a memory.
                // Add the comparison role without erasing that original binding.
                let evidence = context["evidence"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|entry| entry["event_id"] == link["event_id"])
                    .unwrap();
                ensure!(
                    evidence["event_version"] == link["event_version"],
                    "上下文来源版本不一致"
                );
                if !evidence["comparison_references"].is_array() {
                    evidence["comparison_references"] = json!([]);
                }
                let reference = json!({"comparison_id":comparison_id,"role":link["role"]});
                let references = evidence["comparison_references"].as_array_mut().unwrap();
                if !references.contains(&reference) {
                    references.push(reference);
                }
            }
            if !context["comparison_contexts"].is_array() {
                context["comparison_contexts"] = json!([]);
            }
            let pool = context["comparison_contexts"].as_array_mut().unwrap();
            if let Some(existing) = pool
                .iter()
                .find(|entry| entry["comparison_id"] == comparison_id)
            {
                ensure!(existing == item, "比较上下文版本不一致");
            } else {
                pool.push(item.clone());
            }
            if !ids.contains(&comparison_id.to_owned()) {
                ids.push(comparison_id.to_owned());
            }
        }
    }
    Ok(json!({"comparison_context_status":status,"comparison_context_ids":ids}))
}

pub(super) fn bind(target: &mut Value, binding: &Value) {
    if binding["comparison_context_status"] != "none" {
        target["comparison_context_status"] = binding["comparison_context_status"].clone();
        target["comparison_context_ids"] = binding["comparison_context_ids"].clone();
    }
}
