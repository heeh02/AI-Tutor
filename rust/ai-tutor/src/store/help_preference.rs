//! An explicit UI choice, not inference from ordinary dialogue. Receipts retain
//! identities and a request digest, never a second copy of preference text.
use super::*;
use sha2::{Digest, Sha256};

struct Request<'a> {
    content: &'a str,
    id: &'a str,
    epoch: i64,
    generation: i64,
    digest: String,
}

fn request(data: &Value) -> Result<Request<'_>> {
    let object = data
        .as_object()
        .ok_or_else(|| anyhow!("讲解偏好必须为对象"))?;
    ensure!(
        object.len() == 4
            && object.keys().all(|key| [
                "content",
                "request_id",
                "expected_epoch",
                "expected_generation"
            ]
            .contains(&key.as_str())),
        "讲解偏好字段不符合协议"
    );
    let content = data["content"]
        .as_str()
        .ok_or_else(|| anyhow!("讲解偏好必须为原文"))?;
    let id = data["request_id"]
        .as_str()
        .ok_or_else(|| anyhow!("缺少讲解偏好请求编号"))?;
    ensure!(
        !content.trim().is_empty() && content.chars().count() <= 800,
        "讲解偏好长度必须为1至800字"
    );
    ensure!(
        !id.trim().is_empty() && id.chars().count() <= 96,
        "讲解偏好请求编号长度无效"
    );
    let epoch = data["expected_epoch"]
        .as_i64()
        .filter(|n| *n >= 0)
        .ok_or_else(|| anyhow!("讲解偏好档案版本无效"))?;
    let generation = data["expected_generation"]
        .as_i64()
        .filter(|n| *n >= 0)
        .ok_or_else(|| anyhow!("讲解偏好会话版本无效"))?;
    let digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&json!([content, epoch, generation]))?)
    );
    Ok(Request {
        content,
        id,
        epoch,
        generation,
        digest,
    })
}

