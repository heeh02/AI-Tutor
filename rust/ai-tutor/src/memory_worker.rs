//! One budgeted shadow-consolidation cycle. Scheduling and enabling real-data
//! processing belong to the application; no background loop starts on creation.
use crate::{
    kimi::{KimiProvider, build_prompt, memory_contract},
    store::{MemoryJobLimits, Store, s, uid},
};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;

pub type ProviderFuture<'a> = Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;
pub type CancelFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// A fake provider can exercise the worker without credentials or subprocesses.
/// Send `started=true` only after the actual provider operation has started.
pub trait MemoryConsolidator: Send + Sync {
    fn consolidate<'a>(
        &'a self,
        request: &'a Value,
        started: watch::Sender<bool>,
    ) -> ProviderFuture<'a>;
    fn cancel<'a>(&'a self, request_id: &'a str) -> CancelFuture<'a>;
}
impl MemoryConsolidator for KimiProvider {
    fn consolidate<'a>(
        &'a self,
        request: &'a Value,
        started: watch::Sender<bool>,
    ) -> ProviderFuture<'a> {
        Box::pin(KimiProvider::consolidate(self, request, started))
    }
    fn cancel<'a>(&'a self, request_id: &'a str) -> CancelFuture<'a> {
        Box::pin(KimiProvider::cancel(self, request_id))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemoryWorkerMode {
    #[default]
    Disabled,
    /// Only explicit synthetic_fixture events in the simulator database.
    SyntheticOnly,
    /// Must be selected deliberately by the application's learner-data policy.
    OptInReal,
    /// An explicit per-session review request in the simulator. Unlike a lab
    /// probe, ordinary user-authored simulator records need no fixture marker.
    OptInSimulator,
}
#[derive(Debug, Clone, Default)]
pub struct MemoryWorkerPolicy {
    pub mode: MemoryWorkerMode,
    pub limits: MemoryJobLimits,
}

#[derive(Default)]
struct Control {
    foreground: Arc<AtomicUsize>,
    active: Option<(String, watch::Sender<bool>)>,
}
#[derive(Default)]
pub struct MemoryWorker {
    control: Arc<Mutex<Control>>,
}

/// Keep this guard through the whole foreground operation, including delivery.
/// It owns its state, so it can move into a task or a pending delivery record.
pub struct ForegroundGuard {
    foreground: Arc<AtomicUsize>,
}
impl Drop for ForegroundGuard {
    fn drop(&mut self) {
        self.foreground.fetch_sub(1, Ordering::SeqCst);
    }
}
struct ActiveCycle {
    control: Arc<Mutex<Control>>,
    id: String,
}
impl Drop for ActiveCycle {
    fn drop(&mut self) {
        let mut control = self.control.lock().unwrap();
        if control
            .active
            .as_ref()
            .is_some_and(|(id, _)| id == &self.id)
        {
            control.active = None;
        }
    }
}

fn clock_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
async fn cancellation_requested(
    local: &mut watch::Receiver<bool>,
    external: &mut Option<watch::Receiver<bool>>,
) {
    if let Some(external) = external {
        tokio::select! {
            _ = local.changed() => {},
            _ = external.wait_for(|cancel| *cancel) => {},
        }
    } else {
        let _ = local.changed().await;
    }
}
fn externally_cancelled(cancel: &Option<watch::Receiver<bool>>) -> bool {
    cancel.as_ref().is_some_and(|cancel| *cancel.borrow())
}
fn with_store<T>(
    stores: &Mutex<HashMap<String, Store>>,
    namespace: &str,
    action: impl FnOnce(&Store) -> Result<T>,
) -> Result<T> {
    let stores = stores.lock().unwrap();
    action(
        stores
            .get(namespace)
            .ok_or_else(|| anyhow!("记忆数据空间不存在"))?,
    )
}

impl MemoryWorker {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn is_idle(&self) -> bool {
        let control = self.control.lock().unwrap();
        control.foreground.load(Ordering::SeqCst) == 0 && control.active.is_none()
    }
    pub fn foreground(&self) -> ForegroundGuard {
        let control = self.control.lock().unwrap();
        control.foreground.fetch_add(1, Ordering::SeqCst);
        if let Some((_, cancel)) = &control.active {
            let _ = cancel.send(true);
        }
        ForegroundGuard {
            foreground: control.foreground.clone(),
        }
    }
    pub fn preempt(&self) {
        if let Some((_, cancel)) = &self.control.lock().unwrap().active {
            let _ = cancel.send(true);
        }
    }

    /// Claims and settles at most one job. All Store access ends before await.
    /// The result contains status/IDs/counts, never a model response or report.
    pub async fn run_once<P: MemoryConsolidator + ?Sized>(
        &self,
        stores: &Mutex<HashMap<String, Store>>,
        provider: &P,
        namespace: &str,
        policy: &MemoryWorkerPolicy,
    ) -> Result<Value> {
        self.run_selected(stores, provider, namespace, policy, None, None)
            .await
    }

    /// A user-requested review must never claim another learner's queued work.
    pub async fn run_job<P: MemoryConsolidator + ?Sized>(
        &self,
        stores: &Mutex<HashMap<String, Store>>,
        provider: &P,
        namespace: &str,
        policy: &MemoryWorkerPolicy,
        job_id: &str,
    ) -> Result<Value> {
        self.run_selected(stores, provider, namespace, policy, Some(job_id), None)
            .await
    }

    /// The caller may cancel before the spawned future has first been polled.
    pub async fn run_job_cancellable<P: MemoryConsolidator + ?Sized>(
        &self,
        stores: &Mutex<HashMap<String, Store>>,
        provider: &P,
        namespace: &str,
        policy: &MemoryWorkerPolicy,
        job_id: &str,
        cancellation: watch::Receiver<bool>,
    ) -> Result<Value> {
        self.run_selected(
            stores,
            provider,
            namespace,
            policy,
            Some(job_id),
            Some(cancellation),
        )
        .await
    }

    async fn run_selected<P: MemoryConsolidator + ?Sized>(
        &self,
        stores: &Mutex<HashMap<String, Store>>,
        provider: &P,
        namespace: &str,
        policy: &MemoryWorkerPolicy,
        job_id: Option<&str>,
        mut external_cancel: Option<watch::Receiver<bool>>,
    ) -> Result<Value> {
        if policy.mode == MemoryWorkerMode::Disabled {
            return Ok(json!({"status":"disabled"}));
        }
        ensure!(
            match policy.mode {
                MemoryWorkerMode::SyntheticOnly => namespace == "simulator",
                MemoryWorkerMode::OptInReal => namespace == "real",
                MemoryWorkerMode::OptInSimulator => namespace == "simulator" && job_id.is_some(),
                MemoryWorkerMode::Disabled => false,
            },
            "后台记忆策略不允许此数据空间"
        );
        ensure!(
            policy.limits.lease_seconds >= 100,
            "记忆worker租约必须覆盖90秒调用和取消清理"
        );
        let worker = uid("memory_worker");
        let (cancel, mut cancellation) = watch::channel(false);
        {
            let mut control = self.control.lock().unwrap();
            if control.foreground.load(Ordering::SeqCst) > 0 || control.active.is_some() {
                return Ok(json!({"status":"foreground_or_worker_busy"}));
            }
            control.active = Some((worker.clone(), cancel));
        }
        let _active = ActiveCycle {
            control: self.control.clone(),
            id: worker.clone(),
        };
        if *cancellation.borrow() || externally_cancelled(&external_cancel) {
            return Ok(json!({"status":"preempted_before_claim"}));
        }
        let Some(claim) = with_store(stores, namespace, |store| match job_id {
            Some(id) => store.claim_memory_job_for(&worker, &policy.limits, clock_ms(), id),
            None => store.claim_memory_job(&worker, &policy.limits, clock_ms()),
        })?
        else {
            return Ok(json!({"status":"no_eligible_job"}));
        };
        let receipt = &claim["receipt"];
        let job = &claim["job"];
        let request = match build_request(&claim, policy.mode) {
            Ok(request) => request,
            Err(_) => return settle(stores, namespace, job, receipt, false, false),
        };
        if *cancellation.borrow() || externally_cancelled(&external_cancel) {
            return settle(stores, namespace, job, receipt, true, false);
        }
        let (started_tx, mut started_rx) = watch::channel(false);
        let mut started_observed = false;
        let mut provider_started = false;
        let mut preempted = false;
        let result = {
            let call = provider.consolidate(&request, started_tx);
            tokio::pin!(call);
            let deadline = tokio::time::sleep(Duration::from_secs(90));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    biased;
                    _=cancellation_requested(&mut cancellation, &mut external_cancel)=>{
                        preempted=true;
                        let _=provider.cancel(s(&request,"request_id")).await;
                        // Give the provider its normal cancellation/reap path.
                        // Dropping an unresponsive call then releases its owned
                        // process guard before this cycle can be retried.
                        let actually_started=*started_rx.borrow();
                        if actually_started{
                            let _=tokio::time::timeout(Duration::from_secs(2),&mut call).await;
                        }
                        break Err(anyhow!("后台记忆已让出前台"));
                    }
                    _=&mut deadline=>{
                        let _=provider.cancel(s(&request,"request_id")).await;
                        let actually_started=*started_rx.borrow();
                        if actually_started{
                            let _=tokio::time::timeout(Duration::from_secs(2),&mut call).await;
                        }
                        break Err(anyhow!("后台记忆超过90秒"));
                    }
                    _=started_rx.changed(), if !started_observed=>{
                        started_observed=true;
                        provider_started=*started_rx.borrow();
                        if provider_started && with_store(stores,namespace,|store|store.mark_memory_job_started(receipt,clock_ms())).is_err(){
                            let _=provider.cancel(s(&request,"request_id")).await;
                            let _=tokio::time::timeout(Duration::from_secs(2),&mut call).await;
                            break Err(anyhow!("后台记忆来源或租约已失效"));
                        }
                    }
                    result=&mut call=>break result,
                }
            }
        };
        // A fast provider may return in the same poll that sends its start receipt.
        if !started_observed && *started_rx.borrow() {
            provider_started = true;
            let _ = with_store(stores, namespace, |store| {
                store.mark_memory_job_started(receipt, clock_ms())
            });
        }
        // Commit and foreground entry share this short lock. A foreground
        // arriving before commit always invalidates this cycle's delivery.
        let control = self.control.lock().unwrap();
        if preempted
            || control.foreground.load(Ordering::SeqCst) > 0
            || *cancellation.borrow()
            || externally_cancelled(&external_cancel)
        {
            return settle(stores, namespace, job, receipt, true, provider_started);
        }
        match result.and_then(|response| {
            let candidates = memory_contract::validate_response(&response)?;
            with_store(stores, namespace, |store| {
                store.stage_memory_candidates(receipt, candidates, clock_ms())
            })
        }) {
            Ok(candidates) => Ok(
                json!({"status":"shadow_staged","job_id":job["job_id"],"candidate_count":candidates.len(),"provider_started":provider_started,"applied":false}),
            ),
            Err(_) => settle(stores, namespace, job, receipt, false, provider_started),
        }
    }
}

