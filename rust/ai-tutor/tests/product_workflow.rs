//! Product lifecycle regressions using the real HTTP router and temporary SQLite.
//! All records are synthetic. No camera, audio capture or remote model is invoked.
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

struct Workbench {
    _dir: tempfile::TempDir,
    app: Arc<Workstation>,
    router: Router,
}
impl Workbench {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let app = Workstation::new(dir.path().to_owned(), "SYNTHETIC_WORKFLOW_TOKEN".into())?;
        let router = router(app.clone());
        Ok(Self {
            _dir: dir,
            app,
            router,
        })
    }
    async fn request(&self, method: &str, path: &str, data: Value) -> Result<(StatusCode, Value)> {
        let body = if data.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&data)?
        };
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:8765")
            .header("cookie", "ai_tutor_local=SYNTHETIC_WORKFLOW_TOKEN")
            .header("content-type", "application/json")
            .body(Body::from(body))?;
        let response = self.router.clone().oneshot(request).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 100_000).await?;
        Ok((
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        ))
    }
    async fn api(&self, method: &str, path: &str, data: Value) -> Result<Value> {
        let (status, value) = self.request(method, path, data).await?;
        ensure!(status.is_success(), "{method} {path}: {status} {value}");
        Ok(value)
    }
    async fn learner(&self, name: &str, namespace: &str) -> Result<Value> {
        self.api(
            "POST",
            "/api/learners",
            json!({"name":name,"namespace":namespace}),
        )
        .await
    }
    async fn select(&self, learner: &Value, resume: bool) -> Result<Value> {
        self.api(
            "POST",
            "/api/sessions",
            json!({"learner_id":learner["learner_id"],"resume":resume}),
        )
        .await
    }
    fn store(&self, namespace: &str) -> Result<Store> {
        Store::open(
            self._dir
                .path()
                .join(format!("runtime/data/{namespace}.sqlite3")),
            namespace,
        )
    }
    async fn report(&self, session: &Value, value: Value) -> Result<Value> {
        self.api(
            "POST",
            &format!("/api/activities/{}/events", s(session, "activity_id")),
            value,
        )
        .await
    }
    async fn finish(&self, session: &Value) -> Result<Value> {
        self.api(
            "POST",
            &format!("/api/sessions/{}/finish", s(session, "session_id")),
            json!({}),
        )
        .await
    }
}

#[tokio::test]
async fn observation_only_preserves_confirmed_inventory_and_explicit_empty_clears_it() -> Result<()>
{
    let w = Workbench::new()?;
    let learner = w.learner("synthetic-inventory", "real").await?;
    let session = w.select(&learner, true).await?;
    let modules = json!(["control", "motor", "distance", "light"]);
    w.report(
        &session,
        json!({"modules":modules,"source_type":"operator_confirmed"}),
    )
    .await?;
    let event = w.report(&session, json!({"modules":null,"source_type":"learner_report","report":"已经停下并亮绿灯，希望间隙更大"})).await?;
    let state = w.app.state()?;
    let fast = &state["session"]["state"];
    ensure!(fast["modules"] == modules);
    ensure!(fast["modules_source"] == "operator_confirmed");
    ensure!(fast["reported_result_source"] == "learner_report");
    ensure!(
        fast["field_evidence"]["reported_result"]["source_event_ids"] == json!([event["event_id"]])
    );
    ensure!(fast["field_evidence"]["reported_result"]["hardware_verified"] == false);
    let next_observation = "第二次仅调整停车距离，观察到间隙更大";
    w.report(
        &session,
        json!({"source_type":"learner_report","report":next_observation}),
    )
    .await?;
    w.report(
        &session,
        json!({"source_type":"operator_confirmed","report":""}),
    )
    .await?;
    let state = w.app.state()?;
    let attempts = state["session"]["state"]["attempts"].as_array().unwrap();
    ensure!(
        attempts.len() == 2,
        "manual observations must remain a bounded history"
    );
    ensure!(
        state["session"]["state"]["reported_result"] == next_observation,
        "an empty form may not erase the last observation"
    );
    w.report(
        &session,
        json!({"modules":[],"source_type":"operator_confirmed"}),
    )
    .await?;
    ensure!(w.app.state()?["session"]["state"]["modules"] == json!([]));
    w.app.close().await;
    Ok(())
}

