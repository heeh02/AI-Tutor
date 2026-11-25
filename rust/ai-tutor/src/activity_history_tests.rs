//! Real Store and dispatch/router; no provider request or device is opened.
use super::*;
use tower::ServiceExt;

struct Fixture {
    _directory: tempfile::TempDir,
    app: Arc<Workstation>,
    lid: String,
    source: Value,
    current: Value,
    other_lid: String,
    foreign: Value,
}
impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let app = Workstation::new(directory.path().into(), "continuation-fixture-token".into())?;
        let (lid, source, current, other_lid, foreign) = {
            let stores = app.stores.lock().unwrap();
            let store = &stores["simulator"];
            let learner =
                store.create_learner("Synthetic activity continuation", "zh", "guided")?;
            let lid = s(&learner, "learner_id").to_owned();
            let initial = store.create_session(&lid)?;
            let sid = s(&initial, "session_id");
            let event = store.record_education_event(
                &lid,
                sid,
                "simulated",
                "learner_statement",
                &json!({"text":"我想比较纸桥的跨度；下次用同样的纸再测。"}),
            )?;
            let mut state = initial["state"].clone();
            state["goal"] = json!("比较纸桥的跨度");
            state["goal_source"] = json!("simulated");
            state["field_evidence"]["goal"] =
                json!({"source_event_ids":[event["event_id"]],"historical":false});
            state["modules"] = json!(["control", "motor"]);
            state["modules_source"] = json!("learner_report");
            state["requires_inventory_confirmation"] = json!(false);
            store.checkpoint(sid, &state)?;
            let current = store.create_session(&lid)?;
            let source = store.session(sid)?;
            let other = store.create_learner("Other synthetic owner", "en", "one_step")?;
            let other_lid = s(&other, "learner_id").to_owned();
            let foreign = store.create_session(&other_lid)?;
            store.create_session(&other_lid)?;
            (lid, source, current, other_lid, foreign)
        };
        *app.current.lock().unwrap() =
            Some(("simulator".into(), s(&current, "session_id").to_owned()));
        Ok(Self {
            _directory: directory,
            app,
            lid,
            source,
            current,
            other_lid,
            foreign,
        })
    }
    fn intent(&self) -> Value {
        json!({"client_submission_id":"continue-fixture-1",
            "expected_current_session_id":self.current["session_id"],
            "expected_generation":self.current["generation"]})
    }
    fn path(&self) -> String {
        format!(
            "/api/learners/{}/sessions/{}/continue",
            self.lid,
            s(&self.source, "session_id")
        )
    }
    fn markers(&self) -> Result<Value> {
        let current = self.app.current.lock().unwrap().clone();
        let mut pending = self
            .app
            .pending
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        pending.sort();
        let stores = self.app.stores.lock().unwrap();
        Ok(json!({"current":current,"pending":pending,
            "sessions":stores["simulator"].rows("SELECT session_id,learner_id,activity_id,generation,status,state FROM sessions ORDER BY rowid",[])?,
            "events":stores["simulator"].rows("SELECT count(*) AS n FROM events",[])?,
            "recording_generation":self.app.audio.status()["recording_generation"],
            "speech_epoch":self.app.audio.speech_epoch(),"ptz_intent":self.app.ptz.intent_generation(),
            "provider_requests":self.app.provider.status()["metrics"]["requests"]}))
    }
    fn pending(&self, sid: &str, key: &str, generation: i64) {
        self.app.pending.lock().unwrap().insert(
            key.into(),
            Pending {
                sid: sid.into(),
                generation,
                epoch: 0,
                reply: Value::Null,
                _foreground: None,
            },
        );
    }
    async fn http(
        &self,
        method: Method,
        path: &str,
        data: Option<&Value>,
        authenticated: bool,
    ) -> Result<(StatusCode, Value)> {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:8765");
        if authenticated {
            request = request.header("cookie", "ai_tutor_local=continuation-fixture-token");
        }
        let body = match data {
            Some(data) => {
                request = request.header("content-type", "application/json");
                Body::from(data.to_string())
            }
            None => Body::empty(),
        };
        let response = router(self.app.clone())
            .oneshot(request.body(body)?)
            .await?;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 100_000).await?;
        Ok((
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        ))
    }
}

