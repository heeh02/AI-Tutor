//! One optional CPU encoder: foreground queries outrank idle, single-record indexing.
//! Model I/O never holds teaching control or Store locks. Store owns source validity.
use super::Workstation;
use crate::{
    context::{build_context, build_context_with_semantic, memory_recall_query},
    local_memory::{EmbeddingRole, EncodeResult, Encoded, LocalMemoryPolicy, OwnedEmbedding},
    memory_embedding::EmbeddingProfile,
    store::{
        CandidateRanking, EmbeddingCoverage, EmbeddingExclusion, MemoryEmbeddingSnapshot,
        QueryEmbedding, Store, s, uid,
    },
};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, Weak, atomic::Ordering},
    time::{Duration, Instant},
};
use tokio::{
    sync::{Mutex as AsyncMutex, mpsc, oneshot, watch},
    task::JoinHandle,
};

const QUERY_BUDGET: Duration = Duration::from_millis(600);
const MAX_EXCLUSIONS: usize = 128;
const PROFILE: EmbeddingProfile = EmbeddingProfile::BgeM3NativeFp32;

type EncodeFuture<'a> = Pin<Box<dyn Future<Output = Result<EncodeResult>> + Send + 'a>>;
type StopFuture<'a> = Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

/// A private fake can hold an encode in flight to test real application cancellation.
trait Encoder: Send {
    fn encode<'a>(
        &'a mut self,
        id: &'a str,
        role: EmbeddingRole,
        text: &'a str,
    ) -> EncodeFuture<'a>;
    fn exited(&mut self) -> Result<bool>;
    fn stop(&mut self) -> StopFuture<'_>;
}
impl Encoder for OwnedEmbedding {
    fn encode<'a>(
        &'a mut self,
        id: &'a str,
        role: EmbeddingRole,
        text: &'a str,
    ) -> EncodeFuture<'a> {
        Box::pin(OwnedEmbedding::encode(self, id, role, text))
    }
    fn exited(&mut self) -> Result<bool> {
        OwnedEmbedding::exited(self)
    }
    fn stop(&mut self) -> StopFuture<'_> {
        Box::pin(OwnedEmbedding::stop(self))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Scope {
    namespace: String,
    learner: String,
    session: String,
    epoch: i64,
}
impl Scope {
    fn new(namespace: &str, session: &Value, learner: &Value) -> Self {
        Self {
            namespace: namespace.into(),
            learner: s(learner, "learner_id").into(),
            session: s(session, "session_id").into(),
            epoch: learner["epoch"].as_i64().unwrap_or(-1),
        }
    }
    fn index_owner(&self) -> (String, String, i64) {
        (self.namespace.clone(), self.learner.clone(), self.epoch)
    }
}

struct Query {
    request_id: String,
    text: String,
    reply: oneshot::Sender<std::result::Result<EncodeResult, ()>>,
}
struct Control {
    epoch: u64,
    running_epoch: Option<u64>,
    namespace: Option<String>,
    cancellation: Option<watch::Sender<bool>>,
    queries: Option<mpsc::Sender<Query>>,
    status: Value,
    coverage: Option<(Scope, Value, bool)>,
    last_query: Option<(Scope, Value)>,
}
pub(super) struct MemoryRetrievalRuntime {
    control: Mutex<Control>,
    task: Mutex<Option<JoinHandle<()>>>,
    // A failed cleanup retains the actual owner; retrying cannot start a second model.
    retained_owner: AsyncMutex<Option<Box<dyn Encoder>>>,
}
impl Default for MemoryRetrievalRuntime {
    fn default() -> Self {
        Self {
            control: Mutex::new(Control {
                epoch: 0,
                running_epoch: None,
                namespace: None,
                cancellation: None,
                queries: None,
                status: json!({"mode":"disabled","phase":"disabled",
                "ready":false,"active":false,"indexing":false}),
                coverage: None,
                last_query: None,
            }),
            task: Mutex::new(None),
            retained_owner: AsyncMutex::new(None),
        }
    }
}
struct QueryResult {
    encoded: Option<Encoded>,
    reason: &'static str,
    elapsed_ms: f64,
}

async fn cancelled(stop: &mut watch::Receiver<bool>) {
    if *stop.borrow() {
        return;
    }
    let _ = stop.wait_for(|value| *value).await;
}
fn audio_idle(audio: &Value) -> bool {
    audio["recording"] != true
        && audio["capture_pending_result"] != true
        && !["recording", "transcribing", "speaking", "synthesizing"].contains(&s(audio, "state"))
}

