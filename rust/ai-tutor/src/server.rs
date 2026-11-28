//! Loopback-only Rust application coordination, generation isolation and API.
#[path = "activity_history.rs"]
mod activity_history;
#[cfg(all(test, windows))]
#[path = "background_acp_diagnostic.rs"]
mod background_acp_diagnostic;
#[path = "final_asr_runtime.rs"]
mod final_asr_runtime;
#[path = "gpu_queue.rs"]
mod gpu_queue;
#[path = "local_asr_runtime.rs"]
mod local_asr_runtime;
#[path = "local_speech_runtime.rs"]
mod local_speech_runtime;
#[path = "memory_retrieval.rs"]
mod memory_retrieval;
#[path = "memory_review.rs"]
mod memory_review;
#[path = "memory_schedule.rs"]
mod memory_schedule;
#[path = "parent_growth.rs"]
mod parent_growth;
#[path = "recording.rs"]
mod recording;
#[cfg(all(test, windows))]
#[path = "segment_playback_diagnostic.rs"]
mod segment_playback_diagnostic;
#[path = "speech_delivery.rs"]
mod speech_delivery;
#[cfg(all(test, windows))]
#[path = "teaching_provider_real_diagnostic.rs"]
mod teaching_provider_real_diagnostic;
#[path = "temporary_help_style.rs"]
mod temporary_help_style;
#[path = "tutor_delivery.rs"]
mod tutor_delivery;
#[path = "voice_loop.rs"]
mod voice_loop;
#[cfg(all(test, windows))]
#[path = "voice_loop_model_diagnostic.rs"]
mod voice_loop_model_diagnostic;
#[cfg(all(test, windows))]
#[path = "voice_loop_soak_diagnostic.rs"]
mod voice_loop_soak_diagnostic;
#[cfg(all(test, windows))]
#[path = "voice_memory_managed_diagnostic.rs"]
mod voice_memory_managed_diagnostic;
use crate::{
    audio::AudioService,
    context::build_context,
    kimi::KimiProvider,
    memory_worker::{ForegroundGuard, MemoryWorker},
    preferences::UiPreferences,
    ptz::NativePTZ,
    store::{Store, s, strings, uid},
    tutor::{apply_reply, validate_reply},
    vision::CameraService,
};
use anyhow::{Result, anyhow, bail, ensure};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{
        DefaultBodyLimit, Path, Query, State,
        ws::{Message, WebSocketUpgrade},
    },
    http::{HeaderMap, Method, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{Mutex as AsyncMutex, Notify, broadcast, watch},
    task::JoinHandle,
};
use tutor_delivery::DeliveryMode;

