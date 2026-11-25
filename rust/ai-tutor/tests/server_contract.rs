//! Native API + real SQLite integration. Synthetic records, no remote/device test.
use ai_tutor::{
    server::{Workstation, router},
    store::{Store, s},
};
use anyhow::{Result, ensure};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

struct Harness {
    dir: tempfile::TempDir,
    app: Arc<Workstation>,
    router: Router,
}
impl Harness {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let app = Workstation::new(dir.path().to_owned(), "SYNTHETIC_TEST_TOKEN".into())?;
        let router = router(app.clone());
        Ok(Self { dir, app, router })
    }
    async fn call(&self, method: &str, path: &str, value: Value) -> Result<(StatusCode, Value)> {
        let body = if value.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&value)?
        };
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:8765")
            .header("cookie", "ai_tutor_local=SYNTHETIC_TEST_TOKEN")
            .header("content-type", "application/json")
            .body(Body::from(body))?;
        let response = self.router.clone().oneshot(request).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 100_000).await?;
        let data = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Ok((status, data))
    }
    async fn api(&self, method: &str, path: &str, value: Value) -> Result<Value> {
        let (status, value) = self.call(method, path, value).await?;
        ensure!(status.is_success(), "native API operation failed: {status}");
        Ok(value)
    }
    async fn learner(&self, name: &str) -> Result<Value> {
        self.api("POST", "/api/learners", json!({"name":name}))
            .await
    }
    async fn select(&self, lid: &str) -> Result<Value> {
        self.api(
            "POST",
            "/api/sessions",
            json!({"learner_id":lid,"resume":true}),
        )
        .await
    }
    fn store(&self) -> Result<Store> {
        Store::open(self.dir.path().join("runtime/data/real.sqlite3"), "real")
    }
}

#[tokio::test]
async fn history_route_honors_cursor_limit_and_owner_without_delivering_turns() -> Result<()> {
    let h = Harness::new()?;
    let store = h.store()?;
    let learner = store.create_learner("history synthetic", "zh", "guided")?;
    let other = store.create_learner("other synthetic", "en", "guided")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    for index in 0..4 {
        store.add_turn(
            sid,
            "history-request",
            0,
            "assistant",
            &format!("synthetic {index}"),
            "generated",
            &[],
        )?;
    }
    let path = format!("/api/learners/{lid}/sessions/{sid}/history");
    let first = h
        .api("GET", &format!("{path}?limit=2"), Value::Null)
        .await?;
    ensure!(first["items"].as_array().unwrap().len() == 2);
    ensure!(first["items"][0]["content"] == "synthetic 2");
    ensure!(first["items"][1]["status"] == "generated");
    let before = first["next_before"].as_i64().unwrap();
    let second = h
        .api(
            "GET",
            &format!("{path}?limit=2&before={before}"),
            Value::Null,
        )
        .await?;
    ensure!(second["items"][0]["content"] == "synthetic 0");
    ensure!(second["items"][1]["content"] == "synthetic 1");
    ensure!(second["next_before"].is_null());
    for suffix in ["?limit=0", "?limit=51", "?before=-1", "?before=invalid"] {
        ensure!(
            h.call("GET", &format!("{path}{suffix}"), Value::Null)
                .await?
                .0
                == StatusCode::BAD_REQUEST
        );
    }
    ensure!(
        h.call(
            "GET",
            &format!(
                "/api/learners/{}/sessions/{sid}/history",
                s(&other, "learner_id")
            ),
            Value::Null
        )
        .await?
        .0 == StatusCode::BAD_REQUEST
    );
    let generated: i64 = store.db.query_row(
        "SELECT count(*) FROM turns WHERE session_id=? AND status='generated'",
        [sid],
        |row| row.get(0),
    )?;
    ensure!(generated == 4, "reading history must not confirm delivery");
    h.app.close().await;
    Ok(())
}

