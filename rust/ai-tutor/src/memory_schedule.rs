//! Workstation's optional, single shadow-memory scheduler. Parent startup calls
//! this only after acquiring its exclusive process lock; no loop starts by default.
use super::Workstation;
use crate::{
    kimi::memory_contract,
    memory_worker::{MemoryConsolidator, MemoryWorkerMode, MemoryWorkerPolicy},
    store::{MemoryJobLimits, s},
};
use anyhow::{Result, anyhow, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Disabled,
    Shadow,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SchedulePolicy {
    mode: Mode,
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    limits: MemoryJobLimits,
}
impl SchedulePolicy {
    fn load(root: &Path) -> Result<Option<Self>> {
        let path = root.join("runtime/memory-policy.json");
        let raw = match std::fs::read(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        ensure!(raw.len() <= 4096, "记忆调度策略文件过大");
        let policy: Self =
            serde_json::from_slice(&raw).map_err(|_| anyhow!("记忆调度策略格式无效"))?;
        if matches!(policy.mode, Mode::Disabled) {
            return Ok(None);
        }
        policy.worker_policy()?;
        Ok(Some(policy))
    }
    fn worker_policy(&self) -> Result<MemoryWorkerPolicy> {
        self.limits.validate()?;
        ensure!(self.limits.lease_seconds >= 100, "记忆调度租约至少100秒");
        let mode = match self.namespace.as_deref() {
            Some("real") => MemoryWorkerMode::OptInReal,
            Some("simulator") => MemoryWorkerMode::SyntheticOnly,
            _ => return Err(anyhow!("shadow记忆策略必须指定单个real或simulator空间")),
        };
        ensure!(matches!(self.mode, Mode::Shadow), "记忆调度未启用");
        Ok(MemoryWorkerPolicy {
            mode,
            limits: self.limits.clone(),
        })
    }
}

const MEMORY_IDLE_WINDOW: Duration = Duration::from_secs(10);

fn foreground_audio_idle(foreground_idle: bool, audio: &Value) -> bool {
    foreground_idle
        && audio["recording"] != true
        && audio["capture_pending_result"] != true
        && !["recording", "transcribing", "speaking", "synthesizing"].contains(&s(audio, "state"))
}

fn idle_for_memory(foreground_idle: bool, audio: &Value, last_input_elapsed: Duration) -> bool {
    foreground_audio_idle(foreground_idle, audio) && last_input_elapsed >= MEMORY_IDLE_WINDOW
}

// Generation stamps catch short capture/playback operations between polls.
// last_input alone is not enough: a long reply can finish long after Submit.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ForegroundActivity {
    last_input: Instant,
    recording_generation: Option<u64>,
    speech_epoch: u64,
}

struct IdleAdmission {
    required: Duration,
    since: Option<Instant>,
    activity: Option<ForegroundActivity>,
}
impl IdleAdmission {
    fn new(required: Duration) -> Self {
        Self {
            required,
            since: None,
            activity: None,
        }
    }

    fn reset(&mut self) {
        self.since = None;
    }

    fn observe_worker_outcome(&mut self, outcome: &Value) {
        if matches!(
            s(outcome, "status"),
            "preempted" | "foreground_or_worker_busy" | "preempted_before_claim"
        ) {
            self.reset();
        }
    }

    fn observe(
        &mut self,
        now: Instant,
        foreground_idle: bool,
        audio: &Value,
        speech_epoch: u64,
        last_input: Instant,
    ) -> bool {
        let activity = ForegroundActivity {
            last_input,
            recording_generation: audio["recording_generation"].as_u64(),
            speech_epoch,
        };
        if self.activity != Some(activity) {
            self.reset();
            self.activity = Some(activity);
        }
        if !foreground_audio_idle(foreground_idle, audio) {
            self.reset();
            return false;
        }
        let since = *self.since.get_or_insert(now);
        now.duration_since(since) >= self.required
            && idle_for_memory(foreground_idle, audio, now.duration_since(last_input))
    }
}

struct ScheduleLifetime(Arc<Workstation>);
impl Drop for ScheduleLifetime {
    fn drop(&mut self) {
        self.0
            .memory_schedule_started
            .store(false, Ordering::SeqCst);
    }
}
impl Workstation {
    /// Call once, only after the main process owns its exclusive startup lock.
    /// Missing/disabled policy performs no recovery, discovery or provider call.
    pub fn start_memory_schedule(self: &Arc<Self>) -> Result<Option<JoinHandle<()>>> {
        let Some(policy) = SchedulePolicy::load(&self.root)? else {
            return Ok(None);
        };
        if self.provider.status()["available"] != true {
            self.metrics.lock().unwrap()["memory_schedule"] =
                json!({"mode":"shadow","state":"waiting_for_verified_provider"});
            return Ok(None);
        }
        Ok(Some(self.spawn_memory_schedule(
            self.provider.clone(),
            policy,
            Duration::from_secs(2),
            MEMORY_IDLE_WINDOW,
        )?))
    }

    fn spawn_memory_schedule<P: MemoryConsolidator + 'static>(
        self: &Arc<Self>,
        provider: Arc<P>,
        policy: SchedulePolicy,
        poll: Duration,
        quiet_period: Duration,
    ) -> Result<JoinHandle<()>> {
        ensure!(!self.shutting_down.load(Ordering::SeqCst), "应用正在关闭");
        let worker_policy = policy.worker_policy()?;
        let namespace = policy.namespace.unwrap();
        ensure!(
            self.memory_schedule_started
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok(),
            "记忆调度已启动"
        );
        let lifetime = ScheduleLifetime(self.clone());
        {
            let stores = self.stores.lock().unwrap();
            stores
                .get(&namespace)
                .ok_or_else(|| anyhow!("记忆调度空间不存在"))?
                .recover_memory_jobs(chrono::Utc::now().timestamp_millis())?;
        }
        self.metrics.lock().unwrap()["memory_schedule"] =
            json!({"mode":"shadow","namespace":namespace,"state":"waiting_for_idle"});
        let app = self.clone();
        Ok(tokio::spawn(async move {
            let _lifetime = lifetime;
            let mut ticker = tokio::time::interval(poll);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut reported_error = false;
            let mut admission = IdleAdmission::new(quiet_period);
            loop {
                ticker.tick().await;
                if app.shutting_down.load(Ordering::SeqCst) {
                    break;
                }
                match app
                    .memory_schedule_tick(
                        provider.as_ref(),
                        &namespace,
                        &worker_policy,
                        &mut admission,
                        Instant::now,
                    )
                    .await
                {
                    Ok(result) => {
                        reported_error = false;
                        app.metrics.lock().unwrap()["memory_schedule"] =
                            json!({"mode":"shadow","namespace":namespace,"last_tick":result});
                    }
                    Err(_) => {
                        if !reported_error {
                            app.error("后台记忆整理暂时失败，未应用候选".into());
                            reported_error = true;
                        }
                    }
                }
            }
            app.metrics.lock().unwrap()["memory_schedule"] =
                json!({"mode":"shadow","namespace":namespace,"state":"stopped"});
        }))
    }

    async fn memory_schedule_tick<P: MemoryConsolidator + ?Sized>(
        &self,
        provider: &P,
        namespace: &str,
        policy: &MemoryWorkerPolicy,
        admission: &mut IdleAdmission,
        now: impl Fn() -> Instant,
    ) -> Result<Value> {
        let audio = self.audio.status();
        let last_input = *self.last_input.lock().unwrap();
        if !admission.observe(
            now(),
            self.memory_worker.is_idle(),
            &audio,
            self.audio.speech_epoch(),
            last_input,
        ) {
            return Ok(json!({"status":"waiting_for_idle"}));
        }
        let discovered = {
            let stores = self.stores.lock().unwrap();
            stores
                .get(namespace)
                .ok_or_else(|| anyhow!("记忆调度空间不存在"))?
                .discover_memory_jobs(
                    memory_contract::PROMPT_VERSION,
                    &policy.limits,
                    policy.mode == MemoryWorkerMode::SyntheticOnly,
                    chrono::Utc::now().timestamp_millis(),
                )?
        };
        // Recording/foreground may have started during the short discovery
        // transaction. Recheck before claim; the worker also serializes guards.
        if self.shutting_down.load(Ordering::SeqCst)
            || !admission.observe(
                now(),
                self.memory_worker.is_idle(),
                &self.audio.status(),
                self.audio.speech_epoch(),
                *self.last_input.lock().unwrap(),
            )
        {
            return Ok(json!({"status":"waiting_for_idle","discovery":discovered}));
        }
        let outcome = self
            .memory_worker
            .run_once(&self.stores, provider, namespace, policy)
            .await?;
        // A foreground pulse can begin and finish while run_once yields.
        // Preserve its paid attempt, but never reuse the earlier idle window.
        admission.observe_worker_outcome(&outcome);
        Ok(json!({"discovery":discovered,"worker":outcome}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_worker::{CancelFuture, ProviderFuture};
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::{Notify, watch};

    fn policy() -> SchedulePolicy {
        SchedulePolicy {
            mode: Mode::Shadow,
            namespace: Some("simulator".into()),
            limits: Default::default(),
        }
    }
    fn seed(app: &Workstation) -> Result<(String, String)> {
        let stores = app.stores.lock().unwrap();
        let store = &stores["simulator"];
        let learner = store.create_learner("synthetic schedule", "en", "guided")?;
        let lid = s(&learner, "learner_id").to_owned();
        let session = store.create_session(&lid)?;
        let sid = s(&session, "session_id").to_owned();
        store.record_education_event(
            &lid,
            &sid,
            "simulated",
            "observation",
            &json!({"text":"Synthetic lamp report.","synthetic_fixture":true}),
        )?;
        Ok((lid, sid))
    }

    #[test]
    fn idle_policy_blocks_foreground_audio_and_the_ten_second_input_window() {
        let idle = json!({"state":"idle","recording":false});
        assert!(!idle_for_memory(false, &idle, Duration::from_secs(20)));
        assert!(!idle_for_memory(true, &idle, Duration::from_millis(9999)));
        assert!(idle_for_memory(true, &idle, Duration::from_secs(10)));
        for state in ["recording", "transcribing", "speaking", "synthesizing"] {
            assert!(!idle_for_memory(
                true,
                &json!({"state":state}),
                Duration::from_secs(20)
            ));
        }
        assert!(!idle_for_memory(
            true,
            &json!({"state":"idle","recording":true}),
            Duration::from_secs(20)
        ));
    }

    #[tokio::test]
    async fn missing_and_disabled_policy_do_not_start_or_recover_existing_leases() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let app = Workstation::new(directory.path().to_owned(), "synthetic-token".into())?;
        let (lid, _) = seed(&app)?;
        let claim = {
            let stores = app.stores.lock().unwrap();
            let store = &stores["simulator"];
            store.discover_memory_jobs(
                memory_contract::PROMPT_VERSION,
                &Default::default(),
                true,
                chrono::Utc::now().timestamp_millis(),
            )?;
            store
                .claim_memory_job(
                    "existing-owner",
                    &Default::default(),
                    chrono::Utc::now().timestamp_millis(),
                )?
                .unwrap()
        };
        assert!(app.start_memory_schedule()?.is_none());
        std::fs::write(
            directory.path().join("runtime/memory-policy.json"),
            r#"{"mode":"disabled"}"#,
        )?;
        assert!(app.start_memory_schedule()?.is_none());
        assert!(!app.memory_schedule_started.load(Ordering::SeqCst));
        assert_eq!(
            app.stores.lock().unwrap()["simulator"]
                .memory_job_status(&lid, s(&claim["job"], "job_id"))?["status"],
            "leased"
        );
        for raw in [
            r#"{"mode":"shadow"}"#,
            r#"{"mode":"shadow","namespace":["real","simulator"]}"#,
            r#"{"mode":"shadow","namespace":"simulator","apply":true}"#,
            r#"{"mode":"automatic","namespace":"real"}"#,
        ] {
            std::fs::write(directory.path().join("runtime/memory-policy.json"), raw)?;
            assert!(app.start_memory_schedule().is_err());
        }
        app.close().await;
        Ok(())
    }

    struct Fake {
        calls: AtomicUsize,
        entered: Notify,
        release: watch::Sender<bool>,
    }
    impl MemoryConsolidator for Fake {
        fn consolidate<'a>(
            &'a self,
            _request: &'a Value,
            started: watch::Sender<bool>,
        ) -> ProviderFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let _ = started.send(true);
                let mut release = self.release.subscribe();
                self.entered.notify_one();
                if !*release.borrow() {
                    release.changed().await?;
                }
                Ok(json!({"schema_version":memory_contract::SCHEMA_VERSION,"candidates":[]}))
            })
        }
        fn cancel<'a>(&'a self, _request_id: &'a str) -> CancelFuture<'a> {
            Box::pin(async move {
                self.release.send_replace(true);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn one_scheduler_waits_for_foreground_then_shutdown_preempts_and_joins() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let app = Workstation::new(directory.path().to_owned(), "synthetic-token".into())?;
        seed(&app)?;
        *app.last_input.lock().unwrap() = std::time::Instant::now() - Duration::from_secs(11);
        let foreground = app.memory_worker.foreground();
        let (release, _) = watch::channel(false);
        let provider = Arc::new(Fake {
            calls: AtomicUsize::new(0),
            entered: Notify::new(),
            release,
        });
        let handle = app.spawn_memory_schedule(
            provider.clone(),
            policy(),
            Duration::from_millis(10),
            Duration::from_millis(30),
        )?;
        assert!(
            app.spawn_memory_schedule(
                provider.clone(),
                policy(),
                Duration::from_millis(10),
                Duration::from_millis(30)
            )
            .is_err()
        );
        tokio::time::sleep(Duration::from_millis(35)).await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            app.stores.lock().unwrap()["simulator"]
                .one("SELECT COUNT(*) AS count FROM memory_job_attempts", [])?
                .unwrap()["count"],
            0
        );
        drop(foreground);
        tokio::time::timeout(Duration::from_secs(2), provider.entered.notified()).await?;
        assert!(app.stores.try_lock().is_ok());
        app.shutting_down.store(true, Ordering::SeqCst);
        app.memory_worker.preempt();
        tokio::time::timeout(Duration::from_secs(3), handle).await??;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert!(!app.memory_schedule_started.load(Ordering::SeqCst));
        let job = app.stores.lock().unwrap()["simulator"]
            .one("SELECT status FROM memory_jobs", [])?
            .unwrap();
        assert_eq!(job["status"], "pending");
        app.close().await;
        Ok(())
    }

    #[test]
    fn continuous_idle_starts_after_busy_ends_even_when_submit_is_old() {
        let start = Instant::now();
        let last_input = start - Duration::from_secs(60);
        let idle = json!({"state":"idle","recording":false,"recording_generation":2});
        for (foreground_idle, busy_audio) in [
            (false, idle.clone()),
            (true, json!({"state":"recording","recording_generation":2})),
            (
                true,
                json!({"state":"transcribing","recording_generation":2}),
            ),
            (
                true,
                json!({"state":"synthesizing","recording_generation":2}),
            ),
            (true, json!({"state":"speaking","recording_generation":2})),
            (
                true,
                json!({"state":"idle","recording":true,"recording_generation":2}),
            ),
        ] {
            let mut admission = IdleAdmission::new(MEMORY_IDLE_WINDOW);
            assert!(!admission.observe(start, true, &idle, 6, last_input));
            assert!(admission.observe(start + Duration::from_secs(10), true, &idle, 6, last_input));
            assert!(!admission.observe(
                start + Duration::from_secs(20),
                foreground_idle,
                &busy_audio,
                6,
                last_input
            ));
            assert!(!admission.observe(
                start + Duration::from_secs(21),
                true,
                &idle,
                6,
                last_input
            ));
            // The roughly half-second voice cooldown cannot spend a reservation.
            assert!(!admission.observe(
                start + Duration::from_millis(21530),
                true,
                &idle,
                6,
                last_input
            ));
            assert!(!admission.observe(
                start + Duration::from_millis(30999),
                true,
                &idle,
                6,
                last_input
            ));
            assert!(admission.observe(start + Duration::from_secs(31), true, &idle, 6, last_input));
        }
    }

    #[test]
    fn short_activity_between_polls_and_new_input_reset_the_idle_window() {
        let start = Instant::now();
        let old_input = start - Duration::from_secs(60);
        let idle = json!({"state":"idle","recording":false,"recording_generation":2});
        let mut admission = IdleAdmission::new(MEMORY_IDLE_WINDOW);
        assert!(!admission.observe(start, true, &idle, 6, old_input));
        assert!(admission.observe(start + Duration::from_secs(10), true, &idle, 6, old_input));
        let captured = json!({"state":"idle","recording":false,"recording_generation":4});
        assert!(!admission.observe(
            start + Duration::from_secs(11),
            true,
            &captured,
            6,
            old_input
        ));
        assert!(!admission.observe(
            start + Duration::from_millis(20999),
            true,
            &captured,
            6,
            old_input
        ));
        assert!(admission.observe(
            start + Duration::from_secs(21),
            true,
            &captured,
            6,
            old_input
        ));
        assert!(!admission.observe(
            start + Duration::from_secs(22),
            true,
            &captured,
            8,
            old_input
        ));
        assert!(admission.observe(
            start + Duration::from_secs(32),
            true,
            &captured,
            8,
            old_input
        ));
        let new_input = start + Duration::from_secs(33);
        assert!(!admission.observe(new_input, true, &captured, 8, new_input));
        assert!(!admission.observe(
            start + Duration::from_millis(42999),
            true,
            &captured,
            8,
            new_input
        ));
        assert!(admission.observe(
            start + Duration::from_secs(43),
            true,
            &captured,
            8,
            new_input
        ));
        // Consecutive background jobs may use an already mature quiet interval.
        assert!(admission.observe(
            start + Duration::from_secs(44),
            true,
            &captured,
            8,
            new_input
        ));
    }

    #[test]
    fn pending_final_cannot_mature_idle_before_a_short_transcription() {
        let start = Instant::now();
        let last_input = start - Duration::from_secs(60);
        let pending = json!({"state":"idle","recording":false,
            "recording_generation":2,"capture_pending_result":true});
        let idle = json!({"state":"idle","recording":false,
            "recording_generation":2,"capture_pending_result":false});
        let mut admission = IdleAdmission::new(MEMORY_IDLE_WINDOW);
        // Endpoint closure can precede the user's Stop/final retrieval by a long
        // time. The completed ASR may then fit wholly between two polls.
        assert!(!admission.observe(start, true, &pending, 6, last_input));
        assert!(!admission.observe(
            start + Duration::from_secs(20),
            true,
            &pending,
            6,
            last_input
        ));
        assert!(!admission.observe(start + Duration::from_secs(22), true, &idle, 6, last_input));
        assert!(!admission.observe(
            start + Duration::from_millis(31999),
            true,
            &idle,
            6,
            last_input
        ));
        assert!(admission.observe(start + Duration::from_secs(32), true, &idle, 6, last_input));
    }

    #[tokio::test]
    async fn preclaim_foreground_refusal_discards_a_previously_mature_window() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let app = Workstation::new(directory.path().to_owned(), "synthetic-token".into())?;
        seed(&app)?;
        let start = Instant::now();
        let last_input = start - Duration::from_secs(60);
        let idle = app.audio.status();
        let epoch = app.audio.speech_epoch();
        let policy = policy().worker_policy()?;
        app.stores.lock().unwrap()["simulator"].discover_memory_jobs(
            memory_contract::PROMPT_VERSION,
            &policy.limits,
            true,
            chrono::Utc::now().timestamp_millis(),
        )?;
        let mut admission = IdleAdmission::new(MEMORY_IDLE_WINDOW);
        assert!(!admission.observe(start, true, &idle, epoch, last_input));
        assert!(admission.observe(
            start + Duration::from_secs(10),
            true,
            &idle,
            epoch,
            last_input
        ));
        let (release, _) = watch::channel(true);
        let provider = Fake {
            calls: AtomicUsize::new(0),
            entered: Notify::new(),
            release,
        };
        // Reproduce the worker's actual refusal after admission matured, then
        // end the whole foreground pulse before the next scheduler observation.
        let guard = app.memory_worker.foreground();
        let refused = app
            .memory_worker
            .run_once(&app.stores, &provider, "simulator", &policy)
            .await?;
        assert_eq!(refused["status"], "foreground_or_worker_busy");
        drop(guard);
        admission.observe_worker_outcome(&refused);
        assert!(!admission.observe(
            start + Duration::from_secs(12),
            true,
            &idle,
            epoch,
            last_input
        ));
        assert!(!admission.observe(
            start + Duration::from_millis(21999),
            true,
            &idle,
            epoch,
            last_input
        ));
        assert!(admission.observe(
            start + Duration::from_secs(22),
            true,
            &idle,
            epoch,
            last_input
        ));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            app.stores.lock().unwrap()["simulator"]
                .one("SELECT COUNT(*) AS count FROM memory_job_attempts", [])?
                .unwrap()["count"],
            0
        );
        // The separate pre-claim cancellation status has the same scheduling
        // meaning. Do not require a racy thread interleaving to exercise it.
        admission.observe_worker_outcome(&json!({"status":"preempted_before_claim"}));
        assert!(!admission.observe(
            start + Duration::from_secs(24),
            true,
            &idle,
            epoch,
            last_input
        ));
        assert!(admission.observe(
            start + Duration::from_secs(34),
            true,
            &idle,
            epoch,
            last_input
        ));
        app.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn real_queue_does_not_reserve_a_job_during_a_short_idle_gap() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let app = Workstation::new(directory.path().to_owned(), "synthetic-token".into())?;
        seed(&app)?;
        let start = Instant::now();
        *app.last_input.lock().unwrap() = start - Duration::from_secs(60);
        let (release, _) = watch::channel(true);
        let provider = Fake {
            calls: AtomicUsize::new(0),
            entered: Notify::new(),
            release,
        };
        let policy = policy().worker_policy()?;
        let mut admission = IdleAdmission::new(MEMORY_IDLE_WINDOW);
        let guard = app.memory_worker.foreground();
        assert_eq!(
            app.memory_schedule_tick(&provider, "simulator", &policy, &mut admission, || start)
                .await?["status"],
            "waiting_for_idle"
        );
        drop(guard);
        for elapsed in [20_000, 20_530, 29_999] {
            assert_eq!(
                app.memory_schedule_tick(&provider, "simulator", &policy, &mut admission, || start
                    + Duration::from_millis(elapsed))
                    .await?["status"],
                "waiting_for_idle"
            );
            assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                app.stores.lock().unwrap()["simulator"]
                    .one("SELECT COUNT(*) AS count FROM memory_job_attempts", [])?
                    .unwrap()["count"],
                0
            );
        }
        let result = app
            .memory_schedule_tick(&provider, "simulator", &policy, &mut admission, || {
                start + Duration::from_secs(30)
            })
            .await?;
        assert_eq!(result["worker"]["status"], "shadow_staged");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            app.stores.lock().unwrap()["simulator"]
                .one("SELECT COUNT(*) AS count FROM memory_job_attempts", [])?
                .unwrap()["count"],
            1
        );
        app.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn preempted_job_keeps_its_reservation_and_waits_for_a_new_idle_window() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let app = Workstation::new(directory.path().to_owned(), "synthetic-token".into())?;
        seed(&app)?;
        let start = Instant::now();
        *app.last_input.lock().unwrap() = start - Duration::from_secs(60);
        let (release, _) = watch::channel(false);
        let provider = Fake {
            calls: AtomicUsize::new(0),
            entered: Notify::new(),
            release,
        };
        let mut policy = policy().worker_policy()?;
        policy.limits.max_attempts = 2;
        let mut admission = IdleAdmission::new(MEMORY_IDLE_WINDOW);
        assert_eq!(
            app.memory_schedule_tick(&provider, "simulator", &policy, &mut admission, || start)
                .await?["status"],
            "waiting_for_idle"
        );
        let (first, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(
                app.memory_schedule_tick(&provider, "simulator", &policy, &mut admission, || start
                    + Duration::from_secs(10)),
                async {
                    provider.entered.notified().await;
                    // This whole foreground pulse occurs between scheduler polls.
                    let guard = app.memory_worker.foreground();
                    drop(guard);
                }
            )
        })
        .await?;
        let first = first?;
        assert_eq!(first["worker"]["status"], "preempted");
        let job_id = s(&first["worker"], "job_id").to_owned();
        for elapsed in [11_000, 11_530, 20_999] {
            assert_eq!(
                app.memory_schedule_tick(&provider, "simulator", &policy, &mut admission, || start
                    + Duration::from_millis(elapsed))
                    .await?["status"],
                "waiting_for_idle"
            );
            assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                app.stores.lock().unwrap()["simulator"]
                    .one("SELECT COUNT(*) AS count FROM memory_job_attempts", [])?
                    .unwrap()["count"],
                1
            );
        }
        let recovered = app
            .memory_schedule_tick(&provider, "simulator", &policy, &mut admission, || {
                start + Duration::from_secs(21)
            })
            .await?;
        assert_eq!(recovered["worker"]["job_id"], job_id);
        assert_eq!(recovered["worker"]["status"], "shadow_staged");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        let stores = app.stores.lock().unwrap();
        let store = &stores["simulator"];
        assert_eq!(
            store
                .one("SELECT COUNT(*) AS count FROM memory_jobs", [])?
                .unwrap()["count"],
            1
        );
        assert_eq!(
            store
                .one("SELECT COUNT(*) AS count FROM memory_job_attempts", [])?
                .unwrap()["count"],
            2
        );
        assert_eq!(
            store
                .one("SELECT attempts FROM memory_jobs WHERE job_id=?", [&job_id])?
                .unwrap()["attempts"],
            2
        );
        let statuses = store.rows(
            "SELECT status FROM memory_job_attempts WHERE job_id=? ORDER BY attempt_no",
            [&job_id],
        )?;
        assert_eq!(statuses[0]["status"], "preempted");
        assert_eq!(statuses[1]["status"], "response_received");
        drop(stores);
        app.close().await;
        Ok(())
    }
}