struct Pending {
    sid: String,
    generation: i64,
    epoch: i64,
    reply: Value,
    _foreground: Option<ForegroundGuard>,
}
struct Running {
    sid: String,
    _task: JoinHandle<()>,
    cancellation: watch::Sender<bool>,
}
pub struct Workstation {
    pub root: PathBuf,
    pub token: String,
    stores: Mutex<HashMap<String, Store>>,
    ui_preferences: Mutex<UiPreferences>,
    current: Mutex<Option<(String, String)>>,
    control: AsyncMutex<()>,
    camera_control: AsyncMutex<()>,
    pending: Mutex<HashMap<String, Pending>>,
    running: Mutex<HashMap<String, Running>>,
    recording: Mutex<Option<recording::RecordingBinding>>,
    voice_loop: Mutex<voice_loop::VoiceLoopControl>,
    speech_delivery: Mutex<speech_delivery::SpeechDelivery>,
    local_speech: Arc<local_speech_runtime::LocalSpeechRuntime>,
    local_asr: local_asr_runtime::LocalAsrRuntime,
    final_asr: final_asr_runtime::FinalAsrRuntime,
    gpu_queue: Mutex<gpu_queue::GpuQueuePin>,
    pub camera: Arc<CameraService>,
    pub audio: Arc<AudioService>,
    pub ptz: NativePTZ,
    pub provider: Arc<KimiProvider>,
    memory_worker: MemoryWorker,
    local_memory: Arc<memory_retrieval::MemoryRetrievalRuntime>,
    memory_schedule_started: AtomicBool,
    manual_memory_review: Mutex<Option<memory_review::ReviewRun>>,
    metrics: Mutex<Value>,
    errors: Mutex<Vec<Value>>,
    last_input: Mutex<Instant>,
    started: Instant,
    pub shutdown: Notify,
    shutting_down: AtomicBool,
    events: broadcast::Sender<Value>,
    delivery_mode: DeliveryMode,
}
impl Workstation {
    pub fn new(root: PathBuf, token: String) -> Result<Arc<Self>> {
        let delivery_mode = DeliveryMode::load(&root)?;
        let mut stores = HashMap::new();
        for ns in ["real", "simulator"] {
            stores.insert(
                ns.into(),
                Store::open(root.join(format!("runtime/data/{ns}.sqlite3")), ns)?,
            );
        }
        let (events, _) = broadcast::channel(16);
        let camera = Arc::new(CameraService::new(root.clone()));
        let ui_preferences = UiPreferences::open(&root.join("runtime/data/workstation.sqlite3"))?;
        Ok(Arc::new(Self {
            camera: camera.clone(),
            audio: Arc::new(AudioService::new_for_workstation(root.clone())),
            ptz: NativePTZ::with_camera(root.clone(), camera),
            provider: Arc::new(KimiProvider::new_for_workstation(root.clone())?),
            memory_worker: MemoryWorker::new(),
            local_memory: Arc::new(memory_retrieval::MemoryRetrievalRuntime::default()),
            memory_schedule_started: AtomicBool::new(false),
            manual_memory_review: Mutex::new(None),
            root,
            token,
            stores: Mutex::new(stores),
            ui_preferences: Mutex::new(ui_preferences),
            current: Mutex::new(None),
            control: AsyncMutex::new(()),
            camera_control: AsyncMutex::new(()),
            pending: Mutex::new(HashMap::new()),
            running: Mutex::new(HashMap::new()),
            recording: Mutex::new(None),
            voice_loop: Mutex::new(voice_loop::VoiceLoopControl::default()),
            speech_delivery: Mutex::new(speech_delivery::SpeechDelivery::default()),
            local_speech: Arc::new(local_speech_runtime::LocalSpeechRuntime::default()),
            local_asr: local_asr_runtime::LocalAsrRuntime::default(),
            final_asr: final_asr_runtime::FinalAsrRuntime::default(),
            gpu_queue: Mutex::new(gpu_queue::GpuQueuePin::default()),
            metrics: Mutex::new(
                json!({"requests":0,"failed":0,"cancelled":0,"retries":0,"consolidation_requests":0,"reported_tokens":null,"last_latency_ms":null}),
            ),
            errors: Mutex::new(vec![]),
            last_input: Mutex::new(Instant::now() - Duration::from_secs(2)),
            started: Instant::now(),
            shutdown: Notify::new(),
            shutting_down: AtomicBool::new(false),
            events,
            delivery_mode,
        }))
    }
    fn owner(&self, lid: &str) -> Result<String> {
        let stores = self.stores.lock().unwrap();
        for (ns, store) in stores.iter() {
            if store.learner(lid).is_ok() {
                return Ok(ns.clone());
            }
        }
        bail!("学习者档案不存在")
    }
    fn active(&self, sid: &str) -> Result<(String, Value)> {
        let current = self
            .current
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("请先选择学习者档案"))?;
        ensure!(current.1 == sid, "会话已过期，请先选择该档案");
        let session = self.stores.lock().unwrap()[&current.0].session(sid)?;
        Ok((current.0, session))
    }
    fn writable(&self, sid: &str) -> Result<(String, Value)> {
        let current = self.active(sid)?;
        ensure!(
            s(&current.1, "status") != "finished",
            "会话已结束，请开始新活动"
        );
        Ok(current)
    }
    fn bump_metric(&self, key: &str) {
        let mut m = self.metrics.lock().unwrap();
        m[key] = json!(m[key].as_u64().unwrap_or(0) + 1);
    }
    fn notify(&self, kind: &str) {
        let _ = self.events.send(json!({"type":kind}));
    }
    fn error(&self, message: String) {
        let mut errors = self.errors.lock().unwrap();
        errors.push(json!({"message":message,"at":chrono::Utc::now().to_rfc3339()}));
        if errors.len() > 8 {
            errors.remove(0);
        }
    }
    pub fn state(&self) -> Result<Value> {
        let current = self.current.lock().unwrap().clone();
        let stores = self.stores.lock().unwrap();
        let mut learners = vec![];
        let mut persisted = serde_json::Map::new();
        for store in stores.values() {
            learners.extend(store.learners()?);
            for (key, value) in store.run_metrics()?.as_object().into_iter().flatten() {
                let count = persisted.get(key).and_then(Value::as_u64).unwrap_or(0)
                    + value.as_u64().unwrap_or(0);
                persisted.insert(key.clone(), json!(count));
            }
        }
        let (session, parent_digest) = if let Some((ns, sid)) = current {
            let store = &stores[&ns];
            let session = store.session(&sid)?;
            let parent_digest = parent_growth::project(store, &session)?;
            (session, parent_digest)
        } else {
            (Value::Null, parent_growth::empty_digest())
        };
        drop(stores);
        let ptz = self.ptz.status();
        let mut camera = self.camera.status();
        camera["ptz"] = ptz.clone();
        let mut metrics = self.metrics.lock().unwrap().clone();
        metrics["uptime_seconds"] = json!(self.started.elapsed().as_secs());
        metrics["persisted_requests"] =
            json!(persisted.values().filter_map(Value::as_u64).sum::<u64>());
        metrics["persisted_runs"] = Value::Object(persisted);
        let mut system = sysinfo::System::new();
        let pid = sysinfo::get_current_pid().map_err(anyhow::Error::msg)?;
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        metrics["memory_mb"] = system
            .process(pid)
            .map(|p| json!(p.memory() / 1024 / 1024))
            .unwrap_or(Value::Null);
        let local_memory = self.local_memory.status(
            &session,
            &learners,
            self.shutting_down.load(Ordering::SeqCst),
        );
        Ok(
            json!({"learners":learners,"session":session,"parent_digest":parent_digest,"tutor_delivery":self.delivery_mode.status(),"voice_loop":self.voice_loop_status(),"ui_preferences":self.ui_preferences.lock().unwrap().get()?,"camera":camera,"kimi":self.provider.status(),"teaching":self.provider.foreground_status(),"audio":self.audio.status(),"local_speech":self.local_speech_status(),"local_asr":self.local_asr_status(),"final_asr":self.final_asr_status(),"local_memory":local_memory,"metrics":metrics,"errors":self.errors.lock().unwrap().clone(),"ptz":ptz,"emotion":{"enabled":false},"actuator_execution":false,"implementation":"Rust native + Rust WebAssembly"}),
        )
    }
    async fn cancel(&self) -> Result<()> {
        self.pause_voice_loop(None, "manual_action");
        self.cancel_turn().await
    }
    async fn cancel_turn(&self) -> Result<()> {
        self.cancel_queued_memory_review();
        self.memory_worker.preempt();
        let _ = self.stop_playback_locked_reason("superseded").await;
        let current = self.current.lock().unwrap().clone();
        if let Some((namespace, sid)) = current {
            self.stores.lock().unwrap()[&namespace].bump(&sid, "idle")?;
            let cancelled: Vec<_> = {
                let running = self.running.lock().unwrap();
                running
                    .iter()
                    .filter(|(_, v)| v.sid == sid)
                    .map(|(id, run)| {
                        let first = !*run.cancellation.borrow();
                        let _ = run.cancellation.send(true);
                        (id.clone(), first)
                    })
                    .collect()
            };
            for (id, first) in cancelled {
                self.provider.cancel(&id).await?;
                // Keep the task registered while it drains provider cleanup.
                // Never await this server task while holding control: its final
                // stale-generation check also needs that lock.
                if first {
                    self.bump_metric("cancelled");
                }
            }
            self.pending.lock().unwrap().retain(|_, p| p.sid != sid);
        }
        Ok(())
    }
    async fn select(&self, lid: &str, resume: bool) -> Result<Value> {
        let _foreground = self.memory_worker.foreground();
        self.ptz.stop();
        let _camera_guard = self.camera_control.lock().await;
        self.ptz.stop_and_wait().await;
        let _guard = self.control.lock().await;
        self.cancel().await?;
        self.audio.discard_recording();
        let namespace = self.owner(lid)?;
        let stores = self.stores.lock().unwrap();
        let store = &stores[&namespace];
        let existing = if resume {
            store.latest_session(lid)?
        } else {
            None
        };
        let mut session = match existing {
            Some(session) => session,
            None => store.create_session(lid)?,
        };
        let sid = s(&session, "session_id").to_owned();
        session["state"]["modules"] = Value::Null;
        session["state"]["modules_source"] = json!("unknown");
        session["state"]["requires_inventory_confirmation"] = json!(true);
        if let Some(e) = session["state"]["field_evidence"].as_object_mut() {
            e.remove("modules");
        }
        store.checkpoint(&sid, &session["state"])?;
        *self.current.lock().unwrap() = Some((namespace, sid.clone()));
        self.notify("state.changed");
        store.session(&sid)
    }
    async fn submit_with_speech(
        self: &Arc<Self>,
        sid: &str,
        text: &str,
        auto_speak: bool,
        client_submission_id: Option<&str>,
        expected_generation: Option<i64>,
    ) -> Result<Value> {
        self.submit_with_speech_loop(
            sid,
            text,
            auto_speak,
            client_submission_id,
            expected_generation,
            None,
            None,
        )
        .await
    }
    async fn submit_with_speech_loop(
        self: &Arc<Self>,
        sid: &str,
        text: &str,
        auto_speak: bool,
        client_submission_id: Option<&str>,
        expected_generation: Option<i64>,
        loop_tag: Option<&voice_loop::LoopTag>,
        recording_generation: Option<u64>,
    ) -> Result<Value> {
        let _guard = self.control.lock().await;
        ensure!(!self.shutting_down.load(Ordering::SeqCst), "工作台正在关闭");
        let (ns, session) = self.writable(sid)?;
        recording::check_expected_generation(&session, expected_generation)?;
        ensure!(self.audio.status()["recording"] != true, "请先结束录音");
        let mut loop_operation = None;
        if let Some(tag) = loop_tag {
            ensure!(expected_generation.is_some(), "自动发送缺少会话代次");
            self.begin_loop_submit(
                tag,
                sid,
                recording_generation,
                text,
                auto_speak,
                client_submission_id,
            )?;
            loop_operation = Some(self.loop_operation(tag));
        }
        {
            let mut last = self.last_input.lock().unwrap();
            ensure!(
                last.elapsed() > Duration::from_millis(1500),
                "输入过于频繁，请稍后"
            );
            *last = Instant::now();
        }
        let foreground = self.memory_worker.foreground();
        if loop_tag.is_some() {
            self.cancel_turn().await?;
        } else {
            self.cancel().await?;
        }
        if let Some(tag) = loop_tag {
            self.loop_generation(tag, sid)?;
        }
        self.audio.discard_recording();
        if regex::Regex::new(
            r"^\s*(请)?(忘记|忘掉|删除)(我的)?(全部|所有)(记忆|记录|信息)?[。！!\s]*$",
        )?
        .is_match(text)
        {
            self.pause_voice_loop(loop_tag, "local_action");
            let lid = s(&session, "learner_id");
            self.provider.cleanup_owned(lid).await?;
            self.stores.lock().unwrap()[&ns].forget(lid, None)?;
            return Ok(json!({"request_id":null,"local_action":"已删除该档案的记忆与活动记录"}));
        }
        if regex::Regex::new(r"^\s*(请)?(忘记|忘掉|删除).+")?.is_match(text) {
            self.pause_voice_loop(loop_tag, "local_action");
            return Ok(
                json!({"request_id":null,"local_action":"请在学习记忆中选择具体条目并忘记，或发送“忘记全部记忆”。本次请求未发送远程模型。"}),
            );
        }
        let speech_epoch = (self.delivery_mode.segment_speech() && auto_speak)
            .then(|| self.audio.reserve_speech_epoch());
        let (request, epoch, retrieval) = {
            let stores = self.stores.lock().unwrap();
            let store = &stores[&ns];
            let generation = store.bump(sid, "generating")?;
            let learner = store.learner(s(&session, "learner_id"))?;
            let rid = uid("request");
            let event = store.record_education_event(
                s(&learner, "learner_id"),
                sid,
                "learner_report",
                "learner_statement",
                &json!({"text":text}),
            )?;
            let eid = s(&event, "event_id").to_owned();
            store.add_turn(sid, &rid, generation, "user", text, "shown", &[eid.clone()])?;
            let mut fast = store.session(sid)?["state"].clone();
            fast["latest_input"] = json!(text);
            temporary_help_style::apply(&mut fast, text, &eid);
            store.checkpoint(sid, &fast)?;
            let session = store.session(sid)?;
            let vision = self.camera.status();
            let context = match build_context(store, &session, &learner, text, &eid, &vision) {
                Ok(v) => v,
                Err(e) => {
                    store.set_status(sid, "error")?;
                    return Err(e);
                }
            };
            let request = json!({"request_id":rid,"learner_id":learner["learner_id"],"session_id":sid,"generation":generation,"context":context});
            store.start_run(
                &rid,
                s(&learner, "learner_id"),
                sid,
                request.to_string().chars().count(),
            )?;
            if self.delivery_mode.segments() {
                store.create_tutor_stream_with_speech(
                    sid,
                    &rid,
                    generation,
                    learner["epoch"].as_i64().unwrap_or(0),
                    speech_epoch,
                    client_submission_id,
                )?;
            }
            let retrieval = self.local_memory.enabled_for(&ns).then(|| {
                memory_retrieval::RetrievalBinding::capture(
                    &ns, &session, &learner, text, &eid, vision,
                )
            });
            (request, learner["epoch"].as_i64().unwrap_or(0), retrieval)
        };
        if let Some(speech_epoch) = speech_epoch {
            self.register_segment_speech_locked(&ns, &request, epoch, speech_epoch);
        }
        if let Some(tag) = loop_tag {
            let reply = voice_loop::LoopReply {
                session_id: sid.into(),
                request_id: s(&request, "request_id").into(),
                generation: request["generation"].as_i64().unwrap_or(-1),
                audio_epoch: speech_epoch.unwrap(),
            };
            if let Err(error) = self.loop_reply_registered(tag, reply) {
                self.stop_segment_speech_locked("user").await?;
                self.stores.lock().unwrap()[&ns].set_status(sid, "idle")?;
                self.stores.lock().unwrap()[&ns].end_run(
                    s(&request, "request_id"),
                    "cancelled",
                    0,
                )?;
                return Err(error);
            }
        }
        self.bump_metric("requests");
        let rid = s(&request, "request_id").to_owned();
        let generation = request["generation"].clone();
        let app = self.clone();
        let request_clone = request.clone();
        let ns_clone = ns.clone();
        let sid_clone = sid.to_owned();
        let (cancellation, cancelled) = watch::channel(false);
        let (start_send, start_receive) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            if start_receive.await.is_err() {
                return;
            }
            app.generate(
                ns_clone,
                sid_clone,
                request_clone,
                epoch,
                foreground,
                cancelled,
                retrieval,
            )
            .await;
        });
        self.running.lock().unwrap().insert(
            rid.clone(),
            Running {
                sid: sid.into(),
                _task: task,
                cancellation,
            },
        );
        let _ = start_send.send(());
        if let Some(operation) = &mut loop_operation {
            operation.complete();
        }
        self.notify("tutor.started");
        Ok(
            json!({"session_id":sid,"request_id":rid,"generation":generation,"voice_loop":loop_tag.map(|tag|self.voice_loop_echo(tag)).transpose()?,"delivery_mode":self.delivery_mode.name(),
                "speech_gate":if speech_epoch.is_some(){"open"}else{"disabled"},"client_submission_id":client_submission_id}),
        )
    }
    async fn generate(
        self: Arc<Self>,
        ns: String,
        sid: String,
        request: Value,
        epoch: i64,
        foreground: ForegroundGuard,
        mut cancellation: watch::Receiver<bool>,
        retrieval: Option<memory_retrieval::RetrievalBinding>,
    ) {
        let start = Instant::now();
        let rid = s(&request, "request_id").to_owned();
        let generation = request["generation"].as_i64().unwrap_or(0);
        let prepared = self
            .prepare_memory_context(request, retrieval, &mut cancellation)
            .await;
        let result = match prepared {
            Err(error) => {
                if self.delivery_mode.segments() {
                    self.preempt_segment_speech(&sid, &rid, generation);
                    let _guard = self.control.lock().await;
                    let phase = if *cancellation.borrow() {
                        "cancelled"
                    } else {
                        "failed"
                    };
                    let _ = self.stores.lock().unwrap()[&ns].fail_tutor_stream(
                        &sid,
                        &rid,
                        phase,
                        "memory_context_changed",
                    );
                    self.fail_segment_speech_locked(&ns, &sid, &rid, "context_changed")
                        .await;
                }
                Err(error)
            }
            Ok(request) => {
                if self.delivery_mode.segments() {
                    self.generate_stream(&ns, &sid, &request, epoch, foreground, &mut cancellation)
                        .await
                } else {
                    async {
                        let reply = self
                            .generate_whole_cancellable(&request, &mut cancellation)
                            .await?;
                        validate_reply(&reply, &request)?;
                        let _guard = self.control.lock().await;
                        let current = self.current.lock().unwrap().clone();
                        let stores = self.stores.lock().unwrap();
                        let store = &stores[&ns];
                        let session = store.session(&sid)?;
                        ensure!(
                            current == Some((ns.clone(), sid.clone()))
                                && session["generation"] == generation
                                && store.learner(s(&session, "learner_id"))?["epoch"] == epoch,
                            "已取消的旧回复"
                        );
                        let tid = store.add_turn(
                            &sid,
                            &rid,
                            generation,
                            "assistant",
                            s(&reply, "reply_text"),
                            "generated",
                            &strings(&reply["used_evidence_ids"]),
                        )?;
                        let mut pending = self.pending.lock().unwrap();
                        pending.insert(
                            tid,
                            Pending {
                                sid: sid.clone(),
                                generation,
                                epoch,
                                reply,
                                _foreground: Some(foreground),
                            },
                        );
                        if pending.len() > 24 {
                            if let Some(id) = pending.keys().next().cloned() {
                                pending.remove(&id);
                            }
                        }
                        store.set_status(&sid, "idle")?;
                        Ok::<(), anyhow::Error>(())
                    }
                    .await
                }
            }
        };
        let status = match result {
            Ok(()) => {
                self.notify("tutor.reply");
                "completed"
            }
            Err(_) if *cancellation.borrow() => "cancelled",
            Err(error) => {
                self.bump_metric("failed");
                self.error(error.to_string().chars().take(240).collect());
                let stores = self.stores.lock().unwrap();
                if let Ok(session) = stores[&ns].session(&sid) {
                    if session["generation"] == generation {
                        let _ = stores[&ns].set_status(&sid, "error");
                    }
                }
                self.notify("system.error");
                "failed"
            }
        };
        let elapsed = start.elapsed().as_millis() as u64;
        self.metrics.lock().unwrap()["last_latency_ms"] = json!(elapsed);
        let _ = self.stores.lock().unwrap()[&ns].end_run(&rid, status, elapsed);
        self.running.lock().unwrap().remove(&rid);
    }
    async fn deliver(&self, sid: &str, tid: &str) -> Result<Value> {
        let _guard = self.control.lock().await;
        let (ns, session) = self.writable(sid)?;
        let stores = self.stores.lock().unwrap();
        let store = &stores[&ns];
        ensure!(
            !store.is_tutor_stream_turn(tid)?,
            "分句回复必须逐段确认显示"
        );
        let mut pending_replies = self.pending.lock().unwrap();
        store.tx(|| {
            store.acknowledge(sid, tid, "shown")?;
            if let Some(pending) = pending_replies.get(tid) {
                ensure!(
                    pending.sid == sid
                        && pending.generation == session["generation"].as_i64().unwrap_or(-1)
                        && pending.epoch
                            == store.learner(s(&session, "learner_id"))?["epoch"]
                                .as_i64()
                                .unwrap_or(-1),
                    "回复已过期"
                );
                apply_reply(store, sid, &pending.reply, pending.epoch)?;
            }
            Ok(())
        })?;
        // Retain the candidate until the acknowledgement and all memory updates
        // commit. A retry after a storage failure must still apply the reply.
        pending_replies.remove(tid);
        self.notify("memory.updated");
        Ok(json!({"ok":true}))
    }
    pub async fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.cancel_queued_memory_review();
        self.memory_worker.preempt();
        // Motion, playback and capture invalidation precede teaching locks and
        // owned-model cleanup. A busy lock cannot keep the microphone alive.
        self.ptz.close();
        self.audio.reserve_speech_epoch();
        self.audio.discard_recording();
        self.local_speech.preempt();
        self.local_asr.preempt(&self.audio);
        self.local_memory.preempt();
        let _ = self.final_asr.preempt(&self.audio);
        self.pause_voice_loop(None, "shutdown");
        self.ptz.stop_and_wait().await;
        {
            let _guard = self.control.lock().await;
            if let Some(task) = self.invalidate_playback_locked() {
                let _ = task.await;
            }
            if let Err(error) = self.stop_segment_speech_locked("shutdown").await {
                self.error(format!("关闭分句播报状态未保存：{error}"));
            }
        }
    }
    pub async fn close(&self) {
        self.begin_shutdown().await;
        {
            let _guard = self.control.lock().await;
            let _ = self.cancel().await;
        }
        tokio::join!(
            self.close_local_asr(),
            self.close_local_speech(),
            self.close_final_asr(),
            async {
                if let Err(error) = self.stop_local_memory().await {
                    self.error(error.to_string());
                }
            }
        );
        self.audio.close().await;
        if let Err(error) = self.provider.close().await {
            self.error(format!("Teaching cleanup incomplete: {error}"));
        }
        self.close_memory_review().await;
        // Server tasks may still need control for their stale-result check.
        // Let them finish after provider cleanup without holding that lock.
        let drain_started = Instant::now();
        while !self.running.lock().unwrap().is_empty() {
            if drain_started.elapsed() >= Duration::from_secs(5) {
                self.error("关闭时仍有教学任务在收尾".into());
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.camera.close();
        self.ptz.close();
        for store in self.stores.lock().unwrap().values() {
            let _ = store.checkpoint_wal();
        }
    }
}

fn attach_omni_frame(app: &Workstation, request: &mut Value) -> Result<Option<Value>> {
    let include = request
        .get("include_current_frame")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| anyhow!("当前画面提交选项无效"))
        })
        .transpose()?
        .unwrap_or(false);
    if !include {
        ensure!(
            request.get("camera_session_id").is_none() && request.get("frame_id").is_none(),
            "未明确提交当前画面时不能附带摄像头标识"
        );
        request
            .as_object_mut()
            .unwrap()
            .remove("include_current_frame");
        return Ok(None);
    }
    let session_id = text(request, "camera_session_id", 1, 128)?.to_owned();
    let frame_id = request["frame_id"]
        .as_u64()
        .ok_or_else(|| anyhow!("摄像头画面编号无效"))?;
    let (url, submitted) = app
        .camera
        .explicit_frame(&session_id, frame_id)
        .map_err(|error| anyhow!(error))?;
    let messages = request["messages"]
        .as_array()
        .ok_or_else(|| anyhow!("多模态请求需要消息"))?;
    ensure!(
        !messages
            .iter()
            .flat_map(|message| message["content"].as_array().into_iter().flatten())
            .any(|part| part["type"] == "image_url"),
        "每次请求只能提交一张明确选择的图片"
    );
    let content = request["messages"]
        .as_array_mut()
        .and_then(|messages| messages.last_mut())
        .filter(|message| message["role"] == "user")
        .and_then(|message| message.get_mut("content"))
        .ok_or_else(|| anyhow!("当前画面只能随最后一条孩子消息提交"))?;
    if let Some(text) = content.as_str() {
        *content = json!([{"type":"text","text":text}]);
    }
    let parts = content
        .as_array_mut()
        .ok_or_else(|| anyhow!("孩子消息格式无效"))?;
    ensure!(parts.len() < 16, "孩子消息内容过多");
    parts.push(json!({"type":"image_url","image_url":{"url":url}}));
    let object = request.as_object_mut().unwrap();
    object.remove("include_current_frame");
    object.remove("camera_session_id");
    object.remove("frame_id");
    Ok(Some(submitted))
}

