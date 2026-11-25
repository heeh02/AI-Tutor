use gloo_net::http::RequestBuilder;
use gloo_timers::callback::Timeout;
use serde_json::Value;
use std::future::Future;
use web_sys::{AbortController, RequestCache, RequestCredentials};

pub type ApiResult = Result<Value, String>;
fn request_timeout_ms(path: &str, method: &str) -> u32 {
    if path.contains("/voice-loop/")
        || (matches!(path, "/api/audio/status" | "/api/camera/pose") && method == "GET")
    {
        1500
    } else if path.ends_with("/record/stop") {
        90_000
    } else if path == "/api/audio/local-speech/stop" {
        40_000
    } else if path == "/api/audio/local-asr/stop" {
        15_000
    } else if path == "/api/audio/final-asr/stop" {
        50_000
    } else {
        20_000
    }
}

pub async fn api(path: &str, method: &str, body: Option<Value>) -> ApiResult {
    let (status, data) = request(path, method, body).await?;
    if !(200..300).contains(&status) {
        return Err(error_detail(&data));
    }
    Ok(data)
}

fn error_detail(data: &Value) -> String {
    data["detail"]
        .as_str()
        .or_else(|| data["error"].as_str())
        .unwrap_or("操作未完成，请检查当前设置")
        .to_owned()
}

/// Only continuation needs to distinguish a definite rejection from a lost
/// result: its server contract guarantees no new session on a 4xx response.
pub enum ContinueResult {
    Applied(Value),
    Rejected(String),
    Unknown(String),
}
fn continuation_result(result: Result<(u16, Value), String>) -> ContinueResult {
    match result {
        Ok((status, data)) if (200..300).contains(&status) => ContinueResult::Applied(data),
        Ok((status, data)) if (400..500).contains(&status) => {
            ContinueResult::Rejected(error_detail(&data))
        }
        Ok((_, data)) => ContinueResult::Unknown(error_detail(&data)),
        Err(error) => ContinueResult::Unknown(error),
    }
}
pub async fn continue_activity(path: &str, body: Value) -> ContinueResult {
    continuation_result(request(path, "POST", Some(body)).await)
}

async fn request(path: &str, method: &str, body: Option<Value>) -> Result<(u16, Value), String> {
    let controller = AbortController::new().map_err(|_| "无法建立本机请求".to_owned())?;
    let abort = controller.clone();
    let timeout = Timeout::new(request_timeout_ms(path, method), move || abort.abort());
    let request = RequestBuilder::new(path)
        .method(method.parse().map_err(|_| "请求方式无效".to_owned())?)
        .credentials(RequestCredentials::SameOrigin)
        .cache(RequestCache::NoStore)
        .abort_signal(Some(&controller.signal()));
    let response = if let Some(body) = body {
        request
            .json(&body)
            .map_err(|_| "无法组织请求数据".to_owned())?
            .send()
            .await
    } else {
        request.send().await
    }
    .map_err(|_| "本机服务没有响应，请检查应用是否仍在运行".to_owned())?;
    let status = response.status();
    if status == 204 {
        return Ok((status, Value::Null));
    }
    let data = response
        .json::<Value>()
        .await
        .map_err(|_| format!("服务返回了无法读取的结果（{status}）"))?;
    drop(timeout);
    Ok((status, data))
}

#[derive(Clone)]
pub struct Call {
    pub path: String,
    pub method: &'static str,
    pub body: Option<Value>,
}
impl Call {
    pub fn post(path: impl Into<String>, body: Value) -> Self {
        Self {
            path: path.into(),
            method: "POST",
            body: Some(body),
        }
    }
    pub fn delete(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            method: "DELETE",
            body: None,
        }
    }
    pub fn patch(path: impl Into<String>, body: Value) -> Self {
        Self {
            path: path.into(),
            method: "PATCH",
            body: Some(body),
        }
    }
}
pub async fn calls(calls: Vec<Call>) -> ApiResult {
    calls_with(calls, |call| async move {
        api(&call.path, call.method, call.body).await
    })
    .await
}

async fn calls_with<F, Fut>(calls: Vec<Call>, mut execute: F) -> ApiResult
where
    F: FnMut(Call) -> Fut,
    Fut: Future<Output = ApiResult>,
{
    let mut result = Value::Null;
    for call in calls {
        result = execute(call).await?;
    }
    Ok(result)
}

pub enum ActivityTransition {
    Finish,
    NewActivity,
}

