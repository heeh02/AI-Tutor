//! Real Workstation/Store with a gated fake in the production runtime loop.
//! No helper process, model, provider request, microphone or camera is started.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use tokio::sync::Notify;

const QUESTION: &str = "quartz shimmer";

fn unit_vector(index: usize) -> Vec<f32> {
    let mut values = vec![0.0; PROFILE.dimension()];
    values[index] = 1.0;
    assert!(PROFILE.normalized(&values));
    values
}

#[derive(Clone, Debug)]
struct Call {
    request: String,
    role: EmbeddingRole,
    text: String,
}
struct Gates {
    calls: Mutex<Vec<Call>>,
    entered: Notify,
    query: watch::Sender<bool>,
    document: watch::Sender<bool>,
    stops: AtomicUsize,
    fail_stops: usize,
}
impl Gates {
    fn new(query_open: bool, document_open: bool, fail_stops: usize) -> Arc<Self> {
        let (query, _) = watch::channel(query_open);
        let (document, _) = watch::channel(document_open);
        Arc::new(Self {
            calls: Mutex::new(vec![]),
            entered: Notify::new(),
            query,
            document,
            stops: AtomicUsize::new(0),
            fail_stops,
        })
    }
    fn release(&self) {
        self.query.send_replace(true);
        self.document.send_replace(true);
    }
    async fn calls_at_least(&self, count: usize) -> Result<Vec<Call>> {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let entered = self.entered.notified();
                let calls = self.calls.lock().unwrap().clone();
                if calls.len() >= count {
                    return calls;
                }
                entered.await;
            }
        })
        .await
        .map_err(|_| anyhow!("fake encoder did not reach the expected gate"))
    }
}
struct FakeEncoder {
    gates: Arc<Gates>,
}
impl Encoder for FakeEncoder {
    fn encode<'a>(
        &'a mut self,
        id: &'a str,
        role: EmbeddingRole,
        text: &'a str,
    ) -> EncodeFuture<'a> {
        Box::pin(async move {
            self.gates.calls.lock().unwrap().push(Call {
                request: id.into(),
                role,
                text: text.into(),
            });
            self.gates.entered.notify_one();
            let mut release = match role {
                EmbeddingRole::Query => self.gates.query.subscribe(),
                EmbeddingRole::Document => self.gates.document.subscribe(),
            };
            // Even a failed assertion cannot leave an unbounded fake encode.
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if *release.borrow_and_update() {
                        return Ok::<_, anyhow::Error>(());
                    }
                    release
                        .changed()
                        .await
                        .map_err(|_| anyhow!("fake gate closed"))?;
                }
            })
            .await
            .map_err(|_| anyhow!("fake encode gate deadline"))??;
            Ok(EncodeResult::Encoded(Encoded {
                vector: unit_vector(0),
                tokens: 4,
                timings: json!({"tokenize_ms":0.0,"inference_ms":0.0,"total_ms":0.0}),
            }))
        })
    }
    fn exited(&mut self) -> Result<bool> {
        Ok(false)
    }
    fn stop(&mut self) -> StopFuture<'_> {
        Box::pin(async move {
            let call = self.gates.stops.fetch_add(1, AtomicOrdering::SeqCst);
            ensure!(
                call >= self.gates.fail_stops,
                "fake cleanup is not yet confirmed"
            );
            Ok(
                json!({"linux_cleanup_confirmed":true,"group_reaped":true,"worker_exit_code":0,"forced_group_stop":false}),
            )
        })
    }
}