fn allowed_keys(value: &Value, keys: &[&str]) -> Result<()> {
    let object = value.as_object().ok_or_else(|| anyhow!("需要JSON对象"))?;
    ensure!(
        object.keys().all(|k| keys.contains(&k.as_str())),
        "存在未允许的字段"
    );
    Ok(())
}
fn text<'a>(value: &'a Value, key: &str, min: usize, max: usize) -> Result<&'a str> {
    let result = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("缺少字段 {key}"))?;
    ensure!(
        (min..=max).contains(&result.chars().count()),
        "字段 {key} 长度不符合要求"
    );
    Ok(result)
}
fn api_error(error: anyhow::Error) -> Response {
    (
        StatusCode::BAD_REQUEST,
        axum::Json(json!({"detail":error.to_string()})),
    )
        .into_response()
}
fn success(value: Value) -> Response {
    axum::Json(value).into_response()
}
fn same_origin(headers: &HeaderMap) -> bool {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let valid = host
        .strip_prefix("127.0.0.1:")
        .or_else(|| host.strip_prefix("localhost:"))
        .and_then(|p| p.parse::<u16>().ok())
        .is_some();
    valid
        && headers
            .get(header::ORIGIN)
            .is_none_or(|o| o.to_str().ok() == Some(&format!("http://{host}")))
        && headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) != Some("cross-site")
}
fn authenticated(headers: &HeaderMap, token: &str) -> bool {
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    cookie.split(';').any(|c| {
        c.trim()
            .strip_prefix("ai_tutor_local=")
            .is_some_and(|v| constant_equal(v, token))
    })
}
fn constant_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}
async fn protect(
    State(app): State<Arc<Workstation>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !same_origin(request.headers()) {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(json!({"detail":"仅允许本机工作台访问"})),
        )
            .into_response();
    }
    if request.uri().path() == "/" {
        if let Some(query) = request.uri().query() {
            if let Some(token) = query.strip_prefix("token=") {
                if constant_equal(token, &app.token) {
                    return Response::builder()
                        .status(303)
                        .header(header::LOCATION, "/")
                        .header(
                            header::SET_COOKIE,
                            format!(
                                "ai_tutor_local={}; HttpOnly; SameSite=Strict; Path=/",
                                app.token
                            ),
                        )
                        .header(header::CACHE_CONTROL, "no-store")
                        .header("Referrer-Policy", "no-referrer")
                        .body(Body::empty())
                        .unwrap();
                }
                return (StatusCode::UNAUTHORIZED, "启动凭证无效").into_response();
            }
        }
    }
    if request.uri().path() == "/api/health" {
        return success(
            json!({"status":"ok","application":"ai-tutor-local","implementation":"Rust"}),
        );
    }
    if !authenticated(request.headers(), &app.token) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"detail":"请使用 Start AI Tutor 启动器打开工作台"})),
        )
            .into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (key, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("x-frame-options", "DENY"),
        (
            "content-security-policy",
            "default-src 'self'; img-src 'self' data: blob:; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'",
        ),
    ] {
        headers.insert(
            axum::http::HeaderName::from_static(key),
            axum::http::HeaderValue::from_static(value),
        );
    }
    response
}
async fn events(State(app): State<Arc<Workstation>>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move|socket|async move{let(mut send,mut receive)=socket.split();let mut events=app.events.subscribe();loop{tokio::select!{event=events.recv()=>match event{Ok(v)=>if send.send(Message::Text(v.to_string().into())).await.is_err(){break;},Err(broadcast::error::RecvError::Lagged(_))=>{},Err(_)=>break},message=receive.next()=>if message.is_none(){break;}}}})
}
async fn endpoint(
    State(app): State<Arc<Workstation>>,
    method: Method,
    uri: axum::http::Uri,
    body: Bytes,
) -> Response {
    let path = uri.path();
    if method == Method::GET && (path == "/" || path.starts_with("/assets/")) {
        let relative = if path == "/" {
            "index.html"
        } else {
            path.trim_start_matches('/')
        };
        if relative.contains("..") || relative.contains('\\') {
            return StatusCode::NOT_FOUND.into_response();
        }
        let file = app.root.join("frontend/dist").join(relative);
        return match tokio::fs::read(&file).await {
            Ok(bytes) => {
                let mime = match file.extension().and_then(|v| v.to_str()) {
                    Some("html") => "text/html; charset=utf-8",
                    Some("js") => "text/javascript; charset=utf-8",
                    Some("css") => "text/css; charset=utf-8",
                    Some("wasm") => "application/wasm",
                    Some("svg") => "image/svg+xml",
                    Some("json") | Some("webmanifest") => "application/json; charset=utf-8",
                    _ => "application/octet-stream",
                };
                ([(header::CONTENT_TYPE, mime)], bytes).into_response()
            }
            Err(_) => StatusCode::NOT_FOUND.into_response(),
        };
    }
    let data = if body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(v) => v,
            Err(_) => return api_error(anyhow!("JSON格式不正确")),
        }
    };
    match dispatch(app, &method, path, &data).await {
        Ok(v) => success(v),
        Err(e) => api_error(e),
    }
}
async fn dispatch(
    app: Arc<Workstation>,
    method: &Method,
    path: &str,
    data: &Value,
) -> Result<Value> {
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    match (method.as_str(), parts.as_slice()) {
        ("POST", ["api", "omni", "infer"]) => {
            allowed_keys(
                data,
                &[
                    "request_id",
                    "learner_id",
                    "messages",
                    "modalities",
                    "max_tokens",
                    "audio",
                    "include_current_frame",
                    "camera_session_id",
                    "frame_id",
                ],
            )?;
            let mut request = data.clone();
            if s(&request, "request_id").is_empty() {
                request["request_id"] = json!(uid("omni"));
            }
            ensure!(s(&request, "request_id").len() <= 128, "请求编号过长");
            let learner_id = s(&request, "learner_id");
            ensure!(
                !learner_id.is_empty() && learner_id.len() <= 128,
                "多模态请求需要明确的学习者档案"
            );
            {
                let current = app
                    .current
                    .lock()
                    .unwrap()
                    .clone()
                    .ok_or_else(|| anyhow!("请先选择当前学习活动"))?;
                let stores = app.stores.lock().unwrap();
                let session = stores[&current.0].session(&current.1)?;
                ensure!(
                    s(&session, "learner_id") == learner_id,
                    "多模态请求只能使用当前学习者档案"
                );
            }
            let submitted_frame = attach_omni_frame(&app, &mut request)?;
            let id = s(&request, "request_id").to_owned();
            let mut response = app.provider.infer_omni(request).await?;
            response["request_id"] = json!(id);
            if let Some(frame) = submitted_frame {
                response["submitted_frame"] = json!(frame);
            }
            Ok(response)
        }
        ("POST", ["api", "omni", "cancel"]) => {
            let id = s(&data, "request_id");
            ensure!(!id.is_empty(), "缺少多模态请求编号");
            app.provider.cancel(&id).await?;
            Ok(json!({"cancelled":true,"request_id":id}))
        }
        ("GET", ["api", "state"]) => {
            // Capture adoption changes the session generation and lease together.
            let _control = app.control.lock().await;
            app.state()
        }
        ("GET", ["api", "parent", "digest"]) => {
            let _control = app.control.lock().await;
            app.parent_digest()
        }
        ("GET", ["api", "ui", "preferences"]) => app.ui_preferences.lock().unwrap().get(),
        ("POST", ["api", "ui", "preferences"]) => {
            allowed_keys(data, &["ui_language"])?;
            let value = app.ui_preferences.lock().unwrap().set_language(text(
                data,
                "ui_language",
                2,
                2,
            )?)?;
            app.notify("ui.settings");
            Ok(value)
        }
        ("GET", ["api", "camera", "pose"]) => Ok(app.camera.status()["pose"].clone()),
        ("GET", ["api", "camera", "native-pose"]) => app
            .camera
            .next_native_pose()
            .await
            .map_err(anyhow::Error::msg),
        ("GET", ["api", "devices", "cameras"]) => {
            let camera = app.camera.clone();
            Ok(json!(
                tokio::task::spawn_blocking(move || camera.discover())
                    .await?
                    .map_err(anyhow::Error::msg)?
            ))
        }
        ("POST", ["api", "camera", "open"]) => {
            allowed_keys(data, &["device_id"])?;
            app.ptz.stop();
            let _guard = app.camera_control.lock().await;
            ensure!(!app.shutting_down.load(Ordering::SeqCst), "应用正在关闭");
            app.ptz.stop_and_wait().await;
            let camera = app.camera.clone();
            let id = data["device_id"].as_str().map(str::to_owned);
            tokio::task::spawn_blocking(move || camera.open(id))
                .await?
                .map_err(anyhow::Error::msg)
        }
        ("POST", ["api", "camera", "close"]) => {
            app.ptz.stop();
            let _guard = app.camera_control.lock().await;
            app.ptz.stop_and_wait().await;
            Ok(app.camera.close())
        }
        ("GET", ["api", "video", "frame"]) => Ok(app.camera.frame()),
        ("GET", ["api", "camera", "frame-meta"]) => Ok(app.camera.preview_snapshot().0),
        ("POST", ["api", "tracking", "select"]) => {
            allowed_keys(data, &["track_key"])?;
            let key = text(data, "track_key", 1, 150)?;
            app.ptz.stop();
            let _guard = app.camera_control.lock().await;
            ensure!(!app.shutting_down.load(Ordering::SeqCst), "应用正在关闭");
            app.ptz.stop_and_wait().await;
            app.camera.select(key).map_err(anyhow::Error::msg)
        }
        ("POST", ["api", "tracking", "clear"]) => {
            app.ptz.stop();
            let _guard = app.camera_control.lock().await;
            app.ptz.stop_and_wait().await;
            Ok(app.camera.clear())
        }
        ("POST", ["api", "learners"]) => {
            allowed_keys(data, &["name", "language", "help_style", "namespace"])?;
            let name = text(data, "name", 1, 48)?.trim();
            ensure!(!name.is_empty(), "请输入匿名档案名称");
            let ns = data["namespace"].as_str().unwrap_or("real");
            let language = data["language"].as_str().unwrap_or("zh");
            let style = data["help_style"].as_str().unwrap_or("one_step");
            ensure!(
                ["real", "simulator"].contains(&ns)
                    && ["zh", "en", "zh-CN", "en-US"].contains(&language)
                    && ["one_step", "guided", "direct"].contains(&style),
                "设置不符合允许值"
            );
            app.stores.lock().unwrap()[ns].create_learner(name, language, style)
        }
        ("POST", ["api", "sessions"]) => {
            allowed_keys(data, &["learner_id", "resume"])?;
            app.select(
                text(data, "learner_id", 1, 80)?,
                data["resume"].as_bool().unwrap_or(true),
            )
            .await
        }
        ("GET", ["api", "learners", lid, "sessions"]) => app.activity_catalog(lid, None, 20),
        ("POST", ["api", "learners", lid, "sessions", source_sid, "continue"]) => {
            app.continue_activity(lid, source_sid, data).await
        }
        ("POST", ["api", "sessions", sid, "turns"]) => {
            allowed_keys(
                data,
                &[
                    "text",
                    "auto_speak",
                    "client_submission_id",
                    "expected_generation",
                    "voice_loop",
                    "recording_generation",
                ],
            )?;
            let expected_generation = recording::expected_generation(data)?;
            let auto_speak = match data.get("auto_speak") {
                Some(value) => value.as_bool().ok_or_else(|| anyhow!("自动朗读选项无效"))?,
                None => false,
            };
            let client_submission_id = match data.get("client_submission_id") {
                Some(value) => Some(
                    value
                        .as_str()
                        .filter(|s| {
                            !s.is_empty()
                                && s.len() <= 96
                                && s.bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
                        })
                        .ok_or_else(|| anyhow!("提交关联标识无效"))?,
                ),
                None => None,
            };
            let value = text(data, "text", 1, 4000)?.trim();
            ensure!(!value.is_empty(), "请输入内容");
            let loop_tag = voice_loop::LoopTag::parse(data.get("voice_loop"))?;
            let recording_generation = data
                .get("recording_generation")
                .map(|v| v.as_u64().ok_or_else(|| anyhow!("录音代次无效")))
                .transpose()?;
            ensure!(
                loop_tag.is_some() == recording_generation.is_some(),
                "轮换标识和录音代次必须同时提供"
            );
            let result = if loop_tag.is_none() {
                app.submit_with_speech(
                    sid,
                    value,
                    auto_speak,
                    client_submission_id,
                    expected_generation,
                )
                .await
            } else {
                app.submit_with_speech_loop(
                    sid,
                    value,
                    auto_speak,
                    client_submission_id,
                    expected_generation,
                    loop_tag.as_ref(),
                    recording_generation,
                )
                .await
            };
            result
        }
        ("POST", ["api", "sessions", sid, "voice-loop", "enable"]) => {
            app.enable_voice_loop(sid, data).await
        }
        ("POST", ["api", "sessions", sid, "voice-loop", id, "heartbeat"]) => {
            app.heartbeat_voice_loop(sid, id, data).await
        }
        ("POST", ["api", "sessions", sid, "voice-loop", id, "stop"]) => {
            app.stop_voice_loop(sid, id, data).await
        }
        ("POST", ["api", "sessions", sid, "delivery"]) => {
            allowed_keys(data, &["turn_id", "status"])?;
            ensure!(s(data, "status") == "shown", "无效投递状态");
            app.deliver(sid, text(data, "turn_id", 1, 80)?).await
        }
        ("GET", ["api", "sessions", sid, "streams", rid]) => {
            ensure!(
                !sid.is_empty() && sid.len() <= 80 && !rid.is_empty() && rid.len() <= 80,
                "教学流编号无效"
            );
            app.tutor_stream(sid, rid).await
        }
        ("POST", ["api", "sessions", sid, "streams", rid, "display"]) => {
            allowed_keys(data, &["generation", "sequence"])?;
            let generation = data["generation"]
                .as_i64()
                .filter(|v| *v > 0)
                .ok_or_else(|| anyhow!("教学显示代次无效"))?;
            let sequence = data["sequence"]
                .as_u64()
                .filter(|v| *v < 12)
                .ok_or_else(|| anyhow!("教学显示编号无效"))? as usize;
            app.display_segment(sid, rid, generation, sequence).await
        }
        ("POST", ["api", "sessions", sid, "streams", rid, "speech", "close"]) => {
            allowed_keys(data, &["generation", "reason"])?;
            ensure!(
                !sid.is_empty() && sid.len() <= 80 && !rid.is_empty() && rid.len() <= 80,
                "教学流编号无效"
            );
            let generation = data["generation"]
                .as_i64()
                .filter(|v| *v > 0)
                .ok_or_else(|| anyhow!("教学停声代次无效"))?;
            app.pause_voice_loop_reply(sid, rid, generation, "speech_closed");
            app.close_segment_speech(sid, rid, generation, text(data, "reason", 1, 32)?)
                .await
        }
        ("POST", ["api", "sessions", sid, "cancel"]) => {
            let _guard = app.control.lock().await;
            app.active(sid)?;
            app.cancel().await?;
            app.audio.discard_recording();
            Ok(json!({"ok":true}))
        }
        ("POST", ["api", "sessions", sid, "finish"]) => {
            app.ptz.stop();
            let _camera_guard = app.camera_control.lock().await;
            app.ptz.stop_and_wait().await;
            let _guard = app.control.lock().await;
            let (ns, session) = app.active(sid)?;
            if s(&session, "status") != "finished" {
                app.cancel().await?;
            }
            app.audio.discard_recording();
            let result = app.stores.lock().unwrap()[&ns].consolidate_and_finish(sid)?;
            app.notify("memory.updated");
            Ok(result)
        }
        ("GET", ["api", "sessions", sid, "context"]) => {
            let (_, session) = app.active(sid)?;
            Ok(
                json!({"fast_memory":session["state"],"recalled_memories":session["recalled_memories"]}),
            )
        }
        ("POST", ["api", "sessions", sid, "settings"]) => {
            allowed_keys(
                data,
                &["language", "help_style", "reading_level", "speech_speed"],
            )?;
            ensure!(
                data.as_object().is_some_and(|v| !v.is_empty()),
                "请提供需要修改的设置"
            );
            for (field, valid) in [
                ("language", &["zh", "en", "zh-CN", "en-US"][..]),
                ("help_style", &["one_step", "guided", "direct"][..]),
                ("reading_level", &["plain", "standard", "technical"][..]),
                ("speech_speed", &["slow", "normal", "fast"][..]),
            ] {
                if let Some(value) = data.get(field) {
                    ensure!(
                        value.as_str().is_some_and(|s| valid.contains(&s)),
                        "无效设置: {field}"
                    );
                }
            }
            let _guard = app.control.lock().await;
            let (ns, session) = app.writable(sid)?;
            let lid = s(&session, "learner_id");
            let previous = app.stores.lock().unwrap()[&ns].learner(lid)?;
            let reset_session_help = data.get("help_style").is_some()
                && session["state"]["session_preferences"]
                    .as_object()
                    .is_some_and(|v| !v.is_empty());
            let teaching_changed = reset_session_help
                || ["language", "help_style", "reading_level"]
                    .iter()
                    .any(|field| {
                        data.get(*field)
                            .is_some_and(|value| value != &previous[*field])
                    });
            if teaching_changed {
                app.cancel().await?;
                app.audio.discard_recording();
            } else if data
                .get("speech_speed")
                .is_some_and(|value| value != &previous["speech_speed"])
            {
                app.pause_voice_loop(None, "settings_changed");
                app.stop_playback_locked().await?;
            }
            let stores = app.stores.lock().unwrap();
            let store = &stores[&ns];
            let saved = store.tx(|| {
                store.settings(
                    lid,
                    data["language"]
                        .as_str()
                        .unwrap_or(s(&previous, "language")),
                    data["help_style"]
                        .as_str()
                        .unwrap_or(s(&previous, "help_style")),
                )?;
                store.update_learner_preferences(
                    lid,
                    data["reading_level"].as_str(),
                    data["speech_speed"].as_str(),
                )?;
                if reset_session_help {
                    let mut current_state = store.session(sid)?["state"].clone();
                    current_state["session_preferences"] = json!({});
                    store.checkpoint(sid, &current_state)?;
                }
                store.learner(lid)
            })?;
            app.notify("settings.updated");
            Ok(json!({"ok":true,"settings":saved}))
        }
        ("POST", ["api", "activities", aid, "events"]) => {
            allowed_keys(data, &["modules", "source_type", "report"])?;
            let _guard = app.control.lock().await;
            let current = app
                .current
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| anyhow!("请先选择档案"))?;
            let (ns, session) = app.writable(&current.1)?;
            ensure!(s(&session, "activity_id") == *aid, "活动已过期");
            let source = text(data, "source_type", 1, 30)?;
            ensure!(
                ["learner_report", "operator_confirmed", "simulated"].contains(&source),
                "未接入真实硬件"
            );
            ensure!(
                source != "simulated" || ns == "simulator",
                "请使用独立模拟档案"
            );
            let modules = if data["modules"].is_null() {
                None
            } else {
                let modules = data["modules"]
                    .as_array()
                    .ok_or_else(|| anyhow!("模块列表格式不正确"))?;
                ensure!(
                    modules.len() <= 4
                        && modules.iter().all(|m| m
                            .as_str()
                            .is_some_and(
                                |v| ["control", "motor", "distance", "light"].contains(&v)
                            )),
                    "模块值不符合协议"
                );
                Some(modules.clone())
            };
            ensure!(
                data["report"].as_str().unwrap_or("").chars().count() <= 1000,
                "报告过长"
            );
            app.cancel().await?;
            let stores = app.stores.lock().unwrap();
            let store = &stores[&ns];
            let kind = if data["report"]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty())
            {
                "observation"
            } else {
                "learner_statement"
            };
            let event = store.record_education_event(
                s(&session, "learner_id"),
                &current.1,
                source,
                kind,
                data,
            )?;
            let eid = s(&event, "event_id").to_owned();
            let mut fast = store.session(&current.1)?["state"].clone();
            let origin = if ns == "simulator" {
                "simulated"
            } else {
                source
            };
            if let Some(modules) = modules {
                fast["modules"] = json!(modules);
                fast["modules_source"] = json!(origin);
                fast["requires_inventory_confirmation"] = json!(false);
                fast["field_evidence"]["modules"] =
                    json!({"source_event_ids":[eid],"hardware_verified":false});
            }
            if let Some(report) = data["report"]
                .as_str()
                .filter(|report| !report.trim().is_empty())
            {
                fast["reported_result"] = json!(report);
                fast["reported_result_source"] = json!(origin);
                fast["field_evidence"]["reported_result"] =
                    json!({"source_event_ids":[eid],"hardware_verified":false});
                let attempts = fast["attempts"]
                    .as_array_mut()
                    .ok_or_else(|| anyhow!("活动观察列表格式不正确"))?;
                attempts.push(json!({"report":report,"source_event_ids":[eid],"hardware_verified":false,"historical":false,"original_source_type":origin}));
                if attempts.len() > 12 {
                    attempts.drain(..attempts.len() - 12);
                }
            }
            store.checkpoint(&current.1, &fast)?;
            Ok(json!({"event_id":eid,"hardware_verified":false}))
        }
        ("GET", ["api", "learners", lid, "memories"]) => {
            let ns = app.owner(lid)?;
            Ok(json!(app.stores.lock().unwrap()[&ns].memories(lid)?))
        }
        ("GET", ["api", "learners", lid, "sessions", sid, "memory-review"]) => {
            app.memory_review_snapshot(lid, sid)
        }
        ("POST", ["api", "learners", lid, "sessions", sid, "memory-review"]) => {
            app.start_memory_review(lid, sid, data).await
        }
        (
            "POST",
            [
                "api",
                "learners",
                lid,
                "sessions",
                sid,
                "memory-review",
                "cancel",
            ],
        ) => {
            allowed_keys(data, &[])?;
            app.cancel_memory_review(lid, sid).await
        }
        (
            "POST",
            [
                "api",
                "learners",
                lid,
                "sessions",
                sid,
                "memory-review",
                "candidates",
                candidate,
            ],
        ) => {
            allowed_keys(data, &["decision"])?;
            app.decide_memory_review(lid, sid, candidate, text(data, "decision", 1, 16)?)
                .await
        }
        ("POST", ["api", "learners", lid, "sessions", sid, "help-preference"]) => {
            let _control = app.control.lock().await;
            ensure!(!app.shutting_down.load(Ordering::SeqCst), "工作台正在关闭");
            let (ns, session) = app.writable(sid)?;
            ensure!(
                s(&session, "learner_id") == *lid,
                "讲解偏好档案与会话不一致"
            );
            // This explicit action advances the learner's memory epoch inside
            // its transaction. Never cancel/bump the session before validating
            // the request, and never repeat that write for a lost-response retry.
            let response = app.stores.lock().unwrap()[&ns].save_help_preference(lid, sid, data)?;
            if response["replayed"] != true {
                app.memory_worker.preempt();
                *app.last_input.lock().unwrap() = Instant::now();
                app.notify("memory.updated");
            }
            Ok(response)
        }
        ("GET", ["api", "learners", lid, "sessions", sid, "comparisons"]) => {
            let ns = app.owner(lid)?;
            app.stores.lock().unwrap()[&ns].comparisons(lid, sid)
        }
        ("POST", ["api", "learners", lid, "sessions", sid, "comparisons"]) => {
            let _control = app.control.lock().await;
            ensure!(!app.shutting_down.load(Ordering::SeqCst), "工作台正在关闭");
            let (ns, session) = app.writable(sid)?;
            ensure!(
                s(&session, "learner_id") == *lid,
                "比较记录档案与会话不一致"
            );
            // Validate before cancelling; a lost response may be retried while a
            // newer lesson is already running. That retry must not stop it.
            let replay = app.stores.lock().unwrap()[&ns].comparison_replay(lid, sid, data)?;
            if let Some(response) = replay {
                return Ok(response);
            }
            app.cancel().await?;
            app.audio.discard_recording();
            let response = app.stores.lock().unwrap()[&ns].save_comparison(lid, sid, data)?;
            *app.last_input.lock().unwrap() = Instant::now();
            app.notify("activity.updated");
            Ok(response)
        }
        ("GET", ["api", "learners", lid, "memories", mid, "evidence"]) => {
            let ns = app.owner(lid)?;
            app.stores.lock().unwrap()[&ns].memory_evidence(lid, mid)
        }
        ("DELETE", ["api", "learners", lid, "data"])
        | ("DELETE", ["api", "learners", lid, "memories"]) => forget(&app, lid, None, None).await,
        ("DELETE", ["api", "learners", lid, "memories", mid]) => {
            forget(&app, lid, Some(mid), None).await
        }
        ("PATCH", ["api", "learners", lid, "memories", mid]) => {
            allowed_keys(data, &["content"])?;
            forget(&app, lid, Some(mid), Some(text(data, "content", 1, 800)?)).await
        }
        ("GET", ["api", "audio", "devices"]) => {
            let audio = app.audio.clone();
            tokio::task::spawn_blocking(move || audio.devices())
                .await
                .map_err(|_| anyhow!("麦克风设备列表工作线程异常"))?
        }
        ("GET", ["api", "audio", "status"]) => Ok(app.audio.status()),
        ("POST", ["api", "local-memory", "retry"]) => {
            allowed_keys(data, &[])?;
            app.retry_local_memory().await
        }
        ("POST", ["api", "local-memory", "stop"]) => {
            allowed_keys(data, &[])?;
            app.stop_local_memory().await
        }
        ("POST", ["api", "audio", "local-speech", "retry"]) => {
            allowed_keys(data, &[])?;
            app.retry_local_speech().await
        }
        ("POST", ["api", "audio", "local-speech", "stop"]) => {
            allowed_keys(data, &[])?;
            app.pause_voice_loop(None, "speech_service_stopped");
            app.stop_local_speech().await
        }
        ("POST", ["api", "audio", "local-asr", "retry"]) => {
            allowed_keys(data, &[])?;
            app.retry_local_asr().await
        }
        ("POST", ["api", "audio", "final-asr", "retry"]) => {
            allowed_keys(data, &[])?;
            app.retry_final_asr().await
        }
        ("POST", ["api", "audio", "final-asr", "stop"]) => {
            allowed_keys(data, &[])?;
            app.stop_final_asr().await
        }
        ("POST", ["api", "audio", "local-asr", "stop"]) => {
            allowed_keys(data, &[])?;
            app.pause_voice_loop(None, "asr_service_stopped");
            app.stop_local_asr().await
        }
        ("POST", ["api", "audio", "record", "start"]) => app.start_bound_recording(data).await,
        ("POST", ["api", "audio", "record", "discard"]) => app.discard_bound_recording(data).await,
        ("POST", ["api", "audio", "record", "stop"]) => app.stop_bound_recording(data).await,
        ("POST", ["api", "audio", "playback", "stop"]) => {
            app.pause_voice_loop(None, "manual_stop");
            app.stop_playback().await?;
            Ok(json!({"ok":true}))
        }
        ("POST", ["api", "audio", "playback"]) => {
            allowed_keys(data, &["session_id", "turn_id"])?;
            app.playback(
                text(data, "session_id", 1, 80)?,
                text(data, "turn_id", 1, 80)?,
            )
            .await
        }
        ("POST", ["api", "ptz", "stop"]) => {
            app.ptz.stop();
            let _guard = app.camera_control.lock().await;
            Ok(app.ptz.stop_and_wait().await)
        }
        ("POST", ["api", "ptz", "enable"]) => {
            allowed_keys(
                data,
                &[
                    "mode",
                    "camera_session_id",
                    "track_key",
                    "target_generation",
                ],
            )?;
            ensure!(
                data["mode"].as_str().unwrap_or("external_yolo") == "external_yolo",
                "仅支持已标定的外部人脸跟随模式"
            );
            ensure!(!app.shutting_down.load(Ordering::SeqCst), "应用正在关闭");
            let epoch = text(data, "camera_session_id", 1, 80)?;
            let track = text(data, "track_key", 1, 150)?;
            let target_generation = data["target_generation"]
                .as_u64()
                .ok_or_else(|| anyhow!("缺少明确目标代次"))?;
            let intent = app.ptz.intent_generation();
            let _guard = app.camera_control.lock().await;
            ensure!(
                !app.shutting_down.load(Ordering::SeqCst) && intent == app.ptz.intent_generation(),
                "启用请求已经过期，请重新选择并启用"
            );
            let camera = app.camera.status();
            ensure!(
                camera["camera_session_id"] == epoch
                    && camera["selected_track_key"] == track
                    && camera["target_generation"] == target_generation,
                "视觉目标已变化，请重新启用"
            );
            app.ptz
                .enable_external_for_intent(intent)
                .await
                .map_err(anyhow::Error::msg)
        }
        ("POST", ["api", "shutdown"]) => {
            app.begin_shutdown().await;
            app.shutdown.notify_one();
            Ok(json!({"ok":true}))
        }
        _ => bail!("接口不存在"),
    }
}
async fn forget(
    app: &Workstation,
    lid: &str,
    mid: Option<&&str>,
    correction: Option<&str>,
) -> Result<Value> {
    let _guard = app.control.lock().await;
    let ns = app.owner(lid)?;
    let current = app.current.lock().unwrap().clone();
    let active = if let Some((namespace, sid)) = current {
        app.stores.lock().unwrap()[&namespace]
            .session(&sid)
            .is_ok_and(|session| s(&session, "learner_id") == lid)
    } else {
        false
    };
    if active {
        app.cancel().await?;
        app.audio.discard_recording();
    }
    app.provider.cleanup_owned(lid).await?;
    let stores = app.stores.lock().unwrap();
    let store = &stores[&ns];
    if let Some(content) = correction {
        let id = store.correct_memory(lid, mid.ok_or_else(|| anyhow!("缺少记忆编号"))?, content)?;
        Ok(json!({"memory_id":id}))
    } else {
        store.forget(lid, mid.copied())?;
        Ok(json!({"ok":true,"context_reset":true}))
    }
}
#[derive(serde::Deserialize)]
struct HistoryQuery {
    before: Option<i64>,
    limit: Option<usize>,
}