impl Store {
    pub(super) fn migrate_help_preference_receipts(&self) -> Result<()> {
        self.db.execute_batch(
            "CREATE TABLE IF NOT EXISTS help_preference_receipts(
            namespace TEXT NOT NULL,learner_id TEXT NOT NULL,request_id TEXT NOT NULL,
            session_id TEXT NOT NULL,request_digest TEXT NOT NULL,
            expected_epoch INTEGER NOT NULL,expected_generation INTEGER NOT NULL,
            learner_epoch INTEGER NOT NULL,memory_id TEXT NOT NULL,event_id TEXT NOT NULL,
            event_version INTEGER NOT NULL,created_at TEXT NOT NULL,
            PRIMARY KEY(namespace,learner_id,request_id)
        );",
        )?;
        let wrong: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM help_preference_receipts WHERE namespace!=?",
            [&self.namespace],
            |row| row.get(0),
        )?;
        ensure!(wrong == 0, "讲解偏好回执空间不一致");
        Ok(())
    }

    /// Replacement revokes the old sources, not just their summary. Keeping the
    /// immutable event text preserves history, while source_events(), discovery,
    /// and old foreground/model candidates can no longer reuse that authority.
    fn revoke_previous_help_preferences(&self, lid: &str) -> Result<()> {
        let mut ids = HashSet::new();
        let mut revoked_memories = HashSet::new();
        for row in self.rows("SELECT source_event_ids FROM memories WHERE learner_id=? AND kind='help_preference' AND scope='cross_session_explicit'", [lid])? {
            ids.extend(strings(&serde_json::from_str::<Value>(s(&row, "source_event_ids"))?));
        }
        for id in &ids {
            for row in self.rows("SELECT memory_id FROM memory_source_links WHERE learner_id=? AND namespace=? AND event_id=?",
                params![lid,self.namespace,id])? {
                revoked_memories.insert(s(&row,"memory_id").to_owned());
            }
            self.db.execute(
                "UPDATE education_event_meta SET status='invalidated',version=version+1
                WHERE learner_id=? AND namespace=? AND event_id=? AND status='active'",
                params![lid, self.namespace, id],
            )?;
            // add_memory owns preference replacement and its supersedes list.
            // Revoke other derived summaries here; old preference rows remain
            // until that existing replacement step, with unusable source links.
            self.db.execute("UPDATE memories SET status='superseded' WHERE learner_id=? AND status='active' AND kind!='help_preference'
                AND memory_id IN(SELECT memory_id FROM memory_source_links WHERE learner_id=? AND namespace=? AND event_id=?)",
                params![lid,lid,self.namespace,id])?;
            self.db.execute("UPDATE memory_source_links SET status='invalidated' WHERE learner_id=? AND namespace=? AND event_id=?",
                params![lid,self.namespace,id])?;
        }
        for row in self.rows(
            "SELECT session_id,state FROM sessions WHERE learner_id=?",
            [lid],
        )? {
            let mut state: Value = serde_json::from_str(s(&row, "state"))?;
            let before = state.clone();
            let defaults = empty_state();
            // Match the existing correction logic's source-scoped fast-state
            // cleanup without deleting dialogue or advancing session generation.
            for field in [
                "goal",
                "stage",
                "reported_result",
                "pending_step",
                "modules",
            ] {
                if strings(&state["field_evidence"][field]["source_event_ids"])
                    .iter()
                    .any(|id| ids.contains(id))
                {
                    state[field] = defaults[field].clone();
                    state
                        .as_object_mut()
                        .unwrap()
                        .remove(&format!("{field}_source"));
                    if let Some(fields) = state["field_evidence"].as_object_mut() {
                        fields.remove(field);
                    }
                    if field == "modules" {
                        state["modules_source"] = json!("unknown");
                        state["requires_inventory_confirmation"] = json!(true);
                    }
                }
            }
            if let Some(attempts) = state["attempts"].as_array_mut() {
                attempts.retain(|attempt| {
                    !strings(&attempt["source_event_ids"])
                        .iter()
                        .any(|id| ids.contains(id))
                });
            }
            if strings(&state["session_preferences"]["source_event_ids"])
                .iter()
                .any(|id| ids.contains(id))
            {
                state["session_preferences"] = json!({});
            }
            if let Some(finished) = state["finish_memory_ids"].as_array_mut() {
                finished.retain(|id| id.as_str().is_none_or(|id| !revoked_memories.contains(id)));
            }
            if let Some(turn) = self.one(
                "SELECT evidence FROM turns WHERE learner_id=? AND session_id=? AND turn_id=?",
                params![
                    lid,
                    s(&row, "session_id"),
                    s(&state, "last_delivered_turn_id")
                ],
            )? {
                if strings(&serde_json::from_str::<Value>(s(&turn, "evidence"))?)
                    .iter()
                    .any(|id| ids.contains(id))
                {
                    state["last_delivered_hint"] = json!("");
                    state
                        .as_object_mut()
                        .unwrap()
                        .remove("last_delivered_turn_id");
                }
            }
            if state != before {
                self.checkpoint(s(&row, "session_id"), &state)?;
            }
        }
        Ok(())
    }

    pub fn save_help_preference(&self, lid: &str, sid: &str, data: &Value) -> Result<Value> {
        let request = request(data)?;
        self.tx(|| {
            let learner = self.learner(lid)?;
            let session = self.one("SELECT learner_id,generation,status FROM sessions WHERE session_id=?", [sid])?
                .ok_or_else(|| anyhow!("讲解偏好会话不存在"))?;
            ensure!(s(&session,"learner_id") == lid && s(&session,"status") != "finished", "讲解偏好会话已失效或不属于此档案");
            if let Some(receipt) = self.one("SELECT * FROM help_preference_receipts WHERE namespace=? AND learner_id=? AND request_id=?",
                params![self.namespace,lid,request.id])? {
                ensure!(receipt["session_id"] == sid && receipt["request_digest"] == request.digest
                    && receipt["expected_epoch"] == request.epoch && receipt["expected_generation"] == request.generation,
                    "讲解偏好请求编号已用于其他内容或会话");
                ensure!(learner["epoch"] == receipt["learner_epoch"], "讲解偏好回执已被更正、替换或忘记");
                let evidence = self.memory_evidence(lid, s(&receipt,"memory_id"))?;
                let sources = self.source_events(lid, &[s(&receipt,"event_id").to_owned()])?;
                ensure!(sources.len() == 1 && sources[0]["event_version"] == receipt["event_version"]
                    && sources[0]["session_id"] == sid && sources[0]["event_kind"] == "learner_statement"
                    && sources[0]["content"]["text"] == request.content
                    && sources[0]["content"]["explicit_preference"] == true
                    && sources[0]["content"]["preference_scope"] == "persistent"
                    && evidence["memory"]["kind"] == "help_preference", "讲解偏好原始来源已失效");
                return Ok(json!({"memory_id":receipt["memory_id"],"event_id":receipt["event_id"],
                    "learner_id":lid,"session_id":sid,"learner_epoch":receipt["learner_epoch"],"replayed":true}));
            }
            ensure!(learner["epoch"] == request.epoch && session["generation"] == request.generation,
                "讲解偏好请求已过期，请刷新后重新确认");
            let epoch = request.epoch.checked_add(1).ok_or_else(|| anyhow!("档案版本超出范围"))?;
            self.revoke_previous_help_preferences(lid)?;
            self.db.execute("UPDATE learners SET epoch=? WHERE learner_id=?", params![epoch,lid])?;
            let source = if self.namespace == "simulator" { "simulated" } else { "learner_report" };
            let event = self.record_education_event(lid,sid,source,"learner_statement",
                &json!({"text":request.content,"explicit_preference":true,"preference_scope":"persistent",
                    "confirmation":"explicit_save_action"}))?;
            let event_id = s(&event,"event_id");
            let candidate = json!({"kind":"help_preference","content":request.content,"scope":"cross_session_explicit",
                "source_event_ids":[event_id],"source_event_versions":{event_id:event["event_version"]}});
            let memory_id = self.add_memory(lid,&candidate,epoch)?.ok_or_else(|| anyhow!("讲解偏好来源未通过保存校验"))?;
            self.db.execute("INSERT INTO help_preference_receipts VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
                params![self.namespace,lid,request.id,sid,request.digest,request.epoch,request.generation,epoch,
                    memory_id,event_id,event["event_version"].as_i64(),now()])?;
            Ok(json!({"memory_id":memory_id,"event_id":event_id,"learner_id":lid,"session_id":sid,
                "learner_epoch":epoch,"replayed":false}))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(namespace: &str) -> Result<(tempfile::TempDir, Store, String, String)> {
        let directory = tempfile::tempdir()?;
        let store = Store::open(directory.path().join("preference.sqlite3"), namespace)?;
        let learner = store.create_learner("Synthetic preference", "zh", "guided")?;
        let lid = s(&learner, "learner_id").to_owned();
        let session = store.create_session(&lid)?;
        Ok((directory, store, lid, s(&session, "session_id").to_owned()))
    }
    fn input(id: &str, content: &str, epoch: i64, generation: i64) -> Value {
        json!({"request_id":id,"content":content,"expected_epoch":epoch,"expected_generation":generation})
    }

    #[test]
    fn explicit_original_text_has_versioned_sources_and_cross_activity_recall() -> Result<()> {
        for (namespace, source) in [("real", "learner_report"), ("simulator", "simulated")] {
            let (_directory, store, lid, sid) = fixture(namespace)?;
            let text = "  以后请先问我怎么想，再给一个提示。  ";
            let saved = store.save_help_preference(&lid, &sid, &input("save-1", text, 0, 0))?;
            ensure!(saved["learner_epoch"] == 1 && saved["replayed"] == false);
            ensure!(store.session(&sid)?["generation"] == 0);
            let evidence = store.memory_evidence(&lid, s(&saved, "memory_id"))?;
            ensure!(
                evidence["memory"]["content"] == text
                    && evidence["memory"]["scope"] == "cross_session_explicit"
            );
            ensure!(evidence["memory"]["source_type"] == source);
            ensure!(evidence["sources"][0]["content"]["text"] == text);
            ensure!(evidence["sources"][0]["event_kind"] == "learner_statement");
            ensure!(evidence["sources"][0]["content"]["explicit_preference"] == true);
            ensure!(
                evidence["source_links"][0]["event_version"]
                    == evidence["sources"][0]["event_version"]
            );
            let next = store.create_session(&lid)?;
            let recalled =
                store.recall_for_session(s(&next, "session_id"), "一个完全不同的问题", 5)?;
            ensure!(
                recalled
                    .iter()
                    .any(|row| row["memory_id"] == saved["memory_id"])
            );
        }
        Ok(())
    }

    #[test]
    fn lost_response_replays_after_generation_progress_and_reopen_without_new_write() -> Result<()>
    {
        let (directory, store, lid, sid) = fixture("real")?;
        let data = input(
            "retry-1",
            "Please wait for my answer before explaining.",
            0,
            0,
        );
        let saved = store.save_help_preference(&lid, &sid, &data)?;
        store.bump(&sid, "idle")?;
        let replay = store.save_help_preference(&lid, &sid, &data)?;
        ensure!(replay["replayed"] == true && replay["memory_id"] == saved["memory_id"]);
        ensure!(store.learner(&lid)?["epoch"] == 1 && store.session(&sid)?["generation"] == 1);
        ensure!(store.rows("SELECT event_id FROM events", [])?.len() == 1);
        let mut changed = data.clone();
        changed["content"] = json!("Different preference");
        ensure!(store.save_help_preference(&lid, &sid, &changed).is_err());
        changed = data.clone();
        changed["expected_generation"] = json!(1);
        ensure!(store.save_help_preference(&lid, &sid, &changed).is_err());
        drop(store);
        let reopened = Store::open(directory.path().join("preference.sqlite3"), "real")?;
        ensure!(reopened.save_help_preference(&lid, &sid, &data)?["replayed"] == true);
        Ok(())
    }

    #[test]
    fn replacement_revokes_old_model_authority_and_receipts_but_preserves_original_history()
    -> Result<()> {
        let (_directory, store, lid, sid) = fixture("real")?;
        let a = input("a", "以后先给我完整解释", 0, 0);
        let first = store.save_help_preference(&lid, &sid, &a)?;
        let old_source = s(&first, "event_id");
        let old_candidate = json!({"kind":"help_preference","content":a["content"],"scope":"cross_session_explicit",
            "source_event_ids":[old_source],"source_event_versions":{old_source:1}});
        let unrelated = store.record_education_event(
            &lid,
            &sid,
            "learner_report",
            "learner_statement",
            &json!({"text":"今天想了解距离模块"}),
        )?;
        let mut state = store.session(&sid)?["state"].clone();
        state["session_preferences"] =
            json!({"explicit_request":a["content"],"source_event_ids":[old_source]});
        state["goal"] = json!("了解距离模块");
        state["field_evidence"]["goal"] = json!({"source_event_ids":[unrelated["event_id"]]});
        state["reported_result"] = json!("旧偏好的派生表述");
        state["field_evidence"]["reported_result"] = json!({"source_event_ids":[old_source]});
        state["pending_step"] = json!("旧来源的待做步骤");
        state["field_evidence"]["pending_step"] = json!({"source_event_ids":[old_source]});
        state["attempts"] = json!([{"report":"旧来源","source_event_ids":[old_source]},
            {"report":"保留独立来源","source_event_ids":[unrelated["event_id"]]}]);
        store.checkpoint(&sid, &state)?;
        let second =
            store.save_help_preference(&lid, &sid, &input("b", "以后先问我一个问题", 1, 0))?;
        ensure!(second["learner_epoch"] == 2 && store.session(&sid)?["generation"] == 0);
        ensure!(store.session(&sid)?["state"]["session_preferences"] == json!({}));
        let current = store.session(&sid)?;
        ensure!(
            current["state"]["goal"] == "了解距离模块"
                && current["state"]["reported_result"] == ""
                && current["state"]["pending_step"] == ""
                && current["state"]["attempts"].as_array().unwrap().len() == 1
        );
        let question = store.record_education_event(
            &lid,
            &sid,
            "learner_report",
            "learner_statement",
            &json!({"text":"怎么开始？"}),
        )?;
        crate::context::build_context(
            &store,
            &store.session(&sid)?,
            &store.learner(&lid)?,
            "怎么开始？",
            s(&question, "event_id"),
            &json!({}),
        )?;
        ensure!(store.save_help_preference(&lid, &sid, &a).is_err());
        ensure!(store.add_memory(&lid, &old_candidate, 1)?.is_none());
        ensure!(store.add_memory(&lid, &old_candidate, 2)?.is_none());
        ensure!(
            store
                .source_events(&lid, &[old_source.to_owned()])?
                .is_empty()
        );
        let original = store
            .one("SELECT content FROM events WHERE event_id=?", [old_source])?
            .unwrap();
        ensure!(serde_json::from_str::<Value>(s(&original, "content"))?["text"] == a["content"]);
        let meta = store
            .one(
                "SELECT version,status FROM education_event_meta WHERE event_id=?",
                [old_source],
            )?
            .unwrap();
        ensure!(meta["version"] == 2 && meta["status"] == "invalidated");
        let memory = store.memory_evidence(&lid, s(&second, "memory_id"))?;
        ensure!(
            memory["memory"]["supersedes"]
                .as_array()
                .unwrap()
                .contains(&first["memory_id"])
        );
        ensure!(store.memories(&lid)?.len() == 1);
        Ok(())
    }

    #[test]
    fn correction_and_forgetting_never_revive_an_old_request_even_with_refreshed_versions()
    -> Result<()> {
        let (_directory, store, lid, sid) = fixture("real")?;
        let data = input("original", "以后一次只说一步", 0, 0);
        let saved = store.save_help_preference(&lid, &sid, &data)?;
        let corrected = store.correct_memory(&lid, s(&saved, "memory_id"), "以后先让我预测结果")?;
        ensure!(store.save_help_preference(&lid, &sid, &data).is_err());
        let mut refreshed = data.clone();
        refreshed["expected_epoch"] = store.learner(&lid)?["epoch"].clone();
        refreshed["expected_generation"] = store.session(&sid)?["generation"].clone();
        ensure!(store.save_help_preference(&lid, &sid, &refreshed).is_err());
        let next = store.create_session(&lid)?;
        ensure!(
            store
                .recall_for_session(s(&next, "session_id"), "怎么开始", 5)?
                .iter()
                .any(|row| row["memory_id"] == corrected)
        );
        store.forget(&lid, None)?;
        ensure!(store.save_help_preference(&lid, &sid, &data).is_err());
        refreshed["expected_epoch"] = store.learner(&lid)?["epoch"].clone();
        refreshed["expected_generation"] = store.session(&sid)?["generation"].clone();
        ensure!(store.save_help_preference(&lid, &sid, &refreshed).is_err());
        ensure!(store.memories(&lid)?.is_empty());
        ensure!(
            store
                .rows("SELECT request_id FROM help_preference_receipts", [])?
                .len()
                == 1
        );
        Ok(())
    }

    #[test]
    fn owner_namespace_and_late_new_writes_are_rejected_without_partial_state() -> Result<()> {
        let (_directory, store, lid, sid) = fixture("real")?;
        let other = store.create_learner("Other fixture", "en", "guided")?;
        let data = input("scope", "Always ask me first", 0, 0);
        ensure!(
            store
                .save_help_preference(s(&other, "learner_id"), &sid, &data)
                .is_err()
        );
        let (_otherdir, other_space, _, _) = fixture("simulator")?;
        ensure!(other_space.save_help_preference(&lid, &sid, &data).is_err());
        store.bump(&sid, "idle")?;
        ensure!(store.save_help_preference(&lid, &sid, &data).is_err());
        for (field, value) in [
            ("content", json!(" ")),
            ("content", json!("a".repeat(801))),
            ("expected_epoch", json!(-1)),
            ("expected_generation", json!("1")),
            ("request_id", json!("")),
        ] {
            let mut invalid = input("scope", "Always ask me first", 0, 1);
            invalid[field] = value;
            ensure!(store.save_help_preference(&lid, &sid, &invalid).is_err());
        }
        ensure!(
            store.learner(&lid)?["epoch"] == 0
                && store.rows("SELECT event_id FROM events", [])?.is_empty()
        );
        Ok(())
    }

    #[test]
    fn failed_receipt_commit_rolls_back_epoch_replacement_and_evidence() -> Result<()> {
        let (_directory, store, lid, sid) = fixture("real")?;
        let first_data = input("first", "以后先给提示", 0, 0);
        let first = store.save_help_preference(&lid, &sid, &first_data)?;
        store.db.execute_batch("CREATE TEMP TRIGGER fail_preference_receipt BEFORE INSERT ON help_preference_receipts BEGIN SELECT RAISE(ABORT,'fixture failure'); END;")?;
        ensure!(
            store
                .save_help_preference(&lid, &sid, &input("second", "以后先给问题", 1, 0))
                .is_err()
        );
        ensure!(store.learner(&lid)?["epoch"] == 1);
        ensure!(
            store
                .memory_evidence(&lid, s(&first, "memory_id"))?
                .get("memory")
                .is_some()
        );
        ensure!(store.save_help_preference(&lid, &sid, &first_data)?["replayed"] == true);
        ensure!(store.rows("SELECT event_id FROM events", [])?.len() == 1);
        Ok(())
    }

    #[test]
    fn ordinary_dialogue_does_not_gain_persistent_authority() -> Result<()> {
        let (_directory, store, lid, sid) = fixture("real")?;
        let event = store.record_education_event(
            &lid,
            &sid,
            "learner_report",
            "learner_statement",
            &json!({"text":"以后一次只说一步"}),
        )?;
        let candidate = json!({"kind":"help_preference","content":"以后一次只说一步",
            "scope":"cross_session_explicit","source_event_ids":[event["event_id"]]});
        ensure!(store.add_memory(&lid, &candidate, 0)?.is_none());
        ensure!(store.learner(&lid)?["epoch"] == 0 && store.memories(&lid)?.is_empty());
        Ok(())
    }
}
