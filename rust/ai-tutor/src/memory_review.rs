//! One explicitly requested, source-bound memory review. The existing worker
//! owns inference, budgets and foreground preemption; no automatic promotion.
use super::*;
use crate::memory_worker::{MemoryConsolidator, MemoryWorkerMode, MemoryWorkerPolicy};
use crate::store::MemoryJobLimits;

pub(super) struct ReviewRun {
    namespace: String,
    learner: String,
    session: String,
    job: String,
    task: JoinHandle<()>,
    outcome: Arc<Mutex<Option<String>>>,
    cancellation: watch::Sender<bool>,
}

impl Workstation {
    pub(super) fn memory_review_snapshot(&self, lid: &str, sid: &str) -> Result<Value> {
        let (namespace, session) = self.active(sid)?;
        ensure!(session["learner_id"] == lid, "记忆整理档案与会话不一致");
        let mut value = self.stores.lock().unwrap()[&namespace].memory_review_snapshot(lid, sid)?;
        value["provider_available"] = self.provider.status()["available"].clone();
        value["worker_busy"] = json!(!self.memory_worker.is_idle());
        value["review_running"] = json!(false);
        value["review_outcome"] = Value::Null;
        if let Some(run) = self.manual_memory_review.lock().unwrap().as_ref() {
            if run.namespace == namespace
                && run.learner == lid
                && run.session == sid
                && value["job"]["job_id"] == run.job
            {
                value["review_running"] = json!(!run.task.is_finished());
                value["review_outcome"] = json!(*run.outcome.lock().unwrap());
            }
        }
        Ok(value)
    }

    pub(super) async fn start_memory_review(
        self: &Arc<Self>,
        lid: &str,
        sid: &str,
        data: &Value,
    ) -> Result<Value> {
        ensure!(
            self.provider.status()["available"] == true,
            "后台记忆服务尚未就绪"
        );
        self.start_memory_review_with(lid, sid, data, self.provider.clone())
            .await
    }

