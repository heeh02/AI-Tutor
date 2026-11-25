//! Real loopback contract; synthetic records and fake Pending only, no model/devices.
use super::*;

#[tokio::test]
async fn comparison_http_first_save_cancels_but_retry_and_invalid_input_preserve_new_work()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let app = Workstation::new(directory.path().into(), "comparison-fixture-token".into())?;
    let (lid, other, sid) = {
        let stores = app.stores.lock().unwrap();
        let store = &stores["simulator"];
        let learner = store.create_learner("Synthetic comparison", "en", "guided")?;
        let other = store.create_learner("Other comparison", "zh", "guided")?;
        let session = store.create_session(s(&learner, "learner_id"))?;
        (
            s(&learner, "learner_id").to_owned(),
            s(&other, "learner_id").to_owned(),
            s(&session, "session_id").to_owned(),
        )
    };
    *app.current.lock().unwrap() = Some(("simulator".into(), sid.clone()));
    let insert_pending = |id: &str, generation| {
        app.pending.lock().unwrap().insert(
            id.into(),
            Pending {
                sid: sid.clone(),
                generation,
                epoch: 0,
                reply: Value::Null,
                _foreground: None,
            },
        );
    };
    insert_pending("before-comparison", 0);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let routes = router(app.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()?;
    let url = format!("http://{address}/api/learners/{lid}/sessions/{sid}/comparisons");
    let cookie = "ai_tutor_local=comparison-fixture-token";
    ensure!(client.get(&url).send().await?.status() == 401);
    let data = json!({"submission_id":"fixture-comparison-1","question":"Does the threshold change the result?",
        "changed_variable":"distance threshold","conditions":[{"name":"same car and speed","status":"reported_same"}],
        "trials":[{"label":"A","setting":"20 cm","status":"reported","result":"hit the wall once","trial_count":1},
                  {"label":"B","setting":"30 cm","status":"not_tested","result":null,"trial_count":null}],
        "pending_plan":{"text":"Try thirty centimeters next time","accepted":false},"source_type":"learner_report"});
    let mut invalid = data.clone();
    invalid["trials"][1]["trial_count"] = json!(1);
    ensure!(
        client
            .post(&url)
            .header("Cookie", cookie)
            .json(&invalid)
            .send()
            .await?
            .status()
            == 400
    );
    let wrong_owner = format!("http://{address}/api/learners/{other}/sessions/{sid}/comparisons");
    ensure!(
        client
            .post(&wrong_owner)
            .header("Cookie", cookie)
            .json(&data)
            .send()
            .await?
            .status()
            == 400
    );
    ensure!(app.stores.lock().unwrap()["simulator"].session(&sid)?["generation"] == 0);
    ensure!(
        app.pending
            .lock()
            .unwrap()
            .contains_key("before-comparison")
    );
    let response = client
        .post(&url)
        .header("Cookie", cookie)
        .json(&data)
        .send()
        .await?;
    ensure!(response.status() == 200);
    let saved: Value = response.json().await?;
    ensure!(
        saved["item"]["source_type"] == "simulated" && saved["item"]["hardware_verified"] == false
    );
    ensure!(
        !app.pending
            .lock()
            .unwrap()
            .contains_key("before-comparison")
    );
    let generation = {
        let stores = app.stores.lock().unwrap();
        let store = &stores["simulator"];
        let session = store.session(&sid)?;
        ensure!(session["state"]["attempts"].as_array().unwrap().len() == 1);
        ensure!(store.memories(&lid)?.is_empty());
        store.set_status(&sid, "generating")?;
        session["generation"].as_i64().unwrap()
    };
    ensure!(generation > 0);
    insert_pending("newer-lesson", generation);
    let replay: Value = client
        .post(&url)
        .header("Cookie", cookie)
        .json(&data)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    ensure!(replay == saved);
    let mut conflict = data.clone();
    conflict["question"] = json!("Changed payload with the same submission id");
    ensure!(
        client
            .post(&url)
            .header("Cookie", cookie)
            .json(&conflict)
            .send()
            .await?
            .status()
            == 400
    );
    ensure!(app.pending.lock().unwrap().contains_key("newer-lesson"));
    let session = app.stores.lock().unwrap()["simulator"].session(&sid)?;
    ensure!(session["generation"] == generation && session["status"] == "generating");
    app.stores.lock().unwrap()["simulator"].set_status(&sid, "finished")?;
    ensure!(
        client
            .post(&url)
            .header("Cookie", cookie)
            .json(&data)
            .send()
            .await?
            .status()
            == 400
    );
    let history: Value = client
        .get(&url)
        .header("Cookie", cookie)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    ensure!(
        history["items"].as_array().unwrap().len() == 1 && history["items"][0] == saved["item"]
    );
    ensure!(
        client
            .get(&wrong_owner)
            .header("Cookie", cookie)
            .send()
            .await?
            .status()
            == 400
    );
    let state = app.state()?;
    ensure!(
        state["camera"]["state"] == "closed"
            && state["audio"]["recording"] == false
            && state["ptz"]["mode"] == "off"
    );
    server.abort();
    let _ = server.await;
    app.close().await;
    Ok(())
}