async fn conversation_history(
    State(app): State<Arc<Workstation>>,
    Path((lid, sid)): Path<(String, String)>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let result = (|| {
        let namespace = app.owner(&lid)?;
        app.stores.lock().unwrap()[&namespace].turn_history_page(
            &lid,
            &sid,
            query.before,
            query.limit.unwrap_or(50),
        )
    })();
    match result {
        Ok(page) => success(page),
        Err(error) => api_error(error),
    }
}

pub fn router(app: Arc<Workstation>) -> Router {
    Router::new()
        .route("/api/events", get(events))
        .route("/api/video/stream", get(crate::video_stream::upgrade))
        .route(
            "/api/learners/{lid}/sessions",
            get(activity_history::catalog),
        )
        .route(
            "/api/learners/{lid}/sessions/{sid}/history",
            get(conversation_history),
        )
        .route(
            "/api/learners/{lid}/sessions/{sid}/comparison-history",
            get(activity_history::comparison_history),
        )
        .route(
            "/api/omni/infer",
            any(endpoint).layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route("/", any(endpoint))
        .route("/{*path}", any(endpoint))
        .layer(DefaultBodyLimit::max(24000))
        .layer(middleware::from_fn_with_state(app.clone(), protect))
        .with_state(app)
}

#[cfg(test)]
#[path = "experiment_api_tests.rs"]
mod experiment_api_tests;

#[cfg(test)]
#[path = "help_preference_api_tests.rs"]
mod help_preference_api_tests;

#[cfg(test)]
#[path = "voice_loop_integration_tests.rs"]
mod voice_loop_integration_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn report_sources_and_memory_explanations_keep_owner_and_evidence_type() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = Workstation::new(dir.path().into(), "fixture-token".into())?;
        let (lid, other, sid, aid) = {
            let stores = app.stores.lock().unwrap();
            let store = &stores["real"];
            let learner = store.create_learner("synthetic evidence", "zh", "guided")?;
            let other = store.create_learner("other synthetic profile", "en", "guided")?;
            let sid = store.create_session(s(&learner, "learner_id"))?;
            (
                s(&learner, "learner_id").to_owned(),
                s(&other, "learner_id").to_owned(),
                s(&sid, "session_id").to_owned(),
                s(&sid, "activity_id").to_owned(),
            )
        };
        *app.current.lock().unwrap() = Some(("real".into(), sid.clone()));
        let input = json!({"modules":["motor"],"source_type":"learner_report","report":"我打算明天再试一次，今天没有完成。"});
        let response = dispatch(
            app.clone(),
            &Method::POST,
            &format!("/api/activities/{aid}/events"),
            &input,
        )
        .await?;
        let mid = {
            let stores = app.stores.lock().unwrap();
            let store = &stores["real"];
            let source = store.source_events(&lid, &[s(&response, "event_id").to_owned()])?;
            ensure!(source[0]["event_kind"] == "observation");
            ensure!(source[0]["source_type"] == "learner_report");
            ensure!(source[0]["content"]["report"] == input["report"]);
            store.add_memory(&lid, &json!({"kind":"reported_progress","content":input["report"],"scope":"current_activity","source_event_ids":[response["event_id"]]}), 0)?.unwrap()
        };
        let path = format!("/api/learners/{lid}/memories/{mid}/evidence");
        let explanation = dispatch(app.clone(), &Method::GET, &path, &json!({})).await?;
        ensure!(explanation["sources"][0]["content"]["report"] == input["report"]);
        ensure!(
            dispatch(
                app.clone(),
                &Method::GET,
                &format!("/api/learners/{other}/memories/{mid}/evidence"),
                &json!({})
            )
            .await
            .is_err()
        );
        app.stores.lock().unwrap()["real"].forget(&lid, Some(&mid))?;
        ensure!(
            dispatch(app.clone(), &Method::GET, &path, &json!({}))
                .await
                .is_err()
        );
        app.close().await;
        Ok(())
    }
    #[tokio::test]
    async fn independent_settings_preserve_fields_and_cancel_only_teaching_changes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = Workstation::new(dir.path().into(), "fixture-token".into())?;
        let (lid, sid) = {
            let stores = app.stores.lock().unwrap();
            let store = &stores["real"];
            let learner = store.create_learner("settings fixture", "zh", "guided")?;
            let lid = s(&learner, "learner_id").to_owned();
            let session = store.create_session(&lid)?;
            (lid, s(&session, "session_id").to_owned())
        };
        *app.current.lock().unwrap() = Some(("real".into(), sid.clone()));
        let path = format!("/api/sessions/{sid}/settings");
        let recording_generation = app.audio.status()["recording_generation"].as_u64().unwrap();
        dispatch(
            app.clone(),
            &Method::POST,
            "/api/ui/preferences",
            &json!({"ui_language":"en"}),
        )
        .await?;
        {
            let stores = app.stores.lock().unwrap();
            assert_eq!(stores["real"].session(&sid)?["generation"], 0);
            assert_eq!(stores["real"].learner(&lid)?["language"], "zh");
        }
        for invalid in [
            json!({}),
            json!({"reading_level":"child"}),
            json!({"speech_speed":null}),
            json!({"language":"en","unexpected":true}),
        ] {
            assert!(
                dispatch(app.clone(), &Method::POST, &path, &invalid)
                    .await
                    .is_err()
            );
        }
        dispatch(
            app.clone(),
            &Method::POST,
            &path,
            &json!({"speech_speed":"fast"}),
        )
        .await?;
        assert_eq!(
            app.audio.status()["recording_generation"],
            recording_generation
        );
        {
            let stores = app.stores.lock().unwrap();
            assert_eq!(stores["real"].session(&sid)?["generation"], 0);
            assert_eq!(stores["real"].learner(&lid)?["help_style"], "guided");
        }
        dispatch(
            app.clone(),
            &Method::POST,
            &path,
            &json!({"reading_level":"plain"}),
        )
        .await?;
        // Legacy callers send only the two original fields and must not reset newer ones.
        dispatch(
            app.clone(),
            &Method::POST,
            &path,
            &json!({"language":"en","help_style":"one_step"}),
        )
        .await?;
        {
            let stores = app.stores.lock().unwrap();
            let saved = stores["real"].learner(&lid)?;
            assert_eq!(saved["reading_level"], "plain");
            assert_eq!(saved["speech_speed"], "fast");
            assert_eq!(stores["real"].session(&sid)?["generation"], 2);
            let mut state = stores["real"].session(&sid)?["state"].clone();
            state["session_preferences"] = json!({"help_style":"direct","explicit_request":"Show a full explanation this time"});
            stores["real"].checkpoint(&sid, &state)?;
        }
        assert_eq!(
            app.audio.status()["recording_generation"],
            recording_generation + 2
        );
        // Re-selecting the saved value still explicitly clears the current override.
        dispatch(
            app.clone(),
            &Method::POST,
            &path,
            &json!({"help_style":"one_step"}),
        )
        .await?;
        {
            let stores = app.stores.lock().unwrap();
            let session = stores["real"].session(&sid)?;
            assert_eq!(session["generation"], 3);
            assert_eq!(session["state"]["session_preferences"], json!({}));
        }
        let prefs = app.state()?["ui_preferences"].clone();
        assert_eq!(prefs["ui_language"], "en");
        app.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn recording_stop_requires_session_and_generation_before_touching_audio() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = Workstation::new(dir.path().into(), "fixture-token".into())?;
        let sid = {
            let stores = app.stores.lock().unwrap();
            let store = &stores["real"];
            let learner = store.create_learner("A", "zh", "one_step")?;
            s(
                &store.create_session(s(&learner, "learner_id"))?,
                "session_id",
            )
            .to_owned()
        };
        *app.current.lock().unwrap() = Some(("real".into(), sid.clone()));
        let before = app.audio.status();
        for payload in [
            json!({}),
            json!({"session_id":sid}),
            json!({"session_id":sid,"recording_generation":-1}),
            json!({"session_id":"previous-session","recording_generation":before["recording_generation"]}),
        ] {
            for path in ["/api/audio/record/stop", "/api/audio/record/discard"] {
                ensure!(
                    dispatch(app.clone(), &Method::POST, path, &payload)
                        .await
                        .is_err()
                );
            }
            let after = app.audio.status();
            ensure!(after["recording_generation"] == before["recording_generation"]);
            ensure!(after["recording"] == false && after["state"] != "transcribing");
        }
        app.close().await;
        Ok(())
    }
    #[tokio::test]
    async fn failed_delivery_retains_pending_and_retry_commits_once() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = Workstation::new(dir.path().into(), "fixture-token".into())?;
        let (sid, lid, tid) = {
            let stores = app.stores.lock().unwrap();
            let store = &stores["real"];
            let learner = store.create_learner("A", "zh", "one_step")?;
            let lid = s(&learner, "learner_id").to_owned();
            let session = store.create_session(&lid)?;
            let sid = s(&session, "session_id").to_owned();
            let event = store.event(&lid, &sid, "learner_report", &json!({"text":"停车成功"}))?;
            let tid = store.add_turn(
                &sid,
                "r",
                0,
                "assistant",
                "已记录观察",
                "generated",
                &[event.clone()],
            )?;
            let reply = json!({"task_update_candidates":[{"field":"reported_result","value":"停车成功","source_event_ids":[event]}],"memory_candidates":[{"kind":"reported_progress","content":"停车成功","scope":"current_activity","source_event_ids":[event]}]});
            app.pending.lock().unwrap().insert(
                tid.clone(),
                Pending {
                    sid: sid.clone(),
                    generation: 0,
                    epoch: 0,
                    reply,
                    _foreground: None,
                },
            );
            store.db.execute_batch("CREATE TEMP TRIGGER fail_memory BEFORE INSERT ON memories BEGIN SELECT RAISE(ABORT,'synthetic storage failure'); END;")?;
            (sid, lid, tid)
        };
        *app.current.lock().unwrap() = Some(("real".into(), sid.clone()));
        ensure!(app.deliver(&sid, &tid).await.is_err());
        ensure!(app.pending.lock().unwrap().contains_key(&tid));
        {
            let stores = app.stores.lock().unwrap();
            let store = &stores["real"];
            let session = store.session(&sid)?;
            ensure!(session["turns"][0]["status"] == "generated");
            ensure!(
                session["state"]["last_delivered_hint"] == ""
                    && session["state"]["reported_result"] == ""
            );
            ensure!(store.memories(&lid)?.is_empty());
            store.db.execute_batch("DROP TRIGGER fail_memory;")?;
        }
        app.deliver(&sid, &tid).await?;
        app.deliver(&sid, &tid).await?;
        ensure!(!app.pending.lock().unwrap().contains_key(&tid));
        {
            let stores = app.stores.lock().unwrap();
            let store = &stores["real"];
            let session = store.session(&sid)?;
            ensure!(session["turns"][0]["status"] == "shown");
            ensure!(session["state"]["reported_result"] == "停车成功");
            ensure!(store.memories(&lid)?.len() == 1);
            ensure!(session["state"]["attempts"].as_array().unwrap().len() == 1);
        }
        app.close().await;
        Ok(())
    }
    #[test]
    fn omni_current_frame_is_attached_once_and_never_implicitly() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = Workstation::new(dir.path().into(), "fixture-token".into())?;
        let mut ordinary = json!({"messages":[{"role":"user","content":"hello"}]});
        assert!(attach_omni_frame(&app, &mut ordinary)?.is_none());
        assert!(ordinary.get("include_current_frame").is_none());
        let mut implicit =
            json!({"messages":[{"role":"user","content":"hello"}],"camera_session_id":"epoch"});
        assert!(attach_omni_frame(&app, &mut implicit).is_err());
        let mut frame = json!({"camera_session_id":"submit-epoch","frame_id":7,
            "frame_age_ms":0,"raw_media_saved":false,"tracks":[]});
        assert!(
            app.camera
                .inject_preview_fixture(frame.clone(), vec![0xff, 0xd8, 1, 0xff, 0xd9])
        );
        let mut request = json!({"messages":[{"role":"user","content":"看看这个"}],
            "include_current_frame":true,"camera_session_id":"submit-epoch","frame_id":7});
        let submitted = attach_omni_frame(&app, &mut request)?.unwrap();
        assert_eq!(submitted["frame_id"], 7);
        assert_eq!(submitted["raw_media_saved"], false);
        assert!(submitted.get("jpeg").is_none());
        let parts = request["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert!(
            parts[1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/jpeg;base64,")
        );
        for key in ["include_current_frame", "camera_session_id", "frame_id"] {
            assert!(request.get(key).is_none());
        }
        let mut second_image = request.clone();
        second_image["include_current_frame"] = json!(true);
        second_image["camera_session_id"] = json!("submit-epoch");
        second_image["frame_id"] = json!(7);
        assert!(attach_omni_frame(&app, &mut second_image).is_err());
        frame["frame_id"] = json!(8);
        assert!(
            app.camera
                .inject_preview_fixture(frame, vec![0xff, 0xd8, 2, 0xff, 0xd9])
        );
        let mut stale = json!({"messages":[{"role":"user","content":"看看这个"}],
            "include_current_frame":true,"camera_session_id":"submit-epoch","frame_id":7});
        assert!(attach_omni_frame(&app, &mut stale).is_err());
        app.camera.close();
        Ok(())
    }

    #[tokio::test]
    async fn parent_digest_is_minimal_verbatim_and_profile_scoped() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = Workstation::new(dir.path().into(), "fixture-token".into())?;
        let (sid, other_sid) = {
            let stores = app.stores.lock().unwrap();
            let store = &stores["real"];
            let learner = store.create_learner("小林", "zh", "guided")?;
            let lid = s(&learner, "learner_id").to_owned();
            let session = store.create_session(&lid)?;
            let sid = s(&session, "session_id").to_owned();
            let other = store.create_learner("另一个孩子", "zh", "guided")?;
            let other_sid = s(
                &store.create_session(s(&other, "learner_id"))?,
                "session_id",
            )
            .to_owned();
            store.add_turn(
                &sid,
                "request",
                0,
                "user",
                "我想搭一座桥。",
                "received",
                &[],
            )?;
            store.add_turn(
                &sid,
                "request",
                0,
                "assistant",
                "先观察哪里需要支撑。",
                "shown",
                &[],
            )?;
            store.add_turn(
                &other_sid,
                "request",
                0,
                "user",
                "这是另一个孩子的原话。",
                "received",
                &[],
            )?;
            let mut state = session["state"].clone();
            state["goal"] = json!("一起搭一座稳稳的桥");
            store.checkpoint(&sid, &state)?;
            (sid, other_sid)
        };
        *app.current.lock().unwrap() = Some(("real".into(), sid.clone()));
        let digest = dispatch(app.clone(), &Method::GET, "/api/parent/digest", &json!({})).await?;
        assert_eq!(
            digest
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            [
                "activity_finished",
                "activity_title",
                "child_name",
                "has_records",
                "memory_count",
                "recent_child_words",
                "schema_version"
            ]
            .into_iter()
            .collect()
        );
        assert_eq!(digest["child_name"], "小林");
        assert_eq!(digest["activity_title"], "一起搭一座稳稳的桥");
        assert_eq!(digest["recent_child_words"], json!(["我想搭一座桥。"]));
        assert_eq!(digest["memory_count"], 0);
        assert_eq!(digest["activity_finished"], false);
        assert_eq!(digest["has_records"], true);
        assert!(!digest.to_string().contains("另一个孩子"));
        assert!(!digest.to_string().contains("先观察哪里"));
        let state = app.state()?;
        assert_eq!(state["parent_digest"], digest);
        *app.current.lock().unwrap() = Some(("real".into(), other_sid));
        let other = dispatch(app.clone(), &Method::GET, "/api/parent/digest", &json!({})).await?;
        assert_eq!(
            other["recent_child_words"],
            json!(["这是另一个孩子的原话。"])
        );
        assert!(!other.to_string().contains("我想搭一座桥"));
        app.close().await;
        Ok(())
    }

    #[test]
    fn origin_is_exact() {
        let mut h = HeaderMap::new();
        h.insert("host", "127.0.0.1:8765".parse().unwrap());
        assert!(same_origin(&h));
        h.insert("origin", "https://evil.example".parse().unwrap());
        assert!(!same_origin(&h));
    }
    #[test]
    fn cookie_constant_match() {
        let mut h = HeaderMap::new();
        h.insert("cookie", "x=1; ai_tutor_local=abc".parse().unwrap());
        assert!(authenticated(&h, "abc"));
        assert!(!authenticated(&h, "abd"));
    }
    #[test]
    fn command_fields_forbidden() {
        assert!(allowed_keys(&json!({"text":"hi","shell":"oops"}), &["text"]).is_err());
    }
    #[tokio::test]
    async fn stop_invalidates_enable_already_waiting_for_camera_lock() {
        use tower::ServiceExt;
        let dir = tempfile::tempdir().unwrap();
        let app = Workstation::new(dir.path().into(), "fixture-token".into()).unwrap();
        let held = app.camera_control.lock().await;
        let request = Request::builder().method("POST").uri("/api/ptz/enable")
            .header("host", "127.0.0.1:8765")
            .header("cookie", "ai_tutor_local=fixture-token")
            .header("content-type", "application/json")
            .body(Body::from(json!({"mode":"external_yolo","camera_session_id":"fixture-epoch","track_key":"fixture-track","target_generation":0}).to_string())).unwrap();
        let future = router(app.clone()).oneshot(request);
        tokio::pin!(future);
        assert!(futures_util::poll!(future.as_mut()).is_pending());
        app.ptz.stop();
        drop(held);
        let response = future.await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains("启用请求已经过期"));
        app.close().await;
    }
    #[tokio::test]
    async fn shutdown_invalidates_motion_before_waiting_for_teaching_lock() {
        let dir = tempfile::tempdir().unwrap();
        let app = Workstation::new(dir.path().into(), "fixture-token".into()).unwrap();
        let held = app.control.lock().await;
        let before = app.ptz.intent_generation();
        let capture_before = app.audio.status()["recording_generation"].as_u64().unwrap();
        let speech_before = app.audio.speech_epoch();
        let future = app.close();
        tokio::pin!(future);
        assert!(futures_util::poll!(future.as_mut()).is_pending());
        assert!(app.shutting_down.load(Ordering::SeqCst));
        assert!(app.ptz.intent_generation() > before);
        // No capture is started by this fixture. The generation is invalidated
        // synchronously even while shutdown cannot obtain the teaching lock.
        assert!(app.audio.status()["recording_generation"].as_u64().unwrap() > capture_before);
        assert!(app.audio.speech_epoch() > speech_before);
        assert_eq!(app.audio.status()["recording"], false);
        drop(held);
        future.await;
        assert!(app.ptz.enable_external().await.is_err());
    }
}