fn settle(
    stores: &Mutex<HashMap<String, Store>>,
    namespace: &str,
    job: &Value,
    receipt: &Value,
    preempted: bool,
    started: bool,
) -> Result<Value> {
    with_store(stores, namespace, |store| {
        let released = if preempted {
            store.preempt_memory_job(receipt, clock_ms())
        } else {
            store.fail_memory_job(receipt, clock_ms())
        };
        let status = store.memory_job_status(s(job, "learner_id"), s(job, "job_id"))?;
        let outcome = if released.is_ok() {
            if preempted { "preempted" } else { "failed" }
        } else if status["status"] == "invalidated" {
            "invalidated"
        } else {
            "stale_receipt"
        };
        Ok(
            json!({"status":outcome,"job_id":job["job_id"],"job_status":status["status"],"provider_started":started,"applied":false}),
        )
    })
}

fn build_request(claim: &Value, mode: MemoryWorkerMode) -> Result<Value> {
    let job = &claim["job"];
    ensure!(
        job["prompt_version"] == memory_contract::PROMPT_VERSION,
        "记忆提示版本不受支持"
    );
    let sources = claim["sources"]
        .as_array()
        .ok_or_else(|| anyhow!("记忆来源缺失"))?;
    if mode == MemoryWorkerMode::SyntheticOnly {
        ensure!(
            sources
                .iter()
                .all(|source| source["content"]["synthetic_fixture"] == true),
            "未明确标记的来源不得进入合成模型检验"
        );
    }
    let sources:Vec<_>=sources.iter().map(|source|json!({"event_id":source["event_id"],"event_version":source["event_version"],"event_kind":source["event_kind"],"source_type":source["source_type"],"content":source["content"]})).collect();
    let request = json!({"request_id":uid("memory_request"),"learner_id":job["learner_id"],"session_id":job["session_id"],
        "_memory_job_id":job["job_id"],"_memory_attempt":job["attempts"],
        "prompt_version":memory_contract::PROMPT_VERSION,"context":{"namespace":job["namespace"],"sources":sources,"output_status":"shadow_only"}});
    // Preflight before a provider starts. Evidence is never silently truncated.
    build_prompt(
        memory_contract::POLICY,
        &memory_contract::schema(),
        &request,
    )?;
    Ok(request)
}

