//! Explicit selection of one finished activity. Reading the catalog and retrying
//! an already committed selection never cancel the current teaching/audio work.
use super::*;

#[derive(serde::Deserialize)]
pub(super) struct CatalogQuery {
    before: Option<i64>,
    limit: Option<usize>,
}

pub(super) async fn catalog(
    State(app): State<Arc<Workstation>>,
    Path(lid): Path<String>,
    Query(query): Query<CatalogQuery>,
) -> Response {
    match app.activity_catalog(&lid, query.before, query.limit.unwrap_or(20)) {
        Ok(page) => success(page),
        Err(error) => api_error(error),
    }
}

pub(super) async fn comparison_history(
    State(app): State<Arc<Workstation>>,
    Path((lid, sid)): Path<(String, String)>,
    Query(query): Query<CatalogQuery>,
) -> Response {
    let result = (|| {
        let namespace = app.owner(&lid)?;
        app.stores.lock().unwrap()[&namespace].comparisons_page(
            &lid,
            &sid,
            query.before,
            query.limit.unwrap_or(10),
        )
    })();
    match result {
        Ok(page) => success(page),
        Err(error) => api_error(error),
    }
}

struct ContinueIntent<'a> {
    nonce: &'a str,
    current_sid: &'a str,
    generation: i64,
}
impl<'a> ContinueIntent<'a> {
    fn parse(data: &'a Value) -> Result<Self> {
        allowed_keys(
            data,
            &[
                "client_submission_id",
                "expected_current_session_id",
                "expected_generation",
            ],
        )?;
        let nonce = text(data, "client_submission_id", 1, 96)?;
        ensure!(
            nonce
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "续做关联标识无效"
        );
        let current_sid = text(data, "expected_current_session_id", 1, 80)?;
        let generation =
            recording::expected_generation(data)?.ok_or_else(|| anyhow!("续做缺少当前会话代次"))?;
        Ok(Self {
            nonce,
            current_sid,
            generation,
        })
    }
}

impl Workstation {
    pub(super) fn activity_catalog(
        &self,
        lid: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Value> {
        ensure!(!lid.is_empty() && lid.len() <= 80, "档案编号无效");
        let namespace = self.owner(lid)?;
        self.stores.lock().unwrap()[&namespace].session_catalog(lid, before, limit)
    }

    pub(super) async fn continue_activity(
        &self,
        lid: &str,
        source_sid: &str,
        data: &Value,
    ) -> Result<Value> {
        ensure!(
            !lid.is_empty() && lid.len() <= 80 && !source_sid.is_empty() && source_sid.len() <= 80,
            "档案或旧活动编号无效"
        );
        let intent = ContinueIntent::parse(data)?;
        // Match existing selection's lock order. Unlike its legacy path, no
        // motion/audio/foreground invalidation happens before full validation.
        let _camera_guard = self.camera_control.lock().await;
        let _control = self.control.lock().await;
        ensure!(!self.shutting_down.load(Ordering::SeqCst), "工作台正在关闭");
        let namespace = self.owner(lid)?;
        let current = self.current.lock().unwrap().clone();
        let replay = self.stores.lock().unwrap()[&namespace].continuation_replay(
            lid,
            source_sid,
            intent.nonce,
            intent.current_sid,
            intent.generation,
        )?;
        if let Some(response) = replay {
            ensure!(
                current.as_ref() == Some(&(namespace, s(&response, "session_id").to_owned())),
                "当前活动已变化，不能通过旧续做请求重新选择活动"
            );
            return Ok(response);
        }
        ensure!(
            current.as_ref() == Some(&(namespace.clone(), intent.current_sid.to_owned())),
            "当前活动已变化，请重新选择要继续的旧活动"
        );

        let _foreground = self.memory_worker.foreground();
        self.ptz.stop();
        self.audio.reserve_speech_epoch();
        self.audio.discard_recording();
        self.ptz.stop_and_wait().await;
        self.cancel().await?;
        *self.recording.lock().unwrap() = None;
        ensure!(!self.shutting_down.load(Ordering::SeqCst), "工作台正在关闭");
        let response = {
            let stores = self.stores.lock().unwrap();
            let store = &stores[&namespace];
            let after_cancel = store.session(intent.current_sid)?["generation"]
                .as_i64()
                .ok_or_else(|| anyhow!("当前会话代次无效"))?;
            store.continue_finished_session(
                lid,
                source_sid,
                intent.nonce,
                intent.current_sid,
                intent.generation,
                after_cancel,
            )?
        };
        // Commit succeeded. No fallible operation follows before publishing the
        // new selection, so a returned validation error cannot hide a new one.
        *self.current.lock().unwrap() = Some((namespace, s(&response, "session_id").to_owned()));
        self.notify("state.changed");
        Ok(response)
    }
}

#[cfg(test)]
#[path = "activity_history_tests.rs"]
mod tests;