/// Saving a draft is part of the transition: a failed report must prevent
/// finishing or replacing the activity, rather than silently dropping it.
pub fn activity_transition_calls(
    sid: &str,
    aid: &str,
    lid: &str,
    finished: bool,
    payload: Option<Value>,
    transition: ActivityTransition,
) -> Vec<Call> {
    let mut items = vec![];
    if !finished {
        items.push(Call::post(
            format!("/api/sessions/{sid}/cancel"),
            serde_json::json!({}),
        ));
        if let Some(payload) = payload {
            items.push(Call::post(format!("/api/activities/{aid}/events"), payload));
        }
    }
    match transition {
        ActivityTransition::Finish if !finished => items.push(Call::post(
            format!("/api/sessions/{sid}/finish"),
            serde_json::json!({}),
        )),
        ActivityTransition::NewActivity => items.push(Call::post(
            "/api/sessions",
            serde_json::json!({"learner_id":lid,"resume":false}),
        )),
        _ => {}
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        cell::RefCell,
        future::ready,
        task::{Context, Poll, Waker},
    };

    #[test]
    fn continuation_distinguishes_rejection_from_an_unknown_commit() {
        assert!(matches!(
            continuation_result(Ok((409, serde_json::json!({"detail":"stale"})))),
            ContinueResult::Rejected(_)
        ));
        assert!(matches!(
            continuation_result(Ok((500, Value::Null))),
            ContinueResult::Unknown(_)
        ));
        assert!(matches!(
            continuation_result(Err("timeout".into())),
            ContinueResult::Unknown(_)
        ));
        assert!(matches!(
            continuation_result(Ok((200, serde_json::json!({"replayed":true})))),
            ContinueResult::Applied(_)
        ));
    }

    #[test]
    fn caption_status_has_a_short_deadline_without_shortening_audio_actions() {
        assert_eq!(request_timeout_ms("/api/audio/status", "GET"), 1500);
        assert_eq!(request_timeout_ms("/api/audio/status", "POST"), 20_000);
        assert_eq!(request_timeout_ms("/api/state", "GET"), 20_000);
        assert_eq!(request_timeout_ms("/api/audio/record/stop", "POST"), 90_000);
        assert_eq!(
            request_timeout_ms("/api/audio/local-speech/stop", "POST"),
            40_000
        );
        assert_eq!(
            request_timeout_ms("/api/audio/local-asr/stop", "POST"),
            15_000
        );
    }
    #[test]
    fn finish_and_new_activity_save_the_last_observation_first() {
        for action in [ActivityTransition::Finish, ActivityTransition::NewActivity] {
            let calls = activity_transition_calls(
                "s",
                "a",
                "l",
                false,
                Some(json!({"report":"第二次停车间隙更大"})),
                action,
            );
            assert_eq!(calls.len(), 3);
            assert_eq!(calls[0].path, "/api/sessions/s/cancel");
            assert_eq!(calls[1].path, "/api/activities/a/events");
            assert_eq!(
                calls[1].body.as_ref().unwrap()["report"],
                "第二次停车间隙更大"
            );
            assert!(calls[2].path.ends_with("/finish") || calls[2].path == "/api/sessions");
        }
    }
    #[test]
    fn a_report_failure_never_reaches_the_finish_request() {
        let plan = activity_transition_calls(
            "s",
            "a",
            "l",
            false,
            Some(json!({"report":"最后观察"})),
            ActivityTransition::Finish,
        );
        let reached = RefCell::new(Vec::new());
        let mut future = Box::pin(calls_with(plan, |call| {
            let failed = call.path.ends_with("/events");
            reached.borrow_mut().push(call.path);
            ready(if failed {
                Err("保存失败".into())
            } else {
                Ok(json!({"ok":true}))
            })
        }));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            future.as_mut().poll(&mut context),
            Poll::Ready(Err(_))
        ));
        assert_eq!(
            *reached.borrow(),
            vec!["/api/sessions/s/cancel", "/api/activities/a/events"]
        );
    }
    #[test]
    fn an_ended_activity_is_never_cancelled_or_changed_when_starting_again() {
        assert!(
            activity_transition_calls("s", "a", "l", true, None, ActivityTransition::Finish)
                .is_empty()
        );
        let calls = activity_transition_calls(
            "s",
            "a",
            "l",
            true,
            Some(json!({"report":"stale"})),
            ActivityTransition::NewActivity,
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].path, "/api/sessions");
        assert_eq!(
            calls[0].body.as_ref().unwrap(),
            &json!({"learner_id":"l","resume":false})
        );
    }
}
