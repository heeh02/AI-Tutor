//! Authenticated HTTP routing against temporary stores; no provider or device.
use super::*;
use tower::ServiceExt;

async fn post(
    app: Arc<Workstation>,
    path: &str,
    body: &Value,
    authenticated: bool,
) -> Result<(StatusCode, Value)> {
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header("Host", "127.0.0.1:48800")
        .header("Content-Type", "application/json");
    if authenticated {
        request = request.header("Cookie", "ai_tutor_local=preference-fixture-token");
    }
    let response = router(app)
        .oneshot(request.body(Body::from(body.to_string()))?)
        .await?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8192).await?;
    Ok((status, serde_json::from_slice(&bytes)?))
}

#[tokio::test]
async fn explicit_preference_http_keeps_generation_and_replays_without_cancel_or_new_evidence()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let app = Workstation::new(directory.path().into(), "preference-fixture-token".into())?;
    let (lid, sid, other) = {
        let stores = app.stores.lock().unwrap();
        let store = &stores["simulator"];
        let learner = store.create_learner("Synthetic preference", "en", "guided")?;
        let other = store.create_learner("Other synthetic learner", "en", "guided")?;
        let session = store.create_session(s(&learner, "learner_id"))?;
        (
            s(&learner, "learner_id").to_owned(),
            s(&session, "session_id").to_owned(),
            s(&other, "learner_id").to_owned(),
        )
    };
    *app.current.lock().unwrap() = Some(("simulator".into(), sid.clone()));
    let path = format!("/api/learners/{lid}/sessions/{sid}/help-preference");
    let body = json!({"content":"Please ask what I think before giving an explanation.",
        "request_id":"preference-http-1","expected_epoch":0,"expected_generation":0});
    ensure!(post(app.clone(), &path, &body, false).await?.0 == StatusCode::UNAUTHORIZED);
    let wrong = format!("/api/learners/{other}/sessions/{sid}/help-preference");
    ensure!(post(app.clone(), &wrong, &body, true).await?.0 == StatusCode::BAD_REQUEST);
    let (status, saved) = post(app.clone(), &path, &body, true).await?;
    ensure!(status == StatusCode::OK && saved["replayed"] == false && saved["learner_epoch"] == 1);
    ensure!(saved["learner_id"] == lid && saved["session_id"] == sid);
    {
        let stores = app.stores.lock().unwrap();
        let store = &stores["simulator"];
        ensure!(store.session(&sid)?["generation"] == 0);
        let evidence = store.memory_evidence(&lid, s(&saved, "memory_id"))?;
        ensure!(evidence["sources"][0]["source_type"] == "simulated");
        ensure!(evidence["sources"][0]["content"]["text"] == body["content"]);
        store.bump(&sid, "idle")?;
    }
    let speech_epoch = app.audio.speech_epoch();
    let intent = app.ptz.intent_generation();
    let (status, replayed) = post(app.clone(), &path, &body, true).await?;
    ensure!(status == StatusCode::OK && replayed["replayed"] == true);
    ensure!(
        replayed["memory_id"] == saved["memory_id"] && replayed["event_id"] == saved["event_id"]
    );
    ensure!(app.audio.speech_epoch() == speech_epoch && app.ptz.intent_generation() == intent);
    let late = json!({"request_id":"late","content":"A stale action","expected_epoch":1,"expected_generation":0});
    ensure!(post(app.clone(), &path, &late, true).await?.0 == StatusCode::BAD_REQUEST);
    let mut injected = body.clone();
    injected["source_type"] = json!("operator_confirmed");
    ensure!(post(app.clone(), &path, &injected, true).await?.0 == StatusCode::BAD_REQUEST);
    let stores = app.stores.lock().unwrap();
    let store = &stores["simulator"];
    ensure!(store.learner(&lid)?["epoch"] == 1 && store.session(&sid)?["generation"] == 1);
    ensure!(store.rows("SELECT event_id FROM events", [])?.len() == 1);
    ensure!(
        store
            .learners()?
            .iter()
            .find(|row| row["learner_id"] == lid)
            .unwrap()["epoch"]
            == 1
    );
    Ok(())
}

#[tokio::test]
async fn explicit_preference_http_rejects_old_profile_or_namespace_after_selection_changes()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let app = Workstation::new(directory.path().into(), "preference-fixture-token".into())?;
    let ((lid, sid), (sim_lid, sim_sid)) = {
        let stores = app.stores.lock().unwrap();
        let make = |namespace: &str| -> Result<(String, String)> {
            let store = &stores[namespace];
            let learner = store.create_learner("Synthetic owner", "zh", "guided")?;
            let session = store.create_session(s(&learner, "learner_id"))?;
            Ok((
                s(&learner, "learner_id").to_owned(),
                s(&session, "session_id").to_owned(),
            ))
        };
        (make("real")?, make("simulator")?)
    };
    *app.current.lock().unwrap() = Some(("simulator".into(), sim_sid.clone()));
    let body = json!({"request_id":"old-profile","content":"以后先问我一个问题","expected_epoch":0,"expected_generation":0});
    let old = format!("/api/learners/{lid}/sessions/{sid}/help-preference");
    ensure!(post(app.clone(), &old, &body, true).await?.0 == StatusCode::BAD_REQUEST);
    let mixed = format!("/api/learners/{lid}/sessions/{sim_sid}/help-preference");
    ensure!(post(app.clone(), &mixed, &body, true).await?.0 == StatusCode::BAD_REQUEST);
    let current = format!("/api/learners/{sim_lid}/sessions/{sim_sid}/help-preference");
    let (status, _) = post(app.clone(), &current, &body, true).await?;
    ensure!(status == StatusCode::OK);
    {
        let stores = app.stores.lock().unwrap();
        ensure!(stores["real"].memories(&lid)?.is_empty());
        stores["simulator"].consolidate_and_finish(&sim_sid)?;
    }
    ensure!(post(app.clone(), &current, &body, true).await?.0 == StatusCode::BAD_REQUEST);
    Ok(())
}