/// Explicit diagnostic entry point: hardcoded synthetic evidence, a fresh
/// temporary simulator database and one reserved call. Never loads a learner DB.
/// The caller must deliberately invoke this with its already verified provider.
pub async fn run_synthetic_probe<P: MemoryConsolidator + ?Sized>(
    provider: &P,
    language: &str,
) -> Result<Value> {
    ensure!(
        ["zh", "en"].contains(&language),
        "合成记忆检验语言必须为zh或en"
    );
    let directory = std::env::temp_dir().join(uid("ai-tutor-memory-probe"));
    std::fs::create_dir(&directory)?;
    let _cleanup = SyntheticProbeDirectory(directory.clone());
    let store = Store::open(directory.join("synthetic.sqlite3"), "simulator")?;
    let learner = store.create_learner("SYNTHETIC_MEMORY_PROBE", language, "guided")?;
    let lid = s(&learner, "learner_id");
    let session = store.create_session(lid)?;
    let sid = s(&session, "session_id");
    let event=store.record_education_event(lid,sid,"simulated","observation",&json!({
        "text":if language=="zh"{"合成检验记录：按下开关后，绿色指示灯亮起。这不是实际儿童或设备观察。"}else{"Synthetic fixture report: the green light turned on after the switch was pressed. This is not an actual child or device observation."},
        "synthetic_fixture":true,
    }))?;
    let policy = MemoryWorkerPolicy {
        mode: MemoryWorkerMode::SyntheticOnly,
        limits: MemoryJobLimits {
            max_pending: 1,
            max_attempts: 1,
            daily_reservations: 1,
            session_reservations: 1,
            ..Default::default()
        },
    };
    store.enqueue_memory_job(
        lid,
        sid,
        &[s(&event, "event_id").to_owned()],
        memory_contract::PROMPT_VERSION,
        &policy.limits,
        clock_ms(),
    )?;
    let stores = Mutex::new(HashMap::from([("simulator".into(), store)]));
    let result = MemoryWorker::new()
        .run_once(&stores, provider, "simulator", &policy)
        .await?;
    Ok(
        json!({"probe":"synthetic_educational_memory","language":language,"prompt_version":memory_contract::PROMPT_VERSION,"result":result,"real_learner_data":false,"applied":false,"raw_response_retained":false}),
    )
}