struct Fixture {
    app: Arc<Workstation>,
    _directory: tempfile::TempDir,
    learner: String,
    session: String,
    dense_memory: String,
    lexical_memory: String,
    old_memory: String,
    other_learner: String,
    other_session: String,
    other_memory: String,
    real_learner: String,
    real_session: String,
    real_memory: String,
}
fn add_report(store: &Store, learner: &str, session: &str, content: &str) -> Result<String> {
    let source = if store.namespace == "simulator" {
        "simulated"
    } else {
        "learner_report"
    };
    let event = store.record_education_event(
        learner,
        session,
        source,
        "observation",
        &json!({"text":content}),
    )?;
    let epoch = store.learner(learner)?["epoch"].as_i64().unwrap();
    store
        .add_memory(
            learner,
            &json!({"kind":"reported_progress","content":content,
        "scope":"current_activity","source_event_ids":[event["event_id"]]}),
            epoch,
        )?
        .ok_or_else(|| anyhow!("synthetic report was not stored"))
}
impl Fixture {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let app = Workstation::new(directory.path().into(), "fake-retrieval-owner-token".into())?;
        let (
            learner,
            session,
            dense_memory,
            lexical_memory,
            old_memory,
            other_learner,
            other_session,
            other_memory,
            real_learner,
            real_session,
            real_memory,
        ) = {
            let stores = app.stores.lock().unwrap();
            let store = &stores["simulator"];
            let learner = s(
                &store.create_learner("Synthetic retrieval owner", "zh", "one_step")?,
                "learner_id",
            )
            .to_owned();
            let old = store.create_session(&learner)?;
            let old_memory = add_report(
                store,
                &learner,
                s(&old, "session_id"),
                "quartz shimmer from a separate activity must not leak",
            )?;
            let current = store.create_session(&learner)?;
            let session = s(&current, "session_id").to_owned();
            let dense_memory =
                add_report(store, &learner, &session, "城堡纸桥：单次折叠的来源记录。")?;
            let lexical_memory = add_report(
                store,
                &learner,
                &session,
                "quartz shimmer: one reported observation without a general conclusion",
            )?;
            let other_learner = s(
                &store.create_learner("Other synthetic owner", "en", "one_step")?,
                "learner_id",
            )
            .to_owned();
            let other_session = s(&store.create_session(&other_learner)?, "session_id").to_owned();
            let other_memory = add_report(
                store,
                &other_learner,
                &other_session,
                "quartz shimmer from another learner must not leak",
            )?;
            let real = &stores["real"];
            let real_learner = s(
                &real.create_learner("Synthetic namespace control", "en", "one_step")?,
                "learner_id",
            )
            .to_owned();
            let real_session = s(&real.create_session(&real_learner)?, "session_id").to_owned();
            let real_memory = add_report(
                real,
                &real_learner,
                &real_session,
                "quartz shimmer from another namespace must not leak",
            )?;
            (
                learner,
                session,
                dense_memory,
                lexical_memory,
                old_memory,
                other_learner,
                other_session,
                other_memory,
                real_learner,
                real_session,
                real_memory,
            )
        };
        *app.current.lock().unwrap() = Some(("simulator".into(), session.clone()));
        Ok(Self {
            app,
            _directory: directory,
            learner,
            session,
            dense_memory,
            lexical_memory,
            old_memory,
            other_learner,
            other_session,
            other_memory,
            real_learner,
            real_session,
            real_memory,
        })
    }
    fn index(&self, namespace: &str, learner: &str, selected: &[&str]) -> Result<()> {
        let stores = self.app.stores.lock().unwrap();
        let store = &stores[namespace];
        for work in store.embedding_work(learner, &PROFILE.model_key(), 32)? {
            if selected.contains(&work.memory_id.as_str()) {
                let vector = unit_vector(usize::from(work.memory_id == self.lexical_memory));
                ensure!(
                    store.commit_memory_embedding(&work, &PROFILE.model_key(), &vector)?,
                    "fake index CAS failed"
                );
            }
        }
        Ok(())
    }
    fn index_current(&self) -> Result<()> {
        self.index(
            "simulator",
            &self.learner,
            &[&self.dense_memory, &self.lexical_memory],
        )
    }
    fn request(&self, id: &str) -> Result<(Value, RetrievalBinding)> {
        let stores = self.app.stores.lock().unwrap();
        let store = &stores["simulator"];
        let event = store.record_education_event(
            &self.learner,
            &self.session,
            "simulated",
            "learner_statement",
            &json!({"text":QUESTION}),
        )?;
        let mut state = store.session(&self.session)?["state"].clone();
        state["latest_input"] = json!(QUESTION);
        store.checkpoint(&self.session, &state)?;
        let session = store.session(&self.session)?;
        let learner = store.learner(&self.learner)?;
        store.start_run(id, &self.learner, &self.session, 17)?;
        let binding = RetrievalBinding::capture(
            "simulator",
            &session,
            &learner,
            QUESTION,
            s(&event, "event_id"),
            json!({}),
        );
        Ok((
            json!({"request_id":id,"learner_id":self.learner,"session_id":self.session,
            "generation":session["generation"],"context":{"before_encoding":true}}),
            binding,
        ))
    }
    fn receipt(&self) -> Value {
        self.app
            .local_memory
            .control
            .lock()
            .unwrap()
            .last_query
            .as_ref()
            .unwrap()
            .1
            .clone()
    }
    fn no_delivery(&self) -> Result<()> {
        assert!(self.app.pending.lock().unwrap().is_empty());
        assert!(self.app.running.lock().unwrap().is_empty());
        assert!(
            self.app.stores.lock().unwrap()["simulator"].session(&self.session)?["turns"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        Ok(())
    }
}

struct RunningFake {
    app: Arc<Workstation>,
    gates: Arc<Gates>,
}
impl RunningFake {
    fn install(app: &Arc<Workstation>, gates: Arc<Gates>, ready: bool) -> Self {
        let runtime = app.local_memory.clone();
        let epoch = 1;
        let (stop, mut stopped) = watch::channel(false);
        let (send, receive) = mpsc::channel(1);
        {
            let mut c = runtime.control.lock().unwrap();
            c.epoch = epoch;
            c.running_epoch = Some(epoch);
            c.namespace = Some("simulator".into());
            c.cancellation = Some(stop);
            c.queries = ready.then(|| send.clone());
            c.coverage = None;
            c.last_query = None;
            c.status = json!({"mode":"bge_cpu","phase":if ready{"active"}else{"loading"},
                "active":ready,"ready":ready,"indexing":false});
        }
        let weak = Arc::downgrade(app);
        let gate_owner = gates.clone();
        let task_runtime = runtime.clone();
        let task = tokio::spawn(async move {
            let _channel_owner = send;
            let mut encoder: Box<dyn Encoder> = Box::new(FakeEncoder { gates: gate_owner });
            task_runtime
                .run(
                    &weak,
                    "simulator",
                    epoch,
                    &mut encoder,
                    receive,
                    &mut stopped,
                )
                .await;
            let was_stopped = *stopped.borrow();
            task_runtime.finish_owner(epoch, was_stopped, encoder).await;
        });
        *runtime.task.lock().unwrap() = Some(task);
        Self {
            app: app.clone(),
            gates,
        }
    }
    async fn finish(&self) -> Result<()> {
        self.gates.release();
        tokio::time::timeout(Duration::from_secs(3), self.app.stop_local_memory()).await??;
        self.app.close().await;
        Ok(())
    }
}
impl Drop for RunningFake {
    fn drop(&mut self) {
        self.gates.release();
        self.app.local_memory.preempt();
    }
}
fn memory_ids(request: &Value) -> Vec<String> {
    request["context"]["slow_memory"]
        .as_array()
        .unwrap()
        .iter()
        .map(|memory| s(memory, "memory_id").to_owned())
        .collect()
}
async fn prepare(fixture: &Fixture, id: &str) -> Result<Value> {
    let (request, binding) = fixture.request(id)?;
    let (_cancel, mut cancelled) = watch::channel(false);
    fixture
        .app
        .prepare_memory_context(request, Some(binding), &mut cancelled)
        .await
}

#[tokio::test]
async fn complete_index_uses_dense_order_without_crossing_owner_activity_or_namespace() -> Result<()>
{
    let fixture = Fixture::new()?;
    fixture.index_current()?;
    fixture.index("simulator", &fixture.learner, &[&fixture.old_memory])?;
    fixture.index(
        "simulator",
        &fixture.other_learner,
        &[&fixture.other_memory],
    )?;
    fixture.index("real", &fixture.real_learner, &[&fixture.real_memory])?;
    let gates = Gates::new(true, true, 0);
    let running = RunningFake::install(&fixture.app, gates.clone(), true);
    let foreground = fixture.app.memory_worker.foreground();
    let request = prepare(&fixture, "dense-current-scope").await?;
    let ids = memory_ids(&request);
    assert_eq!(ids.first(), Some(&fixture.dense_memory));
    assert!(
        ids.iter()
            .all(|id| id == &fixture.dense_memory || id == &fixture.lexical_memory)
    );
    assert!(
        !ids.contains(&fixture.old_memory)
            && !ids.contains(&fixture.other_memory)
            && !ids.contains(&fixture.real_memory)
    );
    let receipt = fixture.receipt();
    assert_eq!(receipt["method"], "bge_dense");
    assert_eq!(receipt["eligible"], 2);
    assert_eq!(receipt["indexed"], 2);
    assert!(receipt["fallback_reason"].is_null());
    let calls = gates.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].role, EmbeddingRole::Query);
    assert_eq!(calls[0].text, QUESTION);
    fixture.no_delivery()?;
    running.finish().await?;
    drop(foreground);
    Ok(())
}

