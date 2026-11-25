//! Comparison context is rebuilt from receipt-bound live sources. It never
//! promotes a background condition into a candidate's primary evidence.
use super::*;

const ROLES: &[(&str, &str, &str, &str)] = &[
    (
        "observation_id",
        "observation_version",
        "observation",
        "observation",
    ),
    ("untested_id", "untested_version", "untested", "plan"),
    ("pending_id", "pending_version", "pending_plan", "plan"),
];

impl Store {
    /// All-or-unavailable projection within source_events' 100-source bound.
    /// Current task fields may together exceed one memory's existing eight-
    /// source admission limit; that separate memory limit is unchanged.
    /// A failed comparison background must remain inspectable in memory detail,
    /// while retrieval callers can exclude it without hiding correction/delete.
    pub fn comparison_context_for_sources(&self, lid: &str, sources: &[Value]) -> Result<Value> {
        self.learner(lid)?;
        self.tx(|| match self.live_comparison_contexts(lid, sources) {
            Ok(items) => Ok(
                json!({"status":if items.is_empty() {"none"} else {"complete"},
                "items":items,"reason":null}),
            ),
            Err(error) => Ok(json!({"status":"unavailable","items":[],
                "reason":error.to_string().chars().take(240).collect::<String>()})),
        })
    }

    fn live_comparison_contexts(&self, lid: &str, sources: &[Value]) -> Result<Vec<Value>> {
        ensure!(
            sources.len() <= 100,
            "比较上下文的来源超过100条，未截断来源"
        );
        let ids: Vec<_> = sources
            .iter()
            .map(|source| s(source, "event_id").to_owned())
            .collect();
        let unique: HashSet<_> = ids.iter().collect();
        ensure!(
            ids.iter().all(|id| !id.is_empty()) && unique.len() == ids.len(),
            "比较上下文主来源编号无效或重复"
        );
        let canonical = self.source_events(lid, &ids)?;
        ensure!(
            canonical.len() == sources.len(),
            "比较上下文的主来源已失效或不属于此档案"
        );
        let mut seen = HashSet::new();
        let mut items = Vec::new();
        for provided in sources {
            let id = s(provided, "event_id");
            let source = canonical
                .iter()
                .find(|source| s(source, "event_id") == id)
                .ok_or_else(|| anyhow!("比较上下文主来源不存在"))?;
            ensure!(
                source["learner_id"] == lid
                    && source["event_status"] == "active"
                    && [
                        "event_version",
                        "session_id",
                        "activity_id",
                        "event_kind",
                        "source_type"
                    ]
                    .iter()
                    .all(|key| provided[*key] == source[*key])
                    && provided["learner_id"] == lid,
                "比较上下文主来源的归属或版本不一致"
            );
            // Find the authoritative reverse binding first. comparison_id in
            // a source is only a consistency marker, never authority to attach
            // another record (including another learner's experiment).
            let receipts = self.rows(
                "SELECT * FROM experiment_comparisons
                WHERE observation_id=? OR untested_id=? OR pending_id=? LIMIT 2",
                params![id, id, id],
            )?;
            if receipts.is_empty() {
                ensure!(
                    source["content"]["comparison_id"].is_null(),
                    "比较来源缺少有效回执，不能从旧文本恢复背景"
                );
                continue;
            }
            ensure!(receipts.len() == 1, "比较来源回执绑定不唯一");
            let receipt = &receipts[0];
            ensure!(
                receipt["learner_id"] == lid
                    && receipt["namespace"] == self.namespace
                    && receipt["session_id"] == source["session_id"],
                "比较回执归属不一致"
            );
            if seen.insert(s(receipt, "comparison_id").to_owned()) {
                items.push(self.memory_comparison_item(lid, receipt)?);
            }
        }
        Ok(items)
    }