impl MemoryRetrievalRuntime {
    pub(super) fn enabled_for(&self, namespace: &str) -> bool {
        self.control.lock().unwrap().namespace.as_deref() == Some(namespace)
    }
    pub(super) fn status(&self, session: &Value, learners: &[Value], shutting_down: bool) -> Value {
        let scope = learners
            .iter()
            .find(|l| l["learner_id"] == session["learner_id"])
            .map(|l| Scope::new(s(session, "namespace"), session, l));
        let c = self.control.lock().unwrap();
        let mut value = c.status.clone();
        value["available_for_profile"] = json!(
            scope
                .as_ref()
                .is_some_and(|scope| c.namespace.as_deref() == Some(scope.namespace.as_str()))
        );
        let coverage = c
            .coverage
            .as_ref()
            .filter(|(owner, _, _)| scope.as_ref() == Some(owner));
        value["coverage"] = coverage.map(|(_, v, _)| v.clone()).unwrap_or(Value::Null);
        value["index_blocked"] = json!(coverage.is_some_and(|(_, _, blocked)| *blocked));
        value["last_query"] = c
            .last_query
            .as_ref()
            .filter(|(owner, _)| scope.as_ref() == Some(owner))
            .map(|(_, value)| value.clone())
            .unwrap_or(Value::Null);
        value["retry_available"] = json!(
            !shutting_down
                && c.running_epoch.is_none()
                && ["stopped", "degraded", "configuration_error"].contains(&s(&value, "phase"))
        );
        value["shutting_down"] = json!(shutting_down);
        value
    }
    fn queue(self: &Arc<Self>, app: &Arc<Workstation>) -> Result<()> {
        let mut c = self.control.lock().unwrap();
        ensure!(
            c.running_epoch.is_none() && c.status["phase"] != "cleanup_failed",
            "记忆检索仍在运行或收尾"
        );
        let policy = match LocalMemoryPolicy::load(&app.root) {
            Ok(Some(policy)) => policy,
            Ok(None) => {
                c.namespace = None;
                c.status = json!({"mode":"disabled","phase":"disabled","active":false,"ready":false,"indexing":false});
                return Ok(());
            }
            Err(_) => {
                c.namespace = None;
                c.status = json!({"mode":"bge_cpu","phase":"configuration_error","active":false,"ready":false,"indexing":false,
                    "error":"local_memory_policy_invalid"});
                return Err(anyhow!("本地记忆检索配置不可用"));
            }
        };
        c.epoch = c.epoch.wrapping_add(1);
        let epoch = c.epoch;
        let (stop, stopped) = watch::channel(false);
        let (send, receive) = mpsc::channel(1);
        c.namespace = Some(policy.namespace.clone());
        c.running_epoch = Some(epoch);
        c.cancellation = Some(stop);
        c.queries = None;
        c.coverage = None;
        c.last_query = None;
        c.status = json!({"mode":"bge_cpu","phase":"starting","active":false,"ready":false,
            "indexing":false,"model":"BAAI/bge-m3","device":"cpu"});
        let runtime = self.clone();
        let weak = Arc::downgrade(app);
        let root = app.root.clone();
        *self.task.lock().unwrap() = Some(tokio::spawn(async move {
            runtime
                .start_owned(weak, root, policy, epoch, send, receive, stopped)
                .await;
        }));
        Ok(())
    }
    async fn start_owned(
        self: Arc<Self>,
        app: Weak<Workstation>,
        root: std::path::PathBuf,
        policy: LocalMemoryPolicy,
        epoch: u64,
        sender: mpsc::Sender<Query>,
        receiver: mpsc::Receiver<Query>,
        mut stopped: watch::Receiver<bool>,
    ) {
        let spawned = tokio::select! { biased;
            _=cancelled(&mut stopped)=>Err(anyhow!("local_memory_start_cancelled")),
            result=OwnedEmbedding::spawn(&root,&policy)=>result,
        };
        let mut owned = match spawned {
            Ok(owned) => owned,
            Err(_) => {
                self.finished(epoch, *stopped.borrow(), true, None);
                return;
            }
        };
        let prepared = owned
            .prepare(&mut stopped, |event| {
                let mut c = self.control.lock().unwrap();
                if c.epoch == epoch {
                    c.status["phase"] = event["phase"].clone();
                }
            })
            .await;
        let ready = prepared.is_ok() && !*stopped.borrow();
        if ready {
            let mut c = self.control.lock().unwrap();
            if c.epoch == epoch {
                c.queries = Some(sender);
                c.status["phase"] = json!("active");
                c.status["active"] = json!(true);
                c.status["ready"] = json!(true);
                c.status["preparation_timings"] = prepared.as_ref().unwrap()["timings"].clone();
            }
        }
        let mut encoder: Box<dyn Encoder> = Box::new(owned);
        if ready {
            self.run(
                &app,
                &policy.namespace,
                epoch,
                &mut encoder,
                receiver,
                &mut stopped,
            )
            .await;
        }
        let stop_requested = *stopped.borrow();
        self.finish_owner(epoch, stop_requested, encoder).await;
    }
    async fn run(
        &self,
        weak: &Weak<Workstation>,
        namespace: &str,
        epoch: u64,
        encoder: &mut Box<dyn Encoder>,
        mut receive: mpsc::Receiver<Query>,
        stopped: &mut watch::Receiver<bool>,
    ) {
        let mut excluded = VecDeque::<EmbeddingExclusion>::new();
        let mut exclusion_owner = None;
        let mut paused = false;
        let mut delay = Duration::from_secs(1);
        loop {
            tokio::select! { biased;
                _=cancelled(stopped)=>break,
                message=receive.recv()=>{
                    let Some(message)=message else {break};
                    if message.reply.is_closed() {continue;}
                    self.indexing(epoch,false);
                    let result=encoder.encode(&message.request_id,EmbeddingRole::Query,&message.text).await;
                    if *stopped.borrow() {break;}
                    match result {
                        Ok(value)=>{let _=message.reply.send(Ok(value));}
                        Err(_)=>{let _=message.reply.send(Err(()));break;}
                    }
                    delay=Duration::from_secs(1);
                }
                _=tokio::time::sleep(delay)=>{
                    if encoder.exited().unwrap_or(true) {break;}
                    let Some(app)=weak.upgrade() else {break};
                    let picked=app.memory_index_work(namespace,&mut exclusion_owner,&mut excluded,&mut paused);
                    let (scope,work,coverage)=match picked {Ok(Some(value))=>value,Ok(None)=>{delay=Duration::from_secs(1);continue;},Err(_)=>break};
                    self.coverage(scope.clone(),&coverage,paused || (work.is_none() && coverage.indexed<coverage.eligible));
                    let Some(work)=work else {delay=Duration::from_secs(1);continue;};
                    drop(app);
                    self.indexing(epoch,true);
                    let result=encoder.encode(&uid("index"),EmbeddingRole::Document,&work.content).await;
                    self.indexing(epoch,false);
                    if *stopped.borrow() {break;}
                    match result {
                        Ok(EncodeResult::Encoded(value))=>{
                            if let Some(app)=weak.upgrade() {if app.commit_index_if_idle(&scope,&work,&value.vector).is_err(){break;}}
                        }
                        Ok(EncodeResult::TooLong{..})=>{
                            if excluded.len()<MAX_EXCLUSIONS {excluded.push_back((&work).into());}
                            else {paused=true;}
                        }
                        Err(_)=>break,
                    }
                    delay=Duration::from_millis(100);
                }
            }
        }
        self.indexing(epoch, false);
    }
    fn indexing(&self, epoch: u64, active: bool) {
        let mut c = self.control.lock().unwrap();
        if c.epoch == epoch {
            c.status["indexing"] = json!(active);
        }
    }
    fn coverage(&self, scope: Scope, coverage: &EmbeddingCoverage, blocked: bool) {
        self.control.lock().unwrap().coverage = Some((
            scope,
            json!({"eligible":coverage.eligible,"indexed":coverage.indexed}),
            blocked,
        ));
    }
    async fn finish_owner(&self, epoch: u64, stopped: bool, mut owner: Box<dyn Encoder>) {
        let receipt = owner.stop().await;
        let confirmed = receipt.is_ok();
        if !confirmed {
            *self.retained_owner.lock().await = Some(owner);
        }
        self.finished(epoch, stopped, confirmed, receipt.ok());
    }
    fn finished(&self, epoch: u64, stopped: bool, confirmed: bool, receipt: Option<Value>) {
        let mut c = self.control.lock().unwrap();
        if c.running_epoch != Some(epoch) {
            return;
        }
        c.queries = None;
        c.cancellation = None;
        c.running_epoch = None;
        c.status = json!({"mode":"bge_cpu","phase":if !confirmed{"cleanup_failed"}else if stopped{"stopped"}else{"degraded"},
            "active":false,"ready":false,"indexing":false,"cleanup_confirmed":confirmed,
            "ownership_stop":receipt,"error":if !confirmed{Some("local_memory_cleanup_unconfirmed")}
                else if !stopped{Some("local_memory_unavailable")}else{None}});
    }
    pub(super) fn preempt(&self) {
        let mut c = self.control.lock().unwrap();
        c.epoch = c.epoch.wrapping_add(1);
        c.queries = None;
        if let Some(stop) = &c.cancellation {
            let _ = stop.send(true);
        }
        c.status["active"] = json!(false);
        c.status["ready"] = json!(false);
        c.status["indexing"] = json!(false);
        if c.running_epoch.is_some() {
            c.status["phase"] = json!("stopping");
        }
    }
    async fn drain(&self) {
        let task = self.task.lock().unwrap().take();
        if let Some(mut task) = task {
            match tokio::time::timeout(Duration::from_secs(35), &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    self.control.lock().unwrap().status["phase"] = json!("cleanup_failed");
                }
                Err(_) => {
                    *self.task.lock().unwrap() = Some(task);
                    self.control.lock().unwrap().status["phase"] = json!("cleanup_failed");
                    return;
                }
            }
        }
        let mut retained = self.retained_owner.lock().await;
        if let Some(owner) = retained.as_mut() {
            if let Ok(receipt) = owner.stop().await {
                *retained = None;
                let mut c = self.control.lock().unwrap();
                c.status["phase"] = json!("stopped");
                c.status["cleanup_confirmed"] = json!(true);
                c.status["ownership_stop"] = receipt;
                c.status["error"] = Value::Null;
            }
        }
    }
    async fn query(
        &self,
        scope: &Scope,
        rid: &str,
        text: &str,
        cancelled_by_user: &mut watch::Receiver<bool>,
    ) -> Result<QueryResult> {
        let started = Instant::now();
        let fallback = |reason| QueryResult {
            encoded: None,
            reason,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        };
        let (sender, epoch) = {
            let c = self.control.lock().unwrap();
            if c.namespace.as_deref() != Some(scope.namespace.as_str()) {
                return Ok(fallback("profile_disabled"));
            }
            if c.status["active"] != true {
                return Ok(fallback("encoder_not_ready"));
            }
            let Some(sender) = c.queries.clone() else {
                return Ok(fallback("encoder_unavailable"));
            };
            (sender, c.epoch)
        };
        let (reply, received) = oneshot::channel();
        if sender
            .try_send(Query {
                request_id: rid.into(),
                text: text.into(),
                reply,
            })
            .is_err()
        {
            return Ok(fallback("encoder_busy"));
        }
        let response = tokio::select! {biased;
            _=cancelled(cancelled_by_user)=>return Err(anyhow!("已取消的旧记忆查询")),
            value=tokio::time::timeout(QUERY_BUDGET,received)=>value,
        };
        if self.control.lock().unwrap().epoch != epoch {
            return Ok(fallback("encoder_changed"));
        }
        match response {
            Ok(Ok(Ok(EncodeResult::Encoded(encoded)))) => Ok(QueryResult {
                encoded: Some(encoded),
                reason: "encoded",
                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            }),
            Ok(Ok(Ok(EncodeResult::TooLong { .. }))) => Ok(fallback("query_too_long")),
            Err(_) => Ok(fallback("query_budget_exceeded")),
            _ => Ok(fallback("encoder_unavailable")),
        }
    }
}