#[tokio::test]
async fn incomplete_index_or_unready_encoder_returns_lexical_without_an_encode() -> Result<()> {
    for ready in [true, false] {
        let fixture = Fixture::new()?;
        if ready {
            fixture.index("simulator", &fixture.learner, &[&fixture.dense_memory])?;
        } else {
            fixture.index_current()?;
        }
        let gates = Gates::new(true, true, 0);
        let running = RunningFake::install(&fixture.app, gates.clone(), ready);
        let foreground = fixture.app.memory_worker.foreground();
        let request = prepare(&fixture, if ready { "partial-index" } else { "not-ready" }).await?;
        assert_eq!(memory_ids(&request).first(), Some(&fixture.lexical_memory));
        let receipt = fixture.receipt();
        assert_eq!(receipt["method"], "lexical");
        assert_eq!(
            receipt["fallback_reason"],
            if ready {
                "index_incomplete"
            } else {
                "encoder_not_ready"
            }
        );
        assert!(gates.calls.lock().unwrap().is_empty());
        running.finish().await?;
        drop(foreground);
    }
    Ok(())
}

#[tokio::test]
async fn query_wait_exceeding_600ms_falls_back_without_waiting_for_the_encoder() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.index_current()?;
    let gates = Gates::new(false, true, 0);
    let running = RunningFake::install(&fixture.app, gates.clone(), true);
    let foreground = fixture.app.memory_worker.foreground();
    let request =
        tokio::time::timeout(Duration::from_secs(2), prepare(&fixture, "query-budget")).await??;
    let receipt = fixture.receipt();
    assert_eq!(receipt["method"], "lexical");
    assert_eq!(receipt["fallback_reason"], "query_budget_exceeded");
    assert!(receipt["query_wait_ms"].as_f64().unwrap() >= 500.0);
    assert_eq!(memory_ids(&request).first(), Some(&fixture.lexical_memory));
    assert_eq!(gates.calls.lock().unwrap().len(), 1);
    fixture.no_delivery()?;
    running.finish().await?;
    drop(foreground);
    Ok(())
}

