//! Complete structured memory responses over the existing isolated ACP channel.
//! Progress contains identities/timings/counts only, never thought or reply text.
use super::{
    acp_common::*,
    acp_stream::{Packet, Wire, ensure_low_thinking, verified_profile},
    *,
};
use tokio::io::AsyncWriteExt;

const MEMORY_TEXT_LIMIT: usize = 128 * 1024;
const CANCEL_RECEIPT_BUDGET: Duration = Duration::from_millis(500);
const REAP_BUDGET: Duration = Duration::from_millis(750);
const STDERR_BUDGET: Duration = Duration::from_millis(250);

fn initial_metrics(request: &Value) -> Value {
    json!({"transport":"acp_memory_v1","request_id":request["request_id"],
        "observation_id":format!("memory_acp_{}",uuid::Uuid::new_v4().simple()),
        "session_id":request["session_id"],"job_id":request["_memory_job_id"].as_str(),
        "attempt_no":request["_memory_attempt"].as_u64(),"phase":"prepare",
        "prompt_write_attempted":false,"prompt_write_completed":false,"prompt_pending":false,
        "agent_output_observed":false,"nonempty_thought_event_observed":false,
        "agent_message_text_observed":false,"first_nonempty_thought_event_ms":null,
        "first_nonempty_message_event_ms":null,"message_chunks":0,"thought_chunks_discarded":0,
        "nonempty_thought_chunks":0,"message_text_bytes":0,"prompt_stop_reason":null,
        "cancel_notification_attempted":false,"cancel_notification_write_completed":false,
        "cancel_observation_code":null,"agent_cancel_terminal_observed":false,
        "remote_compute_stop_confirmed":false,"operation_success":null,"success":false,
        "requested_thinking":"low","effective_thinking":null,
        "failure_code":null,"failure_phase":null,"remote_rpc_code":null,
        "cleanup_confirmed":null,"process_reaped":null,"stderr_drained":null,
        "owned_directory_removed":null,"stderr_bytes":null,
        "time_origin":"after_owned_process_spawn",
        "output_observation_scope":"ACP agent output; message text may include local agent notices",
        "cancellation_scope":"matching prompt cancelled terminal before close/kill; not remote compute telemetry",
        "thought_text_saved_by_application":false})
}

fn phase(metrics: &mut Value, name: &str) {
    metrics["phase"] = json!(name);
}

fn record_failure(metrics: &mut Value, error: &anyhow::Error) {
    let failure = acp_failure(error).unwrap_or(AcpFailure::Operation);
    metrics["failure_code"] = json!(failure.code());
    metrics["failure_phase"] = metrics["phase"].clone();
    metrics["remote_rpc_code"] = json!(failure.rpc_code());
}

fn stop_reason(result: &Value) -> Result<&str> {
    let reason = result["stopReason"].as_str().ok_or(AcpFailure::Protocol)?;
    ensure!(
        [
            "end_turn",
            "max_tokens",
            "max_turn_requests",
            "refusal",
            "cancelled"
        ]
        .contains(&reason),
        AcpFailure::Protocol
    );
    Ok(reason)
}

// The worker can drop its provider future after its existing cancellation grace.
// Such a drop must clear live progress without inventing a process-wait receipt.
struct ProgressScope<'a> {
    provider: &'a KimiProvider,
    observation_id: String,
    finished: bool,
}
impl Drop for ProgressScope<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let mut state = self.provider.state.lock().unwrap();
            if state.metrics["active_acp_memory"]["observation_id"] == self.observation_id {
                let mut interrupted = state.metrics["active_acp_memory"].take();
                interrupted["phase"] = json!("caller_dropped_future");
                interrupted["failure_code"] = json!("acp_memory_future_dropped");
                interrupted["operation_success"] = json!(false);
                interrupted["success"] = json!(false);
                interrupted["cleanup_confirmed"] = Value::Null;
                interrupted["process_reaped"] = Value::Null;
                state.metrics["last_acp_memory"] = interrupted;
            }
        }
    }
}

impl KimiProvider {
    fn publish_memory_progress(&self, metrics: &Value) {
        let mut state = self.state.lock().unwrap();
        if state.metrics["active_acp_memory"]["observation_id"] == metrics["observation_id"] {
            state.metrics["active_acp_memory"] = metrics.clone();
        }
    }