struct SyntheticProbeDirectory(std::path::PathBuf);
impl Drop for SyntheticProbeDirectory {
    fn drop(&mut self) {
        // Only these known files are created; no recursive deletion or traversal.
        for file in [
            "synthetic.sqlite3",
            "synthetic.sqlite3-wal",
            "synthetic.sqlite3-shm",
        ] {
            let _ = std::fs::remove_file(self.0.join(file));
        }
        let _ = std::fs::remove_dir(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_foreground_under_store_lock_never_waits_for_commit_control() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = Arc::new(Mutex::new(Store::open(
            directory.path().join("lock-order.sqlite3"),
            "simulator",
        )?));
        let worker = MemoryWorker::new();
        let guard = worker.foreground();
        // Simulate the worker commit holding control while delivery owns the
        // Store lock and removes its Pending (thereby dropping the guard).
        let control = worker.control.lock().unwrap();
        let (done, receipt) = std::sync::mpsc::channel();
        let delivery = std::thread::spawn(move || {
            let _store = store.lock().unwrap();
            drop(guard);
            let _ = done.send(());
        });
        let dropped = receipt.recv_timeout(Duration::from_secs(1));
        let count = control.foreground.load(Ordering::SeqCst);
        drop(control);
        delivery.join().unwrap();
        ensure!(
            dropped.is_ok() && count == 0,
            "前台guard释放不得反向等待control锁"
        );
        Ok(())
    }
}