#[tokio::test]
async fn encoding_releases_control_and_changed_sources_or_owner_reject_old_context() -> Result<()> {
    for change in ["forget", "correct", "owner"] {
        let fixture = Fixture::new()?;
        fixture.index_current()?;
        let gates = Gates::new(false, true, 0);
        let running = RunningFake::install(&fixture.app, gates.clone(), true);
        let foreground = fixture.app.memory_worker.foreground();
        let (request, binding) = fixture.request(&format!("changed-{change}"))?;
        let app = fixture.app.clone();
        let (_cancel, mut cancelled) = watch::channel(false);
        let pending = tokio::spawn(async move {
            app.prepare_memory_context(request, Some(binding), &mut cancelled)
                .await
        });
        gates.calls_at_least(1).await?;
        {
            let _control =
                tokio::time::timeout(Duration::from_millis(150), fixture.app.control.lock())
                    .await?;
            let stores = fixture.app.stores.lock().unwrap();
            let store = &stores["simulator"];
            match change {
                "forget" => {
                    store.forget(&fixture.learner, Some(&fixture.dense_memory))?;
                }
                "correct" => {
                    store.correct_memory(
                        &fixture.learner,
                        &fixture.dense_memory,
                        "Correction: the earlier synthetic report is withdrawn.",
                    )?;
                }
                _ => {
                    *fixture.app.current.lock().unwrap() =
                        Some(("simulator".into(), fixture.other_session.clone()));
                }
            }
        }
        gates.query.send_replace(true);
        assert!(
            tokio::time::timeout(Duration::from_secs(2), pending)
                .await??
                .is_err()
        );
        assert!(
            fixture
                .app
                .local_memory
                .control
                .lock()
                .unwrap()
                .last_query
                .is_none()
        );
        fixture.no_delivery()?;
        running.finish().await?;
        drop(foreground);
    }
    Ok(())
}