/// The provider request remains unchanged until encoding and a fresh binding check succeed.
pub(super) struct RetrievalBinding {
    scope: Scope,
    generation: i64,
    revision: i64,
    session: Value,
    learner: Value,
    text: String,
    event: String,
    vision: Value,
}
impl RetrievalBinding {
    pub(super) fn capture(
        namespace: &str,
        session: &Value,
        learner: &Value,
        text: &str,
        event: &str,
        vision: Value,
    ) -> Self {
        Self {
            scope: Scope::new(namespace, session, learner),
            generation: session["generation"].as_i64().unwrap_or(-1),
            revision: session["state_revision"].as_i64().unwrap_or(-1),
            session: session.clone(),
            learner: learner.clone(),
            text: text.into(),
            event: event.into(),
            vision,
        }
    }
    fn validate(&self, app: &Workstation, store: &Store) -> Result<()> {
        ensure!(
            app.current.lock().unwrap().as_ref()
                == Some(&(self.scope.namespace.clone(), self.scope.session.clone()))
                && store.session(&self.scope.session)?["generation"] == self.generation
                && store.state_revision(&self.scope.session)? == self.revision
                && store.learner(&self.scope.learner)?["epoch"] == self.scope.epoch,
            "记忆查询所属活动或来源已变化"
        );
        Ok(())
    }
}