#[tokio::test]
async fn finishing_preserves_history_without_implicitly_resuming_a_finished_activity() -> Result<()>
{
    let w = Workbench::new()?;
    let learner = w.learner("synthetic-returning", "real").await?;
    let session = w.select(&learner, true).await?;
    let observation = "第二次停车间隙更大，绿灯正常；下次移动墙后再测试";
    let event = w.report(&session, json!({"modules":["control","motor","distance","light"],"source_type":"operator_confirmed","report":observation})).await?;
    w.finish(&session).await?;
    let store = w.store("real")?;
    let memories = store.memories(s(&learner, "learner_id"))?;
    ensure!(memories.iter().any(|memory| {
        s(memory, "content").contains(observation)
            && memory["source_event_ids"]
                .as_array()
                .is_some_and(|ids| ids.contains(&event["event_id"]))
            && memory["source_type"] == "operator_confirmed"
    }));
    w.finish(&session).await?;
    ensure!(
        store.memories(s(&learner, "learner_id"))?.len() == memories.len(),
        "finish must be idempotent"
    );
    ensure!(store.session(s(&session, "session_id"))?["status"] == "finished");
    w.app.close().await;

    // A new service instance reads persisted memories; it cannot inherit in-memory state.
    let restarted = Workstation::new(w._dir.path().to_owned(), "SYNTHETIC_WORKFLOW_TOKEN".into())?;
    let router = router(restarted.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/api/sessions")
        .header("host", "127.0.0.1:8765")
        .header("cookie", "ai_tutor_local=SYNTHETIC_WORKFLOW_TOKEN")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(
            &json!({"learner_id":learner["learner_id"],"resume":true}),
        )?))?;
    let response = router.oneshot(request).await?;
    ensure!(response.status().is_success());
    let resumed: Value = serde_json::from_slice(&to_bytes(response.into_body(), 100_000).await?)?;
    ensure!(resumed["session_id"] != session["session_id"]);
    ensure!(resumed["state"]["modules"].is_null());
    ensure!(resumed["state"]["requires_inventory_confirmation"] == true);
    ensure!(resumed["recalled_memories"].as_array().unwrap().is_empty());
    ensure!(
        store
            .recall(s(&learner, "learner_id"), observation, 5)?
            .iter()
            .any(|m| s(m, "content").contains(observation)),
        "finishing preserves history; a fresh activity must not silently select it"
    );
    restarted.close().await;
    Ok(())
}

#[tokio::test]
async fn starting_new_activity_archives_old_and_finished_activity_cannot_be_revived_by_cancel()
-> Result<()> {
    let w = Workbench::new()?;
    let a = w.learner("synthetic-lifecycle-A", "real").await?;
    let b = w.learner("synthetic-lifecycle-B", "real").await?;
    let first = w.select(&a, true).await?;
    w.report(
        &first,
        json!({"source_type":"learner_report","report":"旧活动观察"}),
    )
    .await?;
    let second = w.select(&a, false).await?;
    ensure!(first["session_id"] != second["session_id"]);
    w.finish(&second).await?;
    let sid = s(&second, "session_id");
    let _ = w
        .request("POST", &format!("/api/sessions/{sid}/cancel"), json!({}))
        .await?;
    ensure!(
        w.store("real")?.session(sid)?["status"] == "finished",
        "cancel may not reopen finished work"
    );
    for (path, data) in [
        (
            format!("/api/activities/{}/events", s(&second, "activity_id")),
            json!({"source_type":"operator_confirmed","report":"must be rejected"}),
        ),
        (
            "/api/audio/record/start".to_owned(),
            json!({"session_id":sid}),
        ),
        (
            format!("/api/sessions/{sid}/turns"),
            json!({"text":"must be rejected"}),
        ),
    ] {
        let (status, _) = w.request("POST", &path, data).await?;
        ensure!(
            status == StatusCode::BAD_REQUEST,
            "finished write accepted: {path}"
        );
    }
    let other = w.select(&b, true).await?;
    ensure!(other["recalled_memories"].as_array().unwrap().is_empty());
    let next = w.select(&a, true).await?;
    ensure!(
        next["session_id"] != first["session_id"] && next["session_id"] != second["session_id"]
    );
    ensure!(next["state"]["goal"] == "");
    w.app.close().await;
    Ok(())
}