#[tokio::test]
async fn foreground_query_precedes_next_document_and_inflight_index_is_not_committed() -> Result<()>
{
    let fixture = Fixture::new()?;
    fixture.index_current()?;
    *fixture.app.last_input.lock().unwrap() = Instant::now() - Duration::from_secs(10);
    let gates = Gates::new(true, false, 0);
    let running = RunningFake::install(&fixture.app, gates.clone(), true);
    let first = gates.calls_at_least(1).await?;
    assert_eq!(first[0].role, EmbeddingRole::Document);
    assert!(first[0].text.contains("separate activity"));
    let foreground = fixture.app.memory_worker.foreground();
    let (request, binding) = fixture.request("priority-query")?;
    let app = fixture.app.clone();
    let (_cancel, mut cancelled) = watch::channel(false);
    let pending = tokio::spawn(async move {
        app.prepare_memory_context(request, Some(binding), &mut cancelled)
            .await
    });
    tokio::time::timeout(Duration::from_millis(150), async {
        loop {
            let queued = fixture
                .app
                .local_memory
                .control
                .lock()
                .unwrap()
                .queries
                .as_ref()
                .is_some_and(|sender| sender.capacity() == 0);
            if queued {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    gates.document.send_replace(true);
    let request = tokio::time::timeout(Duration::from_secs(2), pending).await???;
    let calls = gates.calls_at_least(2).await?;
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].role, EmbeddingRole::Query);
    assert_eq!(calls[1].request, "priority-query");
    assert_eq!(memory_ids(&request).first(), Some(&fixture.dense_memory));
    let vectors = fixture.app.stores.lock().unwrap()["simulator"]
        .eligible_memory_embeddings(&fixture.learner, &PROFILE.model_key())?;
    assert!(
        !vectors
            .iter()
            .any(|row| row.snapshot.memory_id == fixture.old_memory)
    );
    running.finish().await?;
    drop(foreground);
    Ok(())
}

#[tokio::test]
async fn failed_stop_keeps_the_actual_owner_until_later_cleanup_is_confirmed() -> Result<()> {
    let fixture = Fixture::new()?;
    let gates = Gates::new(true, true, 2);
    let running = RunningFake::install(&fixture.app, gates.clone(), true);
    let foreground = fixture.app.memory_worker.foreground();
    gates.release();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), fixture.app.stop_local_memory())
            .await?
            .is_err()
    );
    assert!(
        fixture
            .app
            .local_memory
            .retained_owner
            .lock()
            .await
            .is_some()
    );
    assert_eq!(
        fixture.app.local_memory.control.lock().unwrap().status["phase"],
        "cleanup_failed"
    );
    assert_eq!(gates.stops.load(AtomicOrdering::SeqCst), 2);
    assert!(fixture.app.local_memory.queue(&fixture.app).is_err());
    tokio::time::timeout(Duration::from_secs(2), fixture.app.stop_local_memory()).await??;
    assert!(
        fixture
            .app
            .local_memory
            .retained_owner
            .lock()
            .await
            .is_none()
    );
    assert_eq!(gates.stops.load(AtomicOrdering::SeqCst), 3);
    assert_eq!(
        fixture.app.local_memory.control.lock().unwrap().status["phase"],
        "stopped"
    );
    fixture.app.close().await;
    drop(foreground);
    drop(running);
    Ok(())
}

#[tokio::test]
async fn status_does_not_publish_another_owners_coverage_or_query_receipt() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.index_current()?;
    let gates = Gates::new(true, true, 0);
    let running = RunningFake::install(&fixture.app, gates, true);
    let foreground = fixture.app.memory_worker.foreground();
    prepare(&fixture, "private-query-scope").await?;
    {
        let stores = fixture.app.stores.lock().unwrap();
        let store = &stores["simulator"];
        let current = store.session(&fixture.session)?;
        let learner = store.learner(&fixture.learner)?;
        let own = fixture.app.local_memory.status(&current, &[learner], false);
        assert_eq!(own["coverage"]["eligible"], 2);
        assert_eq!(own["last_query"]["request_id"], "private-query-scope");
        let other = store.session(&fixture.other_session)?;
        let other_learner = store.learner(&fixture.other_learner)?;
        let foreign = fixture
            .app
            .local_memory
            .status(&other, &[other_learner], false);
        assert!(foreign["coverage"].is_null() && foreign["last_query"].is_null());
        let real = &stores["real"];
        let session = real.session(&fixture.real_session)?;
        let learner = real.learner(&fixture.real_learner)?;
        let outside = fixture.app.local_memory.status(&session, &[learner], false);
        assert_eq!(outside["available_for_profile"], false);
        assert!(outside["coverage"].is_null() && outside["last_query"].is_null());
    }
    running.finish().await?;
    drop(foreground);
    Ok(())
}