    fn memory_comparison_item(&self, lid: &str, receipt: &Value) -> Result<Value> {
        let sid = s(receipt, "session_id");
        // Do not call session()/memory_evidence(): both may enter retrieval.
        let session = self
            .one(
                "SELECT learner_id,activity_id FROM sessions WHERE session_id=?",
                [sid],
            )?
            .ok_or_else(|| anyhow!("比较所属活动已失效"))?;
        let activity = s(&session, "activity_id");
        ensure!(
            session["learner_id"] == lid && !activity.is_empty(),
            "比较所属档案或活动不一致"
        );
        let mut item = self.project_comparison(receipt)?;
        ensure!(
            ["learner_report", "operator_confirmed", "simulated"]
                .contains(&s(&item, "source_type"))
                && if self.namespace == "simulator" {
                    item["source_type"] == "simulated"
                } else {
                    item["source_type"] != "simulated"
                },
            "比较来源等级与数据空间不一致"
        );
        let role_ids: Vec<_> = ROLES
            .iter()
            .filter_map(|(id, _, _, _)| receipt[*id].as_str().map(str::to_owned))
            .collect();
        let role_sources = self.source_events(lid, &role_ids)?;
        ensure!(
            role_sources.len() == role_ids.len(),
            "比较条件的完整来源已失效"
        );
        let mut links = Vec::new();
        for (id_column, version_column, role, kind) in ROLES {
            let Some(id) = receipt[*id_column].as_str() else {
                continue;
            };
            let source = role_sources
                .iter()
                .find(|source| s(source, "event_id") == id)
                .ok_or_else(|| anyhow!("比较角色来源缺失"))?;
            ensure!(
                source["learner_id"] == lid
                    && source["session_id"] == sid
                    && source["activity_id"] == activity
                    && source["event_status"] == "active"
                    && source["event_version"] == receipt[*version_column]
                    && source["event_kind"] == *kind
                    && source["source_type"] == item["source_type"],
                "比较角色来源的活动、类别或版本已失效"
            );
            links.push(
                json!({"role":role,"event_id":id,"event_version":source["event_version"],
                "event_kind":source["event_kind"],"source_type":source["source_type"]}),
            );
        }
        let trials = item["trials"]
            .as_array()
            .ok_or_else(|| anyhow!("比较试验结构无效"))?;
        let reported: Vec<_> = trials
            .iter()
            .filter(|trial| trial["status"] == "reported")
            .map(|trial| trial["label"].clone())
            .collect();
        let untested: Vec<_> = trials
            .iter()
            .filter(|trial| trial["status"] == "not_tested")
            .map(|trial| trial["label"].clone())
            .collect();
        let unknown_count: Vec<_> = trials
            .iter()
            .filter(|trial| trial["status"] == "reported" && trial["trial_count"].is_null())
            .map(|trial| trial["label"].clone())
            .collect();
        let unconfirmed: Vec<_> = item["conditions"]
            .as_array()
            .ok_or_else(|| anyhow!("比较条件结构无效"))?
            .iter()
            .filter(|condition| condition["status"] == "not_confirmed")
            .cloned()
            .collect();
        item["limits"] = json!({"comparison_incomplete":!untested.is_empty() || !unknown_count.is_empty() || !unconfirmed.is_empty(),
            "reported_labels":reported,"untested_labels":untested,"unknown_count_labels":unknown_count,
            "unconfirmed_conditions":unconfirmed,"causal_effect_established":false});
        item["learner_id"] = json!(lid);
        item["session_id"] = json!(sid);
        item["activity_id"] = json!(activity);
        item["namespace"] = json!(self.namespace);
        item["historical"] = json!(true);
        item["hardware_verified"] = json!(false);
        item["source_links"] = json!(links);
        Ok(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_embedding::EmbeddingProfile;

    struct Fixture {
        _directory: tempfile::TempDir,
        store: Store,
        lid: String,
        sid: String,
    }
    impl Fixture {
        fn new(namespace: &str) -> Result<Self> {
            let directory = tempfile::tempdir()?;
            let store = Store::open(
                directory.path().join("comparison-memory.sqlite3"),
                namespace,
            )?;
            let learner = store.create_learner("Synthetic comparison memory", "en", "guided")?;
            let lid = s(&learner, "learner_id").to_owned();
            let sid = s(&store.create_session(&lid)?, "session_id").to_owned();
            Ok(Self {
                _directory: directory,
                store,
                lid,
                sid,
            })
        }
        fn payload(&self, id: &str) -> Value {
            json!({"submission_id":id,"source_type":"learner_report","question":"What changed between A and B?",
                "changed_variable":"Distance from wall","conditions":[{"name":"same sensor","status":"reported_same"},
                    {"name":"same surface and angle","status":"not_confirmed"}],
                "trials":[{"label":"A","setting":"30 cm","status":"reported","result":"The display read 30 cm; the number of repetitions was not recorded.","trial_count":null},
                    {"label":"B","setting":"50 cm","status":"not_tested","result":null,"trial_count":null}],
                "pending_plan":{"text":"Maybe compare 50 cm next time; not agreed or performed.","accepted":false}})
        }
        fn comparison(&self, id: &str) -> Result<(Value, Vec<Value>)> {
            let saved = self
                .store
                .save_comparison(&self.lid, &self.sid, &self.payload(id))?;
            let cid = &saved["item"]["comparison_id"];
            let sources = self
                .store
                .recent_evidence(&self.sid)?
                .into_iter()
                .filter(|source| source["content"]["comparison_id"] == *cid)
                .collect();
            Ok((saved["item"].clone(), sources))
        }
        fn memory(&self, source: &Value) -> Result<String> {
            self.store
                .add_memory(
                    &self.lid,
                    &json!({"kind":"reported_progress","content":"A display report",
                "scope":"current_activity","source_event_ids":[source["event_id"]],
                "source_event_versions":{(s(source,"event_id")):source["event_version"]}}),
                    self.store.learner(&self.lid)?["epoch"].as_i64().unwrap(),
                )?
                .ok_or_else(|| anyhow!("fixture memory rejected"))
        }
    }

    #[test]
    fn projection_preserves_full_constraints_and_distinguishes_unknown_counts_from_not_tested()
    -> Result<()> {
        let f = Fixture::new("simulator")?;
        let (record, sources) = f.comparison("full")?;
        let context = f.store.comparison_context_for_sources(&f.lid, &sources)?;
        ensure!(context["status"] == "complete" && context["items"].as_array().unwrap().len() == 1);
        let item = &context["items"][0];
        for field in [
            "question",
            "changed_variable",
            "conditions",
            "trials",
            "pending_plan",
        ] {
            ensure!(item[field] == record[field], "{field}");
        }
        ensure!(item["limits"]["reported_labels"] == json!(["A"]));
        ensure!(item["limits"]["untested_labels"] == json!(["B"]));
        ensure!(item["limits"]["unknown_count_labels"] == json!(["A"]));
        ensure!(item["limits"]["unconfirmed_conditions"] == json!([record["conditions"][1]]));
        ensure!(
            item["limits"]["comparison_incomplete"] == true
                && item["limits"]["causal_effect_established"] == false
        );
        ensure!(item["pending_plan"]["accepted"] == false && item["trials"][1]["result"].is_null());
        ensure!(item["source_type"] == "simulated" && item["hardware_verified"] == false);
        ensure!(item["source_links"].as_array().unwrap().len() == 3);
        ensure!(
            item["source_links"][0]["role"] == "observation"
                && item["source_links"][1]["role"] == "untested"
                && item["source_links"][2]["role"] == "pending_plan"
        );
        let observation = sources
            .iter()
            .find(|row| row["event_kind"] == "observation")
            .unwrap();
        let mid = f.memory(observation)?;
        ensure!(f.store.memory_evidence(&f.lid, &mid)?["comparison_context"] == context);
        Ok(())
    }

    #[test]
    fn ordinary_sources_stay_none_and_claimed_or_cross_owner_comparisons_never_attach() -> Result<()>
    {
        let f = Fixture::new("real")?;
        let (record, sources) = f.comparison("owned")?;
        let ordinary = f.store.record_education_event(
            &f.lid,
            &f.sid,
            "learner_report",
            "observation",
            &json!({"text":"An ordinary report"}),
        )?;
        let mut forged_view = ordinary.clone();
        forged_view["content"]["comparison_id"] = record["comparison_id"].clone();
        ensure!(
            f.store
                .comparison_context_for_sources(&f.lid, &[forged_view])?["status"]
                == "none"
        );
        let forged_source = f.store.record_education_event(&f.lid,&f.sid,"learner_report","observation",
            &json!({"text":"A report with an unauthenticated marker","comparison_id":record["comparison_id"]}))?;
        let unavailable = f
            .store
            .comparison_context_for_sources(&f.lid, &[forged_source])?;
        ensure!(unavailable["status"] == "unavailable" && unavailable["items"] == json!([]));
        let other = f
            .store
            .create_learner("Other comparison owner", "en", "guided")?;
        ensure!(
            f.store
                .comparison_context_for_sources(s(&other, "learner_id"), &sources)?["status"]
                == "unavailable"
        );
        let ordinary_id = f.memory(&ordinary)?;
        ensure!(
            f.store
                .recall_for_session(&f.sid, "ordinary report", 5)?
                .iter()
                .any(|row| row["memory_id"] == ordinary_id)
        );
        for (field, value) in [
            ("event_version", json!(99)),
            ("session_id", json!("other-session")),
            ("activity_id", json!("other-activity")),
        ] {
            let mut changed = sources[0].clone();
            changed[field] = value;
            ensure!(
                f.store.comparison_context_for_sources(&f.lid, &[changed])?["status"]
                    == "unavailable",
                "{field}"
            );
        }
        let other_space = Fixture::new("simulator")?;
        ensure!(
            other_space
                .store
                .comparison_context_for_sources(&other_space.lid, &sources)?["status"]
                == "unavailable"
        );
        Ok(())
    }

    #[test]
    fn invalid_sibling_hides_recall_and_existing_dense_cache_but_detail_correction_remain_available()
    -> Result<()> {
        let f = Fixture::new("simulator")?;
        let (_, sources) = f.comparison("background-version")?;
        let observation = sources
            .iter()
            .find(|row| row["event_kind"] == "observation")
            .unwrap();
        let sibling = sources
            .iter()
            .find(|row| row["event_kind"] == "plan")
            .unwrap();
        let mid = f.memory(observation)?;
        let profile = EmbeddingProfile::E5NativeFp32;
        let model_key = profile.model_key();
        let snapshot = f.store.embedding_work(&f.lid, &model_key, 8)?.remove(0);
        let mut vector = vec![0.0; profile.dimension()];
        vector[0] = 1.0;
        ensure!(
            f.store
                .commit_memory_embedding(&snapshot, &model_key, &vector)?
        );
        f.store.db.execute(
            "UPDATE education_event_meta SET version=version+1 WHERE event_id=?",
            [s(sibling, "event_id")],
        )?;
        let detail = f.store.memory_evidence(&f.lid, &mid)?;
        ensure!(
            detail["comparison_context"]["status"] == "unavailable"
                && detail["comparison_context"]["items"] == json!([])
        );
        ensure!(
            f.store
                .memories(&f.lid)?
                .iter()
                .any(|row| row["memory_id"] == mid)
        );
        ensure!(
            f.store
                .recall_for_session(&f.sid, "display report", 5)?
                .is_empty()
        );
        ensure!(
            f.store
                .eligible_memory_embeddings(&f.lid, &model_key)?
                .is_empty()
        );
        ensure!(f.store.embedding_work(&f.lid, &model_key, 8)?.is_empty());
        ensure!(
            !f.store
                .commit_memory_embedding(&snapshot, &model_key, &vector)?
        );
        ensure!(
            f.store
                .recall_candidates_for_session_ranked(
                    &f.sid,
                    "display report",
                    QueryEmbedding {
                        model_key: &model_key,
                        values: &vector
                    },
                    5,
                    CandidateRanking::DenseOnly
                )?
                .is_empty()
        );
        let corrected = f.store.correct_memory(
            &f.lid,
            &mid,
            "Explicit current correction supplied by the user",
        )?;
        ensure!(f.store.memory_evidence(&f.lid, &corrected).is_ok());
        f.store.forget(&f.lid, Some(&corrected))?;
        ensure!(f.store.memories(&f.lid)?.is_empty());
        Ok(())
    }

    #[test]
    fn deleted_background_or_receipt_does_not_reappear_on_reopen_or_from_same_text() -> Result<()> {
        for delete_receipt in [false, true] {
            let f = Fixture::new("simulator")?;
            let (record, sources) = f.comparison("deleted-background")?;
            let observation = sources
                .iter()
                .find(|row| row["event_kind"] == "observation")
                .unwrap();
            let sibling = sources
                .iter()
                .find(|row| row["event_kind"] == "plan")
                .unwrap();
            let mid = f.memory(observation)?;
            if delete_receipt {
                f.store.db.execute(
                    "DELETE FROM experiment_comparisons WHERE comparison_id=?",
                    [s(&record, "comparison_id")],
                )?;
            } else {
                f.store.db.execute(
                    "DELETE FROM events WHERE event_id=?",
                    [s(sibling, "event_id")],
                )?;
            }
            ensure!(
                f.store.memory_evidence(&f.lid, &mid)?["comparison_context"]["status"]
                    == "unavailable"
            );
            // Re-recording identical words receives a new event identity and is
            // not authority to repair the old comparison's deleted role link.
            f.store.record_education_event(
                &f.lid,
                &f.sid,
                "simulated",
                "plan",
                &sibling["content"],
            )?;
            ensure!(
                f.store.memory_evidence(&f.lid, &mid)?["comparison_context"]["status"]
                    == "unavailable"
            );
            let path = f._directory.path().join("comparison-memory.sqlite3");
            drop(f.store);
            let reopened = Store::open(path, "simulator")?;
            ensure!(
                reopened.memory_evidence(&f.lid, &mid)?["comparison_context"]["status"]
                    == "unavailable"
            );
            ensure!(
                reopened
                    .recall_for_session(&f.sid, "display report", 5)?
                    .is_empty()
            );
        }
        Ok(())
    }

    #[test]
    fn eight_primary_sources_preserve_all_comparisons_and_conditions_or_return_nothing()
    -> Result<()> {
        let f = Fixture::new("simulator")?;
        let mut primary = Vec::new();
        let mut siblings = Vec::new();
        for i in 0..8 {
            let mut input = f.payload(&format!("complete-{i}"));
            input["conditions"] = json!((0..8).map(|j|json!({"name":format!("{i}-{j}-{}-complete-tail", "condition ".repeat(12)),"status":"not_confirmed"})).collect::<Vec<_>>());
            let saved = f.store.save_comparison(&f.lid, &f.sid, &input)?;
            let sources: Vec<_> = f
                .store
                .recent_evidence(&f.sid)?
                .into_iter()
                .filter(|source| {
                    source["content"]["comparison_id"] == saved["item"]["comparison_id"]
                })
                .collect();
            primary.push(
                sources
                    .iter()
                    .find(|row| row["event_kind"] == "observation")
                    .unwrap()
                    .clone(),
            );
            siblings.push(
                sources
                    .iter()
                    .find(|row| row["event_kind"] == "plan")
                    .unwrap()
                    .clone(),
            );
        }
        let context = f.store.comparison_context_for_sources(&f.lid, &primary)?;
        ensure!(context["status"] == "complete" && context["items"].as_array().unwrap().len() == 8);
        for item in context["items"].as_array().unwrap() {
            ensure!(
                item["conditions"].as_array().unwrap().len() == 8
                    && item["limits"]["unconfirmed_conditions"] == item["conditions"]
            );
            ensure!(
                item["conditions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|c| s(c, "name").ends_with("complete-tail"))
            );
        }
        // A current task can combine more than eight canonical sources. Its
        // shared comparison stays whole and is deduplicated by receipt.
        let mut combined = primary.clone();
        combined.push(siblings[0].clone());
        let combined_context = f.store.comparison_context_for_sources(&f.lid, &combined)?;
        ensure!(
            combined_context["status"] == "complete"
                && combined_context["items"].as_array().unwrap().len() == 8
        );
        let over_budget = vec![primary[0].clone(); 101];
        let too_large = f
            .store
            .comparison_context_for_sources(&f.lid, &over_budget)?;
        ensure!(too_large["status"] == "unavailable" && s(&too_large, "reason").contains("100"));
        f.store.db.execute(
            "UPDATE education_event_meta SET status='invalidated' WHERE event_id=?",
            [s(&siblings[7], "event_id")],
        )?;
        let invalid = f.store.comparison_context_for_sources(&f.lid, &primary)?;
        ensure!(invalid["status"] == "unavailable" && invalid["items"] == json!([]));
        Ok(())
    }
}