    pub(super) async fn consolidate_acp(
        &self,
        request: &Value,
        started_signal: watch::Sender<bool>,
    ) -> Result<Value> {
        let mut metrics = initial_metrics(request);
        let mut scope = ProgressScope {
            provider: self,
            observation_id: s(&metrics, "observation_id").to_owned(),
            finished: false,
        };
        let result = self
            .consolidate_acp_inner(request, started_signal, &mut metrics)
            .await;
        if metrics["operation_success"].is_null() {
            metrics["operation_success"] = json!(result.is_ok());
            if let Err(error) = &result {
                record_failure(&mut metrics, error);
            }
        }
        metrics["success"] = json!(result.is_ok());
        let mut state = self.state.lock().unwrap();
        if state.metrics["active_acp_memory"]["observation_id"] == metrics["observation_id"] {
            state.metrics["active_acp_memory"] = Value::Null;
        }
        // Last completed call, not a greatest-generation marker.
        state.metrics["last_acp_memory"] = metrics;
        scope.finished = true;
        result
    }

    async fn consolidate_acp_inner(
        &self,
        request: &Value,
        started_signal: watch::Sender<bool>,
        metrics: &mut Value,
    ) -> Result<Value> {
        ensure!(self.state.lock().unwrap().ready, AcpFailure::Profile);
        ensure!(
            request["prompt_version"] == memory_contract::PROMPT_VERSION,
            AcpFailure::Protocol
        );
        let rid = s(request, "request_id");
        ensure!(!rid.is_empty(), AcpFailure::Protocol);
        phase(metrics, "queue");
        let (_call, permit, mut cancellation) =
            self.register_call(rid, s(request, "learner_id"), true)?;
        let _permit = permit.ok_or(AcpFailure::Operation)?;
        ensure!(!*cancellation.borrow(), AcpFailure::Cancelled);
        self.state.lock().unwrap().metrics["active_acp_memory"] = metrics.clone();
        phase(metrics, "prepare");
        let home = prepare_home(self, true, s(request, "learner_id"), Some(rid), false)
            .map_err(|_| AcpFailure::Prepare)?;
        let prompt = build_prompt(memory_contract::POLICY, &memory_contract::schema(), request)
            .map_err(|_| AcpFailure::Prepare)?;
        metrics["prompt_chars"] = json!(prompt.chars().count());
        metrics["prompt_utf8_bytes"] = json!(prompt.len());
        ensure!(!*cancellation.borrow(), AcpFailure::Cancelled);
        phase(metrics, "spawn");
        let mut child = isolated_command(self, &home)
            .spawn()
            .map_err(|_| AcpFailure::Spawn)?;
        let job = match ProcessJob::attach(&child) {
            Ok(job) => job,
            Err(_) => {
                let _ = child.start_kill();
                let _ = tokio::time::timeout(REAP_BUDGET, child.wait()).await;
                return Err(AcpFailure::ProcessOwnership.into());
            }
        };
        let mut wire = Wire::new(child.stdout.take().unwrap(), child.stdin.take().unwrap());
        let stderr = child.stderr.take().unwrap();
        let mut stderr_task = tokio::spawn(async move {
            KimiProvider::bounded(stderr, 128 * 1024)
                .await
                .map(|v| v.len())
        });
        let started = Instant::now();
        self.increment("requests");
        self.increment("consolidation_requests");
        self.increment("acp_memory_requests");
        // Preserve the old budget receipt meaning: owned process started.
        // Generation-output observation is a separate request-bound metric.
        let _ = started_signal.send(true);
        self.publish_memory_progress(metrics);
        let mut stderr_done = false;
        let mut stderr_ok = true;
        let mut stderr_bytes = None;
        let result = {
            let operation = workflow(self, &home, &mut wire, &prompt, metrics, started);
            tokio::pin!(operation);
            let deadline = tokio::time::sleep(DEADLINE);
            tokio::pin!(deadline);
            loop {
                tokio::select! { biased;
                    _=cancellation.changed()=>break Err(AcpFailure::Cancelled.into()),
                    _=&mut deadline=>break Err(AcpFailure::Deadline.into()),
                    value=&mut operation=>break value,
                    value=&mut stderr_task,if !stderr_done=>{
                        stderr_done=true;
                        match value {
                            Ok(Ok(bytes))=>stderr_bytes=Some(bytes),
                            _=>{stderr_ok=false;break Err(AcpFailure::Stderr.into());}
                        }
                    }
                }
            }
        };
        metrics["operation_success"] = json!(result.is_ok());
        if let Err(error) = &result {
            record_failure(metrics, error);
            // Never close/dispose the session before observing this response:
            // Kimi dispose itself can synthesize a cancelled prompt terminal.
            cancel_and_observe(&mut wire, metrics, CANCEL_RECEIPT_BUDGET).await;
            self.publish_memory_progress(metrics);
        }
        phase(metrics, "cleanup");
        let _ = tokio::time::timeout(Duration::from_millis(100), wire.writer.shutdown()).await;
        drop(job);
        let _ = child.start_kill();
        let reaped = matches!(
            tokio::time::timeout(REAP_BUDGET, child.wait()).await,
            Ok(Ok(_))
        );
        if !stderr_done {
            match tokio::time::timeout(STDERR_BUDGET, &mut stderr_task).await {
                Ok(Ok(Ok(bytes))) => stderr_bytes = Some(bytes),
                _ => {
                    stderr_ok = false;
                    stderr_task.abort();
                }
            }
        }
        let cleaned = home.run.cleanup().is_ok();
        metrics["message_chunks"] = json!(wire.message_chunks);
        metrics["thought_chunks_discarded"] = json!(wire.thought_chunks_discarded);
        metrics["nonempty_thought_chunks"] = json!(wire.nonempty_thought_chunks);
        metrics["process_reaped"] = json!(reaped);
        metrics["stderr_drained"] = json!(stderr_ok);
        metrics["stderr_bytes"] = json!(stderr_bytes);
        metrics["owned_directory_removed"] = json!(cleaned);
        metrics["cleanup_confirmed"] = json!(reaped && stderr_ok && cleaned);
        metrics["total_duration_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
        self.publish_memory_progress(metrics);
        match result {
            Err(error) => {
                self.increment(if *cancellation.borrow() {
                    "cancelled"
                } else {
                    "errors"
                });
                Err(error)
            }
            Ok(reply) => {
                if !(reaped && stderr_ok && cleaned) {
                    metrics["failure_code"] = json!(AcpFailure::Cleanup.code());
                    metrics["failure_phase"] = json!("cleanup");
                    self.increment("errors");
                    return Err(AcpFailure::Cleanup.into());
                }
                self.increment("completed");
                Ok(reply)
            }
        }
    }
}

async fn workflow<R: AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(
    provider: &KimiProvider,
    home: &AcpHome,
    wire: &mut Wire<R, W>,
    prompt: &str,
    metrics: &mut Value,
    started: Instant,
) -> Result<Value> {
    phase(metrics, "initialize");
    let init = wire
        .request(
            1,
            "initialize",
            json!({"protocolVersion":1,
        "clientCapabilities":{},"clientInfo":{"name":"AI Tutor structured memory","version":"1"}}),
        )
        .await?;
    ensure!(
        init["agentInfo"]["version"] == VERSION && init["protocolVersion"] == 1,
        AcpFailure::Protocol
    );
    phase(metrics, "session_new");
    let created = wire
        .request(2, "session/new", json!({"cwd":home.work,"mcpServers":[]}))
        .await?;
    let session = created["sessionId"]
        .as_str()
        .ok_or(AcpFailure::Protocol)?
        .to_owned();
    wire.session = Some(session.clone());
    let runtime_model = created["configOptions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["id"] == "model")
        .and_then(|v| v["currentValue"].as_str())
        .ok_or(AcpFailure::Profile)?;
    ensure!(
        home.model_metadata["configured_alias"] == runtime_model
            || home.model_metadata["configured_model_id"] == runtime_model,
        AcpFailure::Profile
    );
    let updates =
        ensure_low_thinking(home, wire, &session, &created["configOptions"], metrics).await?;
    let thought_before = wire.nonempty_thought_chunks;
    phase(metrics, "prompt_write");
    metrics["prompt_write_attempted"] = json!(true);
    wire.send(json!({"jsonrpc":"2.0","id":3,"method":"session/prompt",
        "params":{"sessionId":session,"prompt":[{"type":"text","text":prompt}]}}))
        .await?;
    metrics["prompt_write_completed"] = json!(true);
    metrics["prompt_pending"] = json!(true);
    phase(metrics, "prompt_response");
    provider.publish_memory_progress(metrics);
    let mut text = String::new();
    loop {
        let packet = wire.next(3).await?;
        let packet_observed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let new_thought = wire.nonempty_thought_chunks > thought_before
            && !metrics["nonempty_thought_event_observed"]
                .as_bool()
                .unwrap_or(false);
        let new_message = matches!(&packet, Packet::Text(value) if !value.is_empty())
            && !metrics["agent_message_text_observed"]
                .as_bool()
                .unwrap_or(false);
        if new_thought || new_message {
            verified_profile(home, &session, Some("low"), updates, metrics).await?;
            phase(metrics, "prompt_response");
            observe_output(metrics, new_thought, new_message, packet_observed_ms);
            provider.publish_memory_progress(metrics);
        }
        match packet {
            Packet::Text(value) => {
                ensure!(
                    value.len() <= MEMORY_TEXT_LIMIT.saturating_sub(text.len()),
                    AcpFailure::OutputLimit
                );
                text.push_str(&value);
                metrics["message_text_bytes"] = json!(text.len());
            }
            Packet::Response(value) => {
                let reason = stop_reason(&value)?;
                metrics["prompt_pending"] = json!(false);
                metrics["prompt_stop_reason"] = json!(reason);
                provider.publish_memory_progress(metrics);
                ensure!(reason == "end_turn", AcpFailure::Protocol);
                break;
            }
            Packet::Ignored => {}
        }
    }
    verified_profile(home, &session, Some("low"), updates, metrics).await?;
    phase(metrics, "final_validation");
    // end_turn alone is insufficient: 2.3.1 can map failed turns to end_turn.
    let reply = structured_reply(&text)?;
    metrics["final_validated_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    phase(metrics, "session_close");
    wire.request(4, "session/close", json!({"sessionId":session}))
        .await?;
    Ok(reply)
}

fn observe_output(metrics: &mut Value, thought: bool, message: bool, elapsed_ms: f64) {
    if thought {
        metrics["nonempty_thought_event_observed"] = json!(true);
        metrics["first_nonempty_thought_event_ms"] = json!(elapsed_ms);
    }
    if message {
        metrics["agent_message_text_observed"] = json!(true);
        metrics["first_nonempty_message_event_ms"] = json!(elapsed_ms);
    }
    metrics["agent_output_observed"] =
        json!(thought || message || metrics["agent_output_observed"] == true);
}

fn structured_reply(text: &str) -> Result<Value> {
    let reply: Value = serde_json::from_str(text).map_err(|_| AcpFailure::Protocol)?;
    memory_contract::validate_response(&reply).map_err(|_| AcpFailure::Protocol)?;
    Ok(reply)
}

async fn cancel_and_observe<R: AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(
    wire: &mut Wire<R, W>,
    metrics: &mut Value,
    budget: Duration,
) {
    if metrics["prompt_write_completed"] != true || metrics["prompt_pending"] != true {
        metrics["cancel_observation_code"] = json!("no_complete_pending_prompt");
        return;
    }
    let Some(session) = wire.session.clone() else {
        return;
    };
    metrics["cancel_notification_attempted"] = json!(true);
    let observation = tokio::time::timeout(budget, async {
        wire.send(
            json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}}),
        )
        .await?;
        metrics["cancel_notification_write_completed"] = json!(true);
        loop {
            match wire.next(3).await? {
                Packet::Response(value) => {
                    let reason = stop_reason(&value)?;
                    metrics["prompt_pending"] = json!(false);
                    metrics["prompt_stop_reason"] = json!(reason);
                    metrics["agent_cancel_terminal_observed"] = json!(reason == "cancelled");
                    metrics["cancel_observation_code"] = json!(if reason == "cancelled" {
                        "matching_prompt_cancelled_before_close"
                    } else {
                        "different_prompt_terminal_after_cancel"
                    });
                    return Ok::<(), anyhow::Error>(());
                }
                // Post-cancel output never becomes a memory response or a
                // pre-cancellation progress observation.
                Packet::Text(_) | Packet::Ignored => {}
            }
        }
    })
    .await;
    match observation {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            metrics["cancel_observation_code"] =
                json!(acp_failure(&error).unwrap_or(AcpFailure::Operation).code());
        }
        Err(_) => metrics["cancel_observation_code"] = json!("cancel_receipt_timeout"),
    }
}

#[cfg(test)]
#[path = "kimi_acp_memory_tests.rs"]
mod tests;