#[tokio::test]
async fn correcting_history_preserves_unrelated_current_evidence_and_rejects_stale_writer()
-> Result<()> {
    let w = Workbench::new()?;
    let learner = w.learner("synthetic-correction", "real").await?;
    let old_session = w.select(&learner, true).await?;
    let store = w.store("real")?;
    let lid = s(&learner, "learner_id");
    let old_event = store.event(
        lid,
        s(&old_session, "session_id"),
        "learner_report",
        &json!({"text":"以前的停车距离报告"}),
    )?;
    let candidate = json!({"kind":"reported_progress","content":"原先报告停车距离太小","scope":"current_activity","source_event_ids":[old_event]});
    let mid = store.add_memory(lid, &candidate, 0)?.unwrap();
    let current = w.select(&learner, false).await?;
    let modules = json!(["control", "motor", "distance", "light"]);
    let observation = "当前搭建正在比较两次试验";
    let current_event = w
        .report(
            &current,
            json!({"modules":modules,"source_type":"operator_confirmed","report":observation}),
        )
        .await?;
    w.api(
        "PATCH",
        &format!("/api/learners/{lid}/memories/{mid}"),
        json!({"content":"教师更正：旧报告的停车距离记录有误"}),
    )
    .await?;
    let state = w.app.state()?;
    ensure!(state["session"]["session_id"] == current["session_id"]);
    ensure!(state["session"]["state"]["modules"] == modules);
    ensure!(state["session"]["state"]["reported_result"] == observation);
    ensure!(
        store
            .source_events(lid, &[s(&current_event, "event_id").to_owned()])?
            .len()
            == 1
    );
    let active = store.memories(lid)?;
    ensure!(
        !active
            .iter()
            .any(|m| s(m, "content") == "原先报告停车距离太小")
    );
    ensure!(
        active
            .iter()
            .any(|m| s(m, "content") == "教师更正：旧报告的停车距离记录有误")
    );
    ensure!(
        store.add_memory(lid, &candidate, 0)?.is_none(),
        "old async consolidation must not resurrect superseded memory"
    );
    w.app.close().await;
    Ok(())
}

#[tokio::test]
async fn simulated_observation_is_only_recalled_in_its_own_namespace_and_profile() -> Result<()> {
    let w = Workbench::new()?;
    let simulation = w.learner("synthetic-simulation", "simulator").await?;
    let real = w.learner("synthetic-real", "real").await?;
    let session = w.select(&simulation, true).await?;
    w.report(
        &session,
        json!({"source_type":"simulated","report":"模拟试验独有进展：间隙增加"}),
    )
    .await?;
    w.finish(&session).await?;
    let next = w.select(&simulation, true).await?;
    ensure!(next["recalled_memories"].as_array().unwrap().is_empty());
    ensure!(
        w.store("simulator")?
            .recall(s(&simulation, "learner_id"), "模拟试验独有进展", 5)?
            .iter()
            .any(
                |m| s(m, "content").contains("模拟试验独有进展") && m["source_type"] == "simulated"
            )
    );
    let real_session = w.select(&real, true).await?;
    ensure!(
        real_session["recalled_memories"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (status, _) = w
        .request(
            "POST",
            &format!("/api/activities/{}/events", s(&real_session, "activity_id")),
            json!({"source_type":"simulated","report":"reject simulation in real profile"}),
        )
        .await?;
    ensure!(status == StatusCode::BAD_REQUEST);
    w.app.close().await;
    Ok(())
}