impl Workstation {
    pub fn start_local_memory_runtime(self: &Arc<Self>) {
        if self.local_memory.queue(self).is_err() {
            self.notify("memory.retrieval");
        }
    }
    pub(super) async fn stop_local_memory(&self) -> Result<Value> {
        self.local_memory.preempt();
        self.local_memory.drain().await;
        let retained = self.local_memory.retained_owner.lock().await.is_some();
        let c = self.local_memory.control.lock().unwrap();
        ensure!(
            !retained && c.running_epoch.is_none() && c.status["phase"] != "cleanup_failed",
            "本地记忆服务退出尚未确认"
        );
        drop(c);
        self.notify("memory.retrieval");
        Ok(json!({"ok":true}))
    }
    pub(super) async fn retry_local_memory(self: &Arc<Self>) -> Result<Value> {
        ensure!(!self.shutting_down.load(Ordering::SeqCst), "工作台正在关闭");
        ensure!(
            self.local_memory
                .control
                .lock()
                .unwrap()
                .running_epoch
                .is_none(),
            "本地记忆仍在运行或收尾"
        );
        self.local_memory.drain().await;
        self.local_memory.queue(self)?;
        self.notify("memory.retrieval");
        Ok(json!({"ok":true}))
    }
    fn memory_index_work(
        &self,
        namespace: &str,
        owner: &mut Option<(String, String, i64)>,
        excluded: &mut VecDeque<EmbeddingExclusion>,
        paused: &mut bool,
    ) -> Result<Option<(Scope, Option<MemoryEmbeddingSnapshot>, EmbeddingCoverage)>> {
        let Ok(_control) = self.control.try_lock() else {
            return Ok(None);
        };
        if self.shutting_down.load(Ordering::SeqCst)
            || !self.memory_worker.is_idle()
            || !audio_idle(&self.audio.status())
            || self.last_input.lock().unwrap().elapsed() < Duration::from_secs(1)
        {
            return Ok(None);
        }
        let current = self.current.lock().unwrap().clone();
        let Some((ns, sid)) = current.filter(|(ns, _)| ns == namespace) else {
            return Ok(None);
        };
        let stores = self.stores.lock().unwrap();
        let store = &stores[&ns];
        let session = store.session(&sid)?;
        let learner = store.learner(s(&session, "learner_id"))?;
        let scope = Scope::new(&ns, &session, &learner);
        if owner.as_ref() != Some(&scope.index_owner()) {
            *owner = Some(scope.index_owner());
            excluded.clear();
            *paused = false;
        }
        let key = PROFILE.model_key();
        let coverage = store.embedding_coverage_for_session(&sid, &key)?;
        let work = if *paused {
            None
        } else {
            store
                .embedding_work_excluding(&scope.learner, &key, 1, excluded.make_contiguous())?
                .into_iter()
                .next()
        };
        Ok(Some((scope, work, coverage)))
    }
    fn commit_index_if_idle(
        &self,
        scope: &Scope,
        work: &MemoryEmbeddingSnapshot,
        vector: &[f32],
    ) -> Result<bool> {
        let Ok(_control) = self.control.try_lock() else {
            return Ok(false);
        };
        if self.shutting_down.load(Ordering::SeqCst)
            || !self.memory_worker.is_idle()
            || !audio_idle(&self.audio.status())
            || self.current.lock().unwrap().as_ref()
                != Some(&(scope.namespace.clone(), scope.session.clone()))
        {
            return Ok(false);
        }
        let stores = self.stores.lock().unwrap();
        let store = &stores[&scope.namespace];
        if store.learner(&scope.learner)?["epoch"] != scope.epoch {
            return Ok(false);
        }
        store.commit_memory_embedding(work, &PROFILE.model_key(), vector)
    }
    pub(super) async fn prepare_memory_context(
        &self,
        mut request: Value,
        binding: Option<RetrievalBinding>,
        cancelled_by_user: &mut watch::Receiver<bool>,
    ) -> Result<Value> {
        let Some(binding) = binding else {
            return Ok(request);
        };
        let key = PROFILE.model_key();
        let query = memory_recall_query(&binding.text, s(&binding.session["state"], "goal"))
            .ok_or_else(|| anyhow!("记忆查询为空"))?
            .to_owned();
        let before = {
            let _control = self.control.lock().await;
            let stores = self.stores.lock().unwrap();
            let store = &stores[&binding.scope.namespace];
            binding.validate(self, store)?;
            store.embedding_coverage_for_session(&binding.scope.session, &key)?
        };
        let result = if before.eligible == 0 {
            QueryResult {
                encoded: None,
                reason: "no_eligible_memories",
                elapsed_ms: 0.0,
            }
        } else if before.eligible != before.indexed {
            QueryResult {
                encoded: None,
                reason: "index_incomplete",
                elapsed_ms: 0.0,
            }
        } else {
            self.local_memory
                .query(
                    &binding.scope,
                    s(&request, "request_id"),
                    &query,
                    cancelled_by_user,
                )
                .await?
        };
        ensure!(!*cancelled_by_user.borrow(), "已取消的旧记忆查询");
        let _control = self.control.lock().await;
        let stores = self.stores.lock().unwrap();
        let store = &stores[&binding.scope.namespace];
        binding.validate(self, store)?;
        let coverage = store.embedding_coverage_for_session(&binding.scope.session, &key)?;
        let complete = coverage.eligible > 0 && coverage.eligible == coverage.indexed;
        let semantic = result.encoded.as_ref().filter(|_| complete);
        let context = match semantic {
            Some(encoded) => build_context_with_semantic(
                store,
                &binding.session,
                &binding.learner,
                &binding.text,
                &binding.event,
                &binding.vision,
                &query,
                QueryEmbedding {
                    model_key: &key,
                    values: &encoded.vector,
                },
                CandidateRanking::DenseOnly,
            )?,
            None => build_context(
                store,
                &binding.session,
                &binding.learner,
                &binding.text,
                &binding.event,
                &binding.vision,
            )?,
        };
        let selected: Vec<_> = context["slow_memory"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| m["memory_id"].clone())
            .collect();
        // Diagnostic receipts contain identities and scores, never memory text
        // or vectors. Scores remain outside the teaching context.
        let ranked = if let Some(encoded) = semantic {
            store.recall_candidates_for_session_ranked(&binding.scope.session,&query,
                QueryEmbedding{model_key:&key,values:&encoded.vector},5,CandidateRanking::DenseOnly)?
                .into_iter().map(|candidate|json!({"memory_id":candidate.memory["memory_id"],
                    "semantic_rank":candidate.semantic_rank,"cosine":candidate.cosine,
                    "ranking_score":candidate.ranking_score,"relevance_status":candidate.relevance_status})).collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let vector_sha = semantic.map(|encoded| {
            format!(
                "{:x}",
                Sha256::digest(
                    encoded
                        .vector
                        .iter()
                        .flat_map(|value| value.to_le_bytes())
                        .collect::<Vec<_>>()
                )
            )
        });
        let has_preferences = context["slow_memory"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|m| m["kind"] == "help_preference" && m["scope"] == "cross_session_explicit");
        let receipt = json!({"request_id":request["request_id"],"method":if semantic.is_some(){if has_preferences{"bge_dense_with_preferences"}else{"bge_dense"}}else{"lexical"},
            "fallback_reason":if semantic.is_some(){Value::Null}else{json!(if result.encoded.is_some(){"index_changed"}else{result.reason})},
            "query_wait_ms":result.elapsed_ms,"tokens":semantic.map(|v|v.tokens),
            "encoder_timings":semantic.map(|v|v.timings.clone()),"selected_memory_ids":selected,
            "indexed":coverage.indexed,"eligible":coverage.eligible,"model_key":key,
            "query_text_sha256":format!("{:x}",Sha256::digest(query.as_bytes())),
            "query_vector_sha256_f32le":vector_sha,"ranked_candidates":ranked,
            "context_selection":context["context_selection"]});
        request["context"] = context;
        store.set_run_payload_chars(
            s(&request, "request_id"),
            request.to_string().chars().count(),
        )?;
        self.local_memory
            .coverage(binding.scope.clone(), &coverage, false);
        self.local_memory.control.lock().unwrap().last_query = Some((binding.scope, receipt));
        Ok(request)
    }
}

#[cfg(test)]
#[path = "memory_retrieval_tests.rs"]
mod tests;