#[tokio::test]
async fn native_http_origin_token_body_and_command_field_guards() -> Result<()> {
    let h = Harness::new()?;
    for (path, host, origin, cookie, expected) in [
        (
            "/api/state",
            "127.0.0.1:8765",
            None,
            None,
            StatusCode::UNAUTHORIZED,
        ),
        ("/api/health", "127.0.0.1:8765", None, None, StatusCode::OK),
        (
            "/api/state",
            "127.0.0.1:8765",
            Some("https://evil.example"),
            Some("ai_tutor_local=SYNTHETIC_TEST_TOKEN"),
            StatusCode::FORBIDDEN,
        ),
        (
            "/api/events",
            "127.0.0.1:8765",
            Some("https://evil.example"),
            Some("ai_tutor_local=SYNTHETIC_TEST_TOKEN"),
            StatusCode::FORBIDDEN,
        ),
        (
            "/api/events",
            "127.0.0.1:8765",
            None,
            None,
            StatusCode::UNAUTHORIZED,
        ),
        (
            "/api/health",
            "evil.example:8765",
            None,
            None,
            StatusCode::FORBIDDEN,
        ),
    ] {
        let mut request = Request::builder().uri(path).header("host", host);
        if let Some(origin) = origin {
            request = request.header("origin", origin)
        }
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie)
        }
        let response = h
            .router
            .clone()
            .oneshot(request.body(Body::empty())?)
            .await?;
        ensure!(response.status() == expected, "guard failed for {path}");
    }
    let response = h
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/?token=SYNTHETIC_TEST_TOKEN")
                .header("host", "127.0.0.1:8765")
                .body(Body::empty())?,
        )
        .await?;
    ensure!(response.status() == StatusCode::SEE_OTHER);
    let cookie = response.headers()["set-cookie"].to_str()?;
    ensure!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    let (status, _) = h
        .call(
            "POST",
            "/api/learners",
            json!({"name":"A","shell":"forbidden"}),
        )
        .await?;
    ensure!(status == StatusCode::BAD_REQUEST);
    let request = Request::builder()
        .method("POST")
        .uri("/api/learners")
        .header("host", "127.0.0.1:8765")
        .header("cookie", "ai_tutor_local=SYNTHETIC_TEST_TOKEN")
        .body(Body::from(vec![b'x'; 25000]))?;
    ensure!(h.router.clone().oneshot(request).await?.status() == StatusCode::PAYLOAD_TOO_LARGE);
    h.app.close().await;
    Ok(())
}

#[tokio::test]
async fn native_delivery_cancel_learner_isolation_and_delete_epoch() -> Result<()> {
    let h = Harness::new()?;
    let a = h.learner("synthetic-A").await?;
    let b = h.learner("synthetic-B").await?;
    let lid = s(&a, "learner_id");
    let session = h.select(lid).await?;
    let sid = s(&session, "session_id");
    let store = h.store()?;
    let event = store.event(
        lid,
        sid,
        "learner_report",
        &json!({"text":"SYNTHETIC A REPORT 停车成功"}),
    )?;
    let candidate = json!({"kind":"reported_progress","content":"据合成测试报告停车成功","scope":"current_activity","source_event_ids":[event]});
    let mid = store.add_memory(lid, &candidate, 0)?.unwrap();
    let generation = session["generation"].as_i64().unwrap();
    let tid = store.add_turn(
        sid,
        "synthetic-request",
        generation,
        "assistant",
        "SYNTHETIC A DELIVERED HINT",
        "generated",
        &[],
    )?;
    ensure!(h.app.state()?["session"]["state"]["last_delivered_hint"] == "");
    h.api(
        "POST",
        &format!("/api/sessions/{sid}/delivery"),
        json!({"turn_id":tid,"status":"shown"}),
    )
    .await?;
    ensure!(
        h.app.state()?["session"]["state"]["last_delivered_hint"] == "SYNTHETIC A DELIVERED HINT"
    );
    h.api("POST", "/api/tracking/clear", json!({})).await?;
    ensure!(h.app.state()?["session"]["learner_id"] == lid);
    let bs = h.select(s(&b, "learner_id")).await?;
    ensure!(
        bs["turns"].as_array().unwrap().is_empty()
            && bs["recalled_memories"].as_array().unwrap().is_empty()
    );
    let (status, _) = h
        .call(
            "POST",
            &format!("/api/sessions/{sid}/delivery"),
            json!({"turn_id":tid,"status":"shown"}),
        )
        .await?;
    ensure!(status == StatusCode::BAD_REQUEST);
    let session = h.select(lid).await?;
    ensure!(session["state"]["modules"].is_null());
    let generation = session["generation"].as_i64().unwrap();
    let cancelled = store.add_turn(
        sid,
        "synthetic-undelivered",
        generation,
        "assistant",
        "NEVER DELIVER THIS",
        "generated",
        &[],
    )?;
    h.api("POST", &format!("/api/sessions/{sid}/cancel"), json!({}))
        .await?;
    let (status, _) = h
        .call(
            "POST",
            &format!("/api/sessions/{sid}/delivery"),
            json!({"turn_id":cancelled,"status":"shown"}),
        )
        .await?;
    ensure!(status == StatusCode::BAD_REQUEST);
    h.api("DELETE", &format!("/api/learners/{lid}/data"), Value::Null)
        .await?;
    ensure!(
        h.api("GET", &format!("/api/learners/{lid}/memories"), Value::Null)
            .await?
            .as_array()
            .unwrap()
            .is_empty()
    );
    ensure!(
        h.app.state()?["session"]["turns"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    ensure!(store.add_memory(lid, &candidate, 0)?.is_none());
    ensure!(store.source_events(lid, &[event])?.is_empty());
    ensure!(
        !store
            .memories(lid)?
            .iter()
            .any(|m| s(m, "memory_id") == mid)
    );
    h.app.close().await;
    Ok(())
}