    async fn start_memory_review_with<P: MemoryConsolidator + 'static>(
        self: &Arc<Self>,
        lid: &str,
        sid: &str,
        data: &Value,
        provider: Arc<P>,
    ) -> Result<Value> {
        allowed_keys(
            data,
            &[
                "expected_generation",
                "expected_epoch",
                "expected_state_revision",
            ],
        )?;
        let _control = self.control.lock().await;
        ensure!(!self.shutting_down.load(Ordering::SeqCst), "工作台正在关闭");
        let (namespace, session) = self.writable(sid)?;
        ensure!(session["learner_id"] == lid, "记忆整理档案与会话不一致");
        {
            let stores = self.stores.lock().unwrap();
            ensure!(
                data["expected_generation"].as_i64() == session["generation"].as_i64()
                    && data["expected_state_revision"].as_i64()
                        == session["state_revision"].as_i64()
                    && data["expected_epoch"].as_i64()
                        == stores[&namespace].learner(lid)?["epoch"].as_i64(),
                "学习记录已变化，请刷新后再整理"
            );
        }
        let existing = self
            .manual_memory_review
            .lock()
            .unwrap()
            .as_ref()
            .map(|run| {
                (
                    !run.task.is_finished(),
                    run.namespace == namespace && run.learner == lid && run.session == sid,
                )
            });
        if let Some((true, same_owner)) = existing {
            ensure!(same_owner, "上一份记忆整理仍在收尾，请稍后继续");
            return self.memory_review_snapshot(lid, sid);
        }
        let audio = self.audio.status();
        ensure!(
            self.memory_worker.is_idle()
                && audio["recording"] != true
                && audio["capture_pending_result"] != true
                && !matches!(
                    s(&audio, "state"),
                    "recording" | "transcribing" | "speaking" | "synthesizing"
                ),
            "请先完成当前对话或录音，再整理学习线索"
        );
        let limits = MemoryJobLimits::default();
        let snapshot = self.stores.lock().unwrap()[&namespace].prepare_memory_review(
            lid,
            sid,
            &limits,
            chrono::Utc::now().timestamp_millis(),
        )?;
        // A leftover lease is recovered only by the target-aware claim when it
        // actually expired; attempts and all other learners' leases are retained.
        if !matches!(s(&snapshot["job"], "status"), "pending" | "leased") {
            return self.memory_review_snapshot(lid, sid);
        }
        let job = s(&snapshot["job"], "job_id").to_owned();
        let policy = MemoryWorkerPolicy {
            mode: if namespace == "simulator" {
                MemoryWorkerMode::OptInSimulator
            } else {
                MemoryWorkerMode::OptInReal
            },
            limits,
        };
        let outcome = Arc::new(Mutex::new(None));
        let (cancellation, cancelled) = watch::channel(false);
        let result_slot = outcome.clone();
        let app = self.clone();
        let target_namespace = namespace.clone();
        let target_job = job.clone();
        let task = tokio::spawn(async move {
            let result = app
                .memory_worker
                .run_job_cancellable(
                    &app.stores,
                    provider.as_ref(),
                    &target_namespace,
                    &policy,
                    &target_job,
                    cancelled,
                )
                .await;
            let status = result
                .as_ref()
                .ok()
                .map(|value| s(value, "status"))
                .unwrap_or("failed")
                .to_owned();
            *result_slot.lock().unwrap() = Some(status.clone());
            app.metrics.lock().unwrap()["memory_review"] =
                json!({"status":status,"automatic_apply":false});
            app.notify("memory.review.updated");
        });
        *self.manual_memory_review.lock().unwrap() = Some(ReviewRun {
            namespace,
            learner: lid.into(),
            session: sid.into(),
            job,
            task,
            outcome,
            cancellation,
        });
        // Shutdown invalidates before waiting for workstation control. It may
        // have raced the admission check while this slot was not registered.
        if self.shutting_down.load(Ordering::SeqCst) {
            self.cancel_queued_memory_review();
        }
        self.memory_review_snapshot(lid, sid)
    }

    pub(super) async fn cancel_memory_review(&self, lid: &str, sid: &str) -> Result<Value> {
        let _control = self.control.lock().await;
        let (namespace, session) = self.active(sid)?;
        ensure!(session["learner_id"] == lid, "记忆整理档案与会话不一致");
        let cancellation = self
            .manual_memory_review
            .lock()
            .unwrap()
            .as_ref()
            .filter(|run| {
                run.namespace == namespace
                    && run.learner == lid
                    && run.session == sid
                    && !run.task.is_finished()
            })
            .map(|run| run.cancellation.clone());
        if let Some(cancel) = cancellation {
            let _ = cancel.send(true);
        }
        self.memory_review_snapshot(lid, sid)
    }

    pub(super) async fn decide_memory_review(
        &self,
        lid: &str,
        sid: &str,
        candidate: &str,
        decision: &str,
    ) -> Result<Value> {
        let _control = self.control.lock().await;
        ensure!(!self.shutting_down.load(Ordering::SeqCst), "工作台正在关闭");
        let (namespace, session) = self.writable(sid)?;
        ensure!(session["learner_id"] == lid, "记忆整理档案与会话不一致");
        // Do not call cancel(): its generation change would invalidate the very
        // snapshot the user is reviewing. Store rechecks versions transactionally.
        let result = self.stores.lock().unwrap()[&namespace].review_memory_candidate(
            lid,
            sid,
            candidate,
            decision,
            chrono::Utc::now().timestamp_millis(),
        )?;
        self.notify("memory.updated");
        Ok(result)
    }

    pub(super) fn cancel_queued_memory_review(&self) {
        if let Some(run) = self.manual_memory_review.lock().unwrap().as_ref() {
            let _ = run.cancellation.send(true);
        }
    }

    pub(super) async fn close_memory_review(&self) {
        let run = self.manual_memory_review.lock().unwrap().take();
        if let Some(mut run) = run {
            // begin_shutdown already preempted the worker and provider.close
            // completed owned-process cleanup before this final task join.
            if tokio::time::timeout(Duration::from_secs(5), &mut run.task)
                .await
                .is_err()
            {
                run.task.abort();
                let _ = run.task.await;
                self.error("记忆整理任务未在关闭期限内收尾".into());
            }
        }
    }
}

#[cfg(test)]
#[path = "memory_review_api_tests.rs"]
mod tests;