#[tokio::test]
async fn explicit_continuation_creates_new_identity_and_replay_preserves_new_work() -> Result<()> {
    let f = Fixture::new()?;
    let intent = f.intent();
    let old_sid = s(&f.source, "session_id");
    f.pending(s(&f.current, "session_id"), "old-pending", 0);
    let (status, response) = f.http(Method::POST, &f.path(), Some(&intent), true).await?;
    ensure!(status == StatusCode::OK, "continue response: {response}");
    let new_sid = s(&response, "session_id");
    ensure!(new_sid != old_sid && new_sid != s(&f.current, "session_id"));
    ensure!(
        response["activity_id"] != f.source["activity_id"]
            && response["activity_id"] != f.current["activity_id"]
    );
    ensure!(
        response["client_submission_id"] == intent["client_submission_id"]
            && response["replayed"] == false
    );
    ensure!(response["session"]["continued_from"]["session_id"] == old_sid);
    ensure!(
        response["session"]["continuation"]["client_submission_id"]
            == intent["client_submission_id"]
    );
    ensure!(response["session"]["continuation"]["historical"] == true);
    ensure!(
        response["session"]["state"]["modules"].is_null()
            && response["session"]["state"]["requires_inventory_confirmation"] == true
    );
    ensure!(
        response["session"]["turns"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    ensure!(!f.app.pending.lock().unwrap().contains_key("old-pending"));
    let generation = f.app.stores.lock().unwrap()["simulator"].bump(new_sid, "generating")?;
    f.pending(new_sid, "new-pending", generation);
    f.app.audio.reserve_speech_epoch();
    let before = f.markers()?;
    let replay = dispatch(f.app.clone(), &Method::POST, &f.path(), &intent).await?;
    ensure!(replay["session_id"] == new_sid && replay["replayed"] == true);
    ensure!(f.markers()? == before, "replay changed current work");
    for key in ["expected_generation", "expected_current_session_id"] {
        let mut changed = intent.clone();
        changed[key] = if key == "expected_generation" {
            json!(1)
        } else {
            json!(new_sid)
        };
        ensure!(
            dispatch(f.app.clone(), &Method::POST, &f.path(), &changed)
                .await
                .is_err()
        );
        ensure!(
            f.markers()? == before,
            "conflicting nonce payload changed current work"
        );
    }
    let other_target = format!(
        "/api/learners/{}/sessions/{}/continue",
        f.lid,
        s(&f.current, "session_id")
    );
    ensure!(
        dispatch(f.app.clone(), &Method::POST, &other_target, &intent)
            .await
            .is_err()
    );
    ensure!(
        f.markers()? == before,
        "same nonce selecting a different source changed current work"
    );
    // The historical activity/session are readable provenance, never a write alias.
    for (path, body) in [
        (
            format!("/api/activities/{}/events", s(&f.source, "activity_id")),
            json!({"source_type":"simulated","report":"late old activity report"}),
        ),
        (
            format!("/api/sessions/{old_sid}/turns"),
            json!({"text":"late old session input","expected_generation":f.source["generation"]}),
        ),
    ] {
        ensure!(
            dispatch(f.app.clone(), &Method::POST, &path, &body)
                .await
                .is_err()
        );
        ensure!(
            f.markers()? == before,
            "old identity wrote into the continued activity"
        );
    }
    ensure!(
        f.app.state()?["session"]["continuation"]["client_submission_id"]
            == intent["client_submission_id"]
    );
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn invalid_continuations_preserve_current_generation_pending_and_audio() -> Result<()> {
    let f = Fixture::new()?;
    f.pending(s(&f.current, "session_id"), "untouched", 0);
    let original = f.intent();
    let real_lid = {
        let stores = f.app.stores.lock().unwrap();
        let learner = stores["real"].create_learner("Other namespace", "en", "guided")?;
        s(&learner, "learner_id").to_owned()
    };
    let before = f.markers()?;
    let mut stale = original.clone();
    stale["expected_generation"] = json!(999);
    let mut wrong_current = original.clone();
    wrong_current["expected_current_session_id"] = f.source["session_id"].clone();
    let mut malformed = original.clone();
    malformed["client_submission_id"] = json!("not allowed nonce");
    let mut missing = original.clone();
    missing
        .as_object_mut()
        .unwrap()
        .remove("expected_generation");
    let mut extra = original.clone();
    extra["activity_id"] = f.source["activity_id"].clone();
    let cases = vec![
        (f.path(), stale),
        (f.path(), wrong_current),
        (f.path(), malformed),
        (f.path(), missing),
        (f.path(), extra),
        (
            format!(
                "/api/learners/{}/sessions/{}/continue",
                f.lid,
                s(&f.current, "session_id")
            ),
            original.clone(),
        ),
        (
            format!(
                "/api/learners/{}/sessions/{}/continue",
                f.lid,
                s(&f.foreign, "session_id")
            ),
            original.clone(),
        ),
        (
            format!(
                "/api/learners/{}/sessions/{}/continue",
                f.other_lid,
                s(&f.source, "session_id")
            ),
            original.clone(),
        ),
        (
            format!("/api/learners/{}/sessions/session_missing/continue", f.lid),
            original.clone(),
        ),
        (
            format!(
                "/api/learners/{real_lid}/sessions/{}/continue",
                s(&f.source, "session_id")
            ),
            original.clone(),
        ),
    ];
    for (path, body) in cases {
        ensure!(
            dispatch(f.app.clone(), &Method::POST, &path, &body)
                .await
                .is_err(),
            "invalid continuation unexpectedly accepted"
        );
        ensure!(
            f.markers()? == before,
            "invalid continuation had side effects"
        );
    }
    let (status, _) = f
        .http(Method::POST, &f.path(), Some(&original), false)
        .await?;
    ensure!(status == StatusCode::UNAUTHORIZED && f.markers()? == before);
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn continuation_late_retry_after_new_activity_never_reselects_the_old_result() -> Result<()> {
    let f = Fixture::new()?;
    let intent = f.intent();
    let first = dispatch(f.app.clone(), &Method::POST, &f.path(), &intent).await?;
    let next = dispatch(
        f.app.clone(),
        &Method::POST,
        "/api/sessions",
        &json!({"learner_id":f.lid,"resume":false}),
    )
    .await?;
    ensure!(
        next["session_id"] != first["session_id"] && next["activity_id"] != first["activity_id"]
    );
    f.pending(
        s(&next, "session_id"),
        "new-activity-pending",
        next["generation"].as_i64().unwrap(),
    );
    let before = f.markers()?;
    ensure!(
        dispatch(f.app.clone(), &Method::POST, &f.path(), &intent)
            .await
            .is_err()
    );
    ensure!(
        f.markers()? == before,
        "late replay reselected or cancelled another activity"
    );
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn continuation_replay_of_finished_current_activity_is_read_only() -> Result<()> {
    let f = Fixture::new()?;
    let intent = f.intent();
    let first = dispatch(f.app.clone(), &Method::POST, &f.path(), &intent).await?;
    dispatch(
        f.app.clone(),
        &Method::POST,
        &format!("/api/sessions/{}/finish", s(&first, "session_id")),
        &json!({}),
    )
    .await?;
    let before = f.markers()?;
    let replay = dispatch(f.app.clone(), &Method::POST, &f.path(), &intent).await?;
    ensure!(
        replay["replayed"] == true
            && replay["session_id"] == first["session_id"]
            && replay["session"]["status"] == "finished"
    );
    ensure!(
        f.markers()? == before,
        "finished-current replay restarted or cancelled activity"
    );
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn activity_catalog_http_is_owner_paged_and_read_only() -> Result<()> {
    let f = Fixture::new()?;
    let base = format!("/api/learners/{}/sessions", f.lid);
    let before = f.markers()?;
    let (status, _) = f.http(Method::GET, &base, None, false).await?;
    ensure!(status == StatusCode::UNAUTHORIZED);
    let (status, page) = f
        .http(Method::GET, &format!("{base}?limit=1"), None, true)
        .await?;
    ensure!(
        status == StatusCode::OK && page["learner_id"] == f.lid && page["namespace"] == "simulator"
    );
    ensure!(page["items"].as_array().is_some_and(|rows| rows.len() == 1));
    ensure!(page["items"][0]["session_id"] == f.current["session_id"]);
    let cursor = page["next_before"]
        .as_i64()
        .ok_or_else(|| anyhow!("directory has no next cursor"))?;
    let (status, older) = f
        .http(
            Method::GET,
            &format!("{base}?limit=1&before={cursor}"),
            None,
            true,
        )
        .await?;
    ensure!(
        status == StatusCode::OK
            && older["items"]
                .as_array()
                .is_some_and(|rows| rows.len() == 1)
    );
    ensure!(
        older["items"][0]["session_id"] == f.source["session_id"]
            && older["items"][0]["status"] == "finished"
    );
    ensure!(older["next_before"].is_null());
    ensure!(older["items"][0]["session_id"] != f.foreign["session_id"]);
    for invalid in [
        format!("{base}?limit=0"),
        format!("{base}?before=-1"),
        "/api/learners/learner_missing/sessions?limit=20".into(),
    ] {
        let (status, _) = f.http(Method::GET, &invalid, None, true).await?;
        ensure!(status == StatusCode::BAD_REQUEST);
    }
    ensure!(
        f.markers()? == before,
        "catalog read interrupted current work"
    );
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn comparison_history_http_pages_finished_activity_without_changing_current_work() -> Result<()> {
    let f = Fixture::new()?;
    let historical_sid = s(&f.current, "session_id");
    let (new_current, real_lid) = {
        let stores = f.app.stores.lock().unwrap();
        let store = &stores["simulator"];
        for i in 0..14 {
            store.save_comparison(&f.lid, historical_sid, &json!({
                "submission_id":format!("history-{i}"),"source_type":"simulated",
                "question":"跨度改变后纸桥承重如何？","changed_variable":"跨度",
                "conditions":[{"name":"折法","status":"not_confirmed"}],
                "trials":[{"label":"A","setting":"15 cm","status":"reported","result":"一次放了 3 枚币。","trial_count":1},
                    {"label":"B","setting":"跨度待定","status":"not_tested","result":null,"trial_count":null}],
                "pending_plan":{"text":"下次改变跨度再试，控制条件未定。","accepted":false}
            }))?;
        }
        let invalid = store.one(
            "SELECT pending_id FROM experiment_comparisons WHERE submission_id='history-13'",
            [],
        )?.unwrap();
        store.db.execute(
            "UPDATE education_event_meta SET version=version+1 WHERE event_id=?",
            [s(&invalid, "pending_id")],
        )?;
        let current = store.create_session(&f.lid)?;
        let real = stores["real"].create_learner("Other namespace reader", "en", "guided")?;
        (current, s(&real, "learner_id").to_owned())
    };
    *f.app.current.lock().unwrap() =
        Some(("simulator".into(), s(&new_current, "session_id").to_owned()));
    f.pending(s(&new_current, "session_id"), "preserve-current-reply", 0);
    let base = format!("/api/learners/{}/sessions/{historical_sid}/comparison-history", f.lid);
    let before = f.markers()?;
    let (status, _) = f.http(Method::GET, &base, None, false).await?;
    ensure!(status == StatusCode::UNAUTHORIZED);
    let (status, first) = f.http(Method::GET, &base, None, true).await?;
    ensure!(status == StatusCode::OK && first["namespace"] == "simulator");
    ensure!(first["learner_id"] == f.lid && first["session_id"] == historical_sid);
    ensure!(first["items"].as_array().unwrap().len() == 10);
    ensure!(first["items"][0]["submission_id"] == "history-12");
    ensure!(first["items"][9]["submission_id"] == "history-3");
    ensure!(first["items"][0]["trials"][1]["status"] == "not_tested");
    ensure!(first["items"][0]["pending_plan"]["accepted"] == false);
    let cursor = first["next_before"].as_i64().ok_or_else(|| anyhow!("missing history cursor"))?;
    let (status, last) = f.http(Method::GET, &format!("{base}?before={cursor}&limit=10"), None, true).await?;
    ensure!(status == StatusCode::OK && last["items"].as_array().unwrap().len() == 3);
    ensure!(last["items"][0]["submission_id"] == "history-2"
        && last["items"][2]["submission_id"] == "history-0"
        && last["next_before"].is_null());
    let (status, legacy) = f.http(Method::GET,
        &format!("/api/learners/{}/sessions/{historical_sid}/comparisons", f.lid), None, true).await?;
    ensure!(status == StatusCode::OK && legacy["items"] == first["items"]);
    ensure!(legacy.as_object().unwrap().len() == 3);
    for invalid in [
        format!("{base}?limit=0"), format!("{base}?limit=21"),
        format!("{base}?before=0"), format!("{base}?before=-1"),
        format!("{base}?before=invalid"),
        format!("/api/learners/{}/sessions/{historical_sid}/comparison-history", f.other_lid),
        format!("/api/learners/{real_lid}/sessions/{historical_sid}/comparison-history"),
        format!("/api/learners/{}/sessions/missing-session/comparison-history", f.lid),
    ] {
        let (status, _) = f.http(Method::GET, &invalid, None, true).await?;
        ensure!(status == StatusCode::BAD_REQUEST, "invalid history request accepted: {invalid}");
    }
    ensure!(f.markers()? == before, "comparison history interrupted current work");
    f.app.close().await;
    Ok(())
}
