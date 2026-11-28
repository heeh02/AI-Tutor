//! Explicit ACP segment generation on the existing KimiProvider foreground gate.
//! Whole teaching generate remains stream-json; memory uses its own ACP workflow.
use super::{acp_common::*, tutor_segments::*, *};
use tokio::{io::AsyncWriteExt, sync::mpsc};

const INPUT_LIMIT: usize = 128 * 1024;
pub const SEGMENT_CHANNEL_CAPACITY: usize = 2;

#[path = "foreground_provider.rs"]
mod foreground_provider;
pub(super) use foreground_provider::ForegroundRuntime;

#[derive(Clone, Copy)]
enum Phase {
    Prepare,
    Queue,
    Spawn,
    Initialize,
    SessionNew,
    Profile,
    PromptWrite,
    PromptResponse,
    SegmentValidation,
    FinalValidation,
    SessionClose,
    Cleanup,
}
fn phase(metrics: &mut Value, value: Phase) {
    metrics["phase"] = json!(match value {
        Phase::Prepare => "prepare",
        Phase::Queue => "queue",
        Phase::Spawn => "spawn",
        Phase::Initialize => "initialize",
        Phase::SessionNew => "session_new",
        Phase::Profile => "profile",
        Phase::PromptWrite => "prompt_write",
        Phase::PromptResponse => "prompt_response",
        Phase::SegmentValidation => "segment_validation",
        Phase::FinalValidation => "final_validation",
        Phase::SessionClose => "session_close",
        Phase::Cleanup => "cleanup",
    });
}
fn initial_metrics(request: &Value, generation: u64) -> Value {
    json!({"transport":"acp_segments_v1","request_id":request["request_id"],"generation":generation,
        "phase":"prepare","prompt_write_attempted":false,"prompt_write_completed":false,
        "requested_thinking":"low","effective_thinking":null,"segments_enqueued":0,
        "first_delta_ms":null,"first_validated_segment_ms":null,"first_segment_enqueued_ms":null,
        "final_validated_ms":null,"first_thought_event_ms":null,"operation_success":null,
        "operation_failure_code":null,"failure_code":null,"failure_phase":null,"remote_rpc_code":null,
        "citation_diagnostic":null,"cleanup_failure_code":null,"process_reaped":null,"owned_directory_removed":null,
        "stderr_drained":null,"stderr_bytes":null,"success":false,
        "time_origin":"before_session_prompt_write","delivery_receipts_owned_by_consumer":true})
}
#[cfg(test)]
fn diagnostic_model_receipt(
    home: &AcpHome,
    options: &Value,
    metrics: &mut Value,
    field: &str,
) -> Result<()> {
    let models: Vec<_> = options
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["id"] == "model")
        .collect();
    ensure!(models.len() == 1, AcpFailure::Profile);
    let resolved = models[0]["currentValue"]
        .as_str()
        .ok_or(AcpFailure::Profile)?;
    ensure!(
        home.model_metadata["configured_alias"] == resolved
            || home.model_metadata["configured_model_id"] == resolved,
        AcpFailure::Profile
    );
    metrics[field] = json!(resolved);
    Ok(())
}
fn record_operation<T>(metrics: &mut Value, result: &Result<T>) {
    metrics["operation_success"] = json!(result.is_ok());
    metrics["citation_diagnostic"] = result
        .as_ref()
        .err()
        .and_then(crate::tutor::citation::diagnostic)
        .unwrap_or(Value::Null);
    if let Err(error) = result {
        let failure = acp_failure(error).unwrap_or(AcpFailure::Operation);
        let code = contract_failure_code(error).unwrap_or(failure.code());
        metrics["operation_failure_code"] = json!(code);
        metrics["failure_code"] = json!(code);
        metrics["failure_phase"] = metrics["phase"].clone();
        metrics["remote_rpc_code"] = json!(failure.rpc_code());
    }
}
fn record_cleanup(metrics: &mut Value, reaped: bool, cleaned: bool, stderr_ok: bool) -> bool {
    phase(metrics, Phase::Cleanup);
    metrics["process_reaped"] = json!(reaped);
    metrics["owned_directory_removed"] = json!(cleaned);
    metrics["stderr_drained"] = json!(stderr_ok);
    let complete = reaped && cleaned && stderr_ok;
    if !complete {
        metrics["cleanup_failure_code"] = json!(AcpFailure::Cleanup.code());
        if metrics["failure_code"].is_null() {
            metrics["failure_code"] = json!(AcpFailure::Cleanup.code());
            metrics["failure_phase"] = json!("cleanup");
        }
    }
    complete
}

/// A test transport replaces only transport I/O. The real provider gate,
/// cancellation registration, decoder and delivery consumer remain in use.
#[cfg(test)]
pub(crate) struct ScriptedSegmentRequest {
    pub request: Value,
    pub generation: u64,
    pub response: tokio::sync::oneshot::Sender<Vec<Value>>,
}

#[cfg(test)]
impl KimiProvider {
    pub(crate) fn scripted_segments_for_test(&self) -> mpsc::Receiver<ScriptedSegmentRequest> {
        let (sender, receiver) = mpsc::channel(2);
        assert!(self
            .scripted_segments
            .lock()
            .unwrap()
            .replace(sender)
            .is_none());
        let mut state = self.state.lock().unwrap();
        state.ready = true;
        state.audit = json!({"available":true,"remote_model":false,"test_transport":true});
        receiver
    }
    async fn run_scripted_segments(
        &self,
        transport: mpsc::Sender<ScriptedSegmentRequest>,
        request: &Value,
        generation: u64,
        output: &mpsc::Sender<TutorSegment>,
        cancellation: &mut watch::Receiver<bool>,
    ) -> Result<ValidatedTutorReply> {
        self.increment("requests");
        self.increment(if self.foreground.is_kimi() {
            "acp_segment_requests"
        } else {
            "deepseek_segment_requests"
        });
        let (response, receive) = tokio::sync::oneshot::channel();
        let call = ScriptedSegmentRequest {
            request: request.clone(),
            generation,
            response,
        };
        let result = async {
            tokio::select! { biased;
                _=cancellation.changed()=>bail!("请求已取消"),
                sent=transport.send(call)=>sent.map_err(|_|anyhow!("scripted transport closed"))?,
            }
            let records = tokio::select! { biased;
                _=cancellation.changed()=>bail!("请求已取消"),
                received=receive=>received.map_err(|_|anyhow!("scripted reply cancelled"))?,
            };
            ensure!(records.len() <= MAX_SEGMENTS + 1, "scripted record count exceeds contract");
            let mut decoder = if self.foreground.is_kimi() {
                SegmentDecoder::new(request, generation)?
            } else {
                SegmentDecoder::new_metadata(request, generation)?
            };
            for record in records {
                ensure!(!*cancellation.borrow(), "请求已取消");
                for segment in decoder.push(format!("{record}\n").as_bytes())? {
                    tokio::select! { biased;
                        _=cancellation.changed()=>bail!("请求已取消"),
                        sent=output.send(segment)=>sent.map_err(|_|anyhow!("scripted consumer closed"))?,
                    }
                }
            }
            decoder.finish_clean_end()
        }.await;
        self.increment(if result.is_ok() {
            "completed"
        } else if *cancellation.borrow() {
            "cancelled"
        } else {
            "errors"
        });
        result
    }
}

pub(super) enum Packet {
    Response(Value),
    Text(String),
    Ignored,
}

pub(super) struct Wire<R, W> {
    reader: RpcReader<R>,
    pub(super) writer: W,
    pub(super) session: Option<String>,
    pub(super) message_chunks: usize,
    pub(super) thought_chunks_discarded: usize,
    pub(super) nonempty_thought_chunks: usize,
}
impl<R: AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin> Wire<R, W> {
    pub(super) fn new(reader: R, writer: W) -> Self {
        Self {
            reader: RpcReader::new(reader),
            writer,
            session: None,
            message_chunks: 0,
            thought_chunks_discarded: 0,
            nonempty_thought_chunks: 0,
        }
    }
    pub(super) async fn send(&mut self, packet: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&packet)?;
        ensure!(bytes.len() <= INPUT_LIMIT, AcpFailure::InputLimit);
        bytes.push(b'\n');
        tokio::time::timeout(Duration::from_secs(2), self.writer.write_all(&bytes))
            .await
            .map_err(|_| AcpFailure::WriteTimeout)?
            .map_err(|_| AcpFailure::Write)?;
        Ok(())
    }
    pub(super) async fn next(&mut self, id: u64) -> Result<Packet> {
        let packet = self.reader.next().await?;
        ensure!(
            packet.is_object() && packet["jsonrpc"] == "2.0",
            AcpFailure::Envelope
        );
        if let Some(method) = packet["method"].as_str() {
            if packet.get("id").is_some() {
                let reply = if method == "session/request_permission" {
                    json!({"jsonrpc":"2.0","id":packet["id"],"result":{"outcome":{"outcome":"cancelled"}}})
                } else {
                    json!({"jsonrpc":"2.0","id":packet["id"],"error":{"code":-32601,"message":"Client capability disabled"}})
                };
                self.send(reply).await?;
                bail!(AcpFailure::Capability);
            }
            ensure!(method == "session/update", AcpFailure::Protocol);
            let params = &packet["params"];
            ensure!(
                self.session.is_some() && self.session.as_deref() == params["sessionId"].as_str(),
                AcpFailure::Protocol
            );
            let update = &params["update"];
            match update["sessionUpdate"].as_str() {
                Some("agent_message_chunk") => {
                    ensure!(update["content"]["type"] == "text", AcpFailure::Protocol);
                    let text = update["content"]["text"]
                        .as_str()
                        .ok_or(AcpFailure::Protocol)?;
                    self.message_chunks += 1;
                    Ok(Packet::Text(text.to_owned()))
                }
                Some("agent_thought_chunk") => {
                    self.thought_chunks_discarded += 1;
                    // Count only verified nonempty text shapes for generation
                    // observation; never copy thought text into application state.
                    if update["content"]["type"] == "text"
                        && update["content"]["text"]
                            .as_str()
                            .is_some_and(|text| !text.is_empty())
                    {
                        self.nonempty_thought_chunks += 1;
                    }
                    Ok(Packet::Ignored)
                }
                Some(
                    "available_commands_update"
                    | "config_option_update"
                    | "current_mode_update"
                    | "usage_update"
                    | "session_info_update",
                ) => Ok(Packet::Ignored),
                Some("tool_call" | "tool_call_update" | "plan") => bail!(AcpFailure::Capability),
                _ => bail!(AcpFailure::Protocol),
            }
        } else {
            ensure!(packet["id"].as_u64() == Some(id), AcpFailure::ResponseId);
            if let Some(error) = packet.get("error") {
                // Numeric JSON-RPC code only; never retain remote message/data.
                bail!(AcpFailure::RemoteRpc(error["code"].as_i64()));
            }
            ensure!(
                packet.get("result").is_some()
                    && (packet["result"].is_object() || packet["result"].is_null()),
                AcpFailure::Protocol
            );
            Ok(Packet::Response(packet["result"].clone()))
        }
    }
    pub(super) async fn request(&mut self, id: u64, method: &str, params: Value) -> Result<Value> {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        loop {
            match self.next(id).await? {
                Packet::Response(value) => {
                    ensure!(
                        value.is_object() || method == "session/close",
                        AcpFailure::Protocol
                    );
                    return Ok(value);
                }
                Packet::Text(_) => bail!(AcpFailure::Protocol),
                Packet::Ignored => {}
            }
        }
    }
    async fn cancel(&mut self) {
        if let Some(session) = self.session.clone() {
            let _ = tokio::time::timeout(
                Duration::from_millis(250),
                self.send(json!({
                "jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}})),
            )
            .await;
        }
    }
}

fn stream_prompt(policy: &str, reply_schema: Value, request: &Value) -> Result<String> {
    const OLD_FORMAT: &str =
        "Return one JSON object only, exactly matching the supplied TutorReply schema.";
    ensure!(
        policy.matches(OLD_FORMAT).count() == 1,
        "教学策略输出格式尚未适配流式契约"
    );
    let policy = policy.replace(OLD_FORMAT,
        "Return only newline-terminated JSONL records matching OUTPUT_SCHEMA. Emit 1 to 12 segment records in order, then exactly one final record and no more text. Each segment is one short teachable sentence, 1 to 320 Unicode characters; keep the entire reply at most 1600 characters. Sequence starts at 0. The final reply.reply_text must equal the exact concatenation of all segment.text strings, including spaces and punctuation; do not rewrite earlier sentences. Include language-appropriate sentence spacing in segment.text itself: an English sentence after another sentence normally starts with a space. No separator is inserted between segments. Every segment used_evidence_ids must be available in this request, and final reply.used_evidence_ids must include their union. Do not emit task or memory candidates before final. End every record, including final, with a newline.");
    let schema = json!({"oneOf":[
        {"type":"object","additionalProperties":false,"required":["schema_version","type","request_id","sequence","text","used_evidence_ids"],
         "properties":{"schema_version":{"const":"1.0"},"type":{"const":"segment"},"request_id":{"const":request["request_id"]},"sequence":{"type":"integer","minimum":0,"maximum":MAX_SEGMENTS-1},"text":{"type":"string","minLength":1,"maxLength":MAX_SEGMENT_CHARS},"used_evidence_ids":{"type":"array","maxItems":20,"uniqueItems":true,"items":{"type":"string"}}}},
        {"type":"object","additionalProperties":false,"required":["schema_version","type","request_id","segment_count","reply"],
         "properties":{"schema_version":{"const":"1.0"},"type":{"const":"final"},"request_id":{"const":request["request_id"]},"segment_count":{"type":"integer","minimum":1,"maximum":MAX_SEGMENTS},"reply":reply_schema}}
    ]});
    build_prompt(&policy, &schema, request)
}

fn complete_prompt(policy: &str, reply_schema: Value, request: &Value) -> Result<String> {
    build_prompt(policy, &reply_schema, request)
}

/// Explicit metadata-final protocol. Legacy formatting above stays unchanged.
fn stream_prompt_metadata(
    policy: &str,
    mut reply_schema: Value,
    request: &Value,
) -> Result<String> {
    const OLD_FORMAT: &str =
        "Return one JSON object only, exactly matching the supplied TutorReply schema.";
    ensure!(
        policy.matches(OLD_FORMAT).count() == 1,
        "metadata_prompt_format_marker"
    );
    ensure!(
        reply_schema["additionalProperties"] == false,
        "metadata_reply_schema_must_be_closed"
    );
    let properties = reply_schema["properties"]
        .as_object_mut()
        .ok_or_else(|| anyhow!("metadata_reply_schema_properties_missing"))?;
    ensure!(
        properties.remove("reply_text").is_some(),
        "metadata_reply_text_schema_missing"
    );
    let required = reply_schema["required"]
        .as_array_mut()
        .ok_or_else(|| anyhow!("metadata_reply_schema_required_missing"))?;
    ensure!(
        required
            .iter()
            .filter(|v| v.as_str() == Some("reply_text"))
            .count()
            == 1,
        "metadata_reply_text_requirement_missing"
    );
    required.retain(|v| v.as_str() != Some("reply_text"));
    let policy = policy.replace(OLD_FORMAT,
        "Return only newline-terminated JSONL records matching OUTPUT_SCHEMA using metadata_v2. Emit 1 to 12 segment records in order, then exactly one final_metadata record and no more text. Each segment is one short teachable sentence, 1 to 320 Unicode characters; keep the entire reply at most 1600 characters. Sequence starts at 0. Generate teaching text only in segment.text. final_metadata.reply must omit reply_text entirely, including null: provide only the other required reply metadata. The server derives standard TutorReply 1.0 reply_text from the exact concatenation of validated segment.text strings. Include language-appropriate sentence spacing in segment.text itself: an English sentence after another sentence normally starts with a space. No separator is inserted between segments. Every segment used_evidence_ids must be available in this request, and final_metadata.reply.used_evidence_ids must include their union. Do not emit task or memory candidates before final_metadata. End every record, including final_metadata, with a newline.");
    let schema = json!({"oneOf":[
        {"type":"object","additionalProperties":false,"required":["schema_version","type","request_id","sequence","text","used_evidence_ids"],
         "properties":{"schema_version":{"const":"1.0"},"type":{"const":"segment"},"request_id":{"const":request["request_id"]},"sequence":{"type":"integer","minimum":0,"maximum":MAX_SEGMENTS-1},"text":{"type":"string","minLength":1,"maxLength":MAX_SEGMENT_CHARS},"used_evidence_ids":{"type":"array","maxItems":20,"uniqueItems":true,"items":{"type":"string"}}}},
        {"type":"object","additionalProperties":false,"required":["schema_version","type","request_id","segment_count","reply"],
         "properties":{"schema_version":{"const":"1.0"},"type":{"const":"final_metadata"},"request_id":{"const":request["request_id"]},"segment_count":{"type":"integer","minimum":1,"maximum":MAX_SEGMENTS},"reply":reply_schema}}
    ]});
    build_prompt(&policy, &schema, request)
}

async fn publish(sender: &mpsc::Sender<TutorSegment>, segment: TutorSegment) -> Result<()> {
    // This wait is inside the outer cancellation/deadline select. A slow
    // consumer cannot create an unbounded queue or delay cancellation.
    sender
        .send(segment)
        .await
        .map_err(|_| AcpFailure::ConsumerClosed.into())
}

pub(super) async fn verified_profile(
    home: &AcpHome,
    session: &str,
    expected_thinking: Option<&str>,
    min_updates: usize,
    metrics: &mut Value,
) -> Result<Value> {
    phase(metrics, Phase::Profile);
    let proof = prove_profile(home, session, expected_thinking, min_updates)
        .await
        .map_err(|_| AcpFailure::Profile)?;
    metrics["profile_verifications"] =
        json!(metrics["profile_verifications"].as_u64().unwrap_or(0) + 1);
    metrics["profile_verification_ms"] = json!(
        metrics["profile_verification_ms"].as_f64().unwrap_or(0.0)
            + proof["verification_ms"].as_f64().unwrap_or(0.0)
    );
    Ok(proof)
}

pub(super) async fn ensure_low_thinking<R: AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(
    home: &AcpHome,
    wire: &mut Wire<R, W>,
    session: &str,
    options: &Value,
    metrics: &mut Value,
) -> Result<usize> {
    phase(metrics, Phase::Profile);
    let initial = thinking_option(options).map_err(|_| AcpFailure::Profile)?;
    let proof =
        verified_profile(home, session, initial["current_value"].as_str(), 0, metrics).await?;
    ensure!(
        initial["options"]
            .as_array()
            .is_some_and(|v| v.iter().any(|v| v == "low")),
        AcpFailure::Profile
    );
    let mut updates = proof["thinking_update_records"].as_u64().unwrap_or(0) as usize;
    metrics["initial_thinking"] = initial["current_value"].clone();
    metrics["thinking_config_rpc_count"] = json!(0);
    // safe_config already requests low. Keep the exact durable profile proof,
    // and mutate only when the actual session reports a different effort.
    if initial["current_value"] != "low" {
        metrics["thinking_config_rpc_count"] = json!(1);
        let configured = wire
            .request(
                5,
                "session/set_config_option",
                json!({"sessionId":session,"configId":"thinking","value":"low"}),
            )
            .await?;
        #[cfg(test)]
        if metrics["diagnostic_model_pin"] == true {
            diagnostic_model_receipt(
                home,
                &configured["configOptions"],
                metrics,
                "resolved_model_after_thinking",
            )?;
            metrics["model_after_thinking_receipt_basis"] = json!("set_config_option_result");
        }
        ensure!(
            thinking_option(&configured["configOptions"]).map_err(|_| AcpFailure::Profile)?
                ["current_value"]
                == "low",
            AcpFailure::Profile
        );
        updates += 1;
        verified_profile(home, session, Some("low"), updates, metrics).await?;
    }
    metrics["effective_thinking"] = json!("low");
    Ok(updates)
}

async fn workflow<R: AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(
    home: &AcpHome,
    wire: &mut Wire<R, W>,
    request: &Value,
    generation: u64,
    prompt: &str,
    sender: &mpsc::Sender<TutorSegment>,
    metrics: &mut Value,
) -> Result<ValidatedTutorReply> {
    let setup_started = Instant::now();
    phase(metrics, Phase::Initialize);
    let init = wire.request(1, "initialize", json!({"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"AI Tutor validated segments","version":"1"}})).await?;
    ensure!(
        init["agentInfo"]["version"] == VERSION && init["protocolVersion"] == 1,
        AcpFailure::Protocol
    );
    phase(metrics, Phase::SessionNew);
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
    #[cfg(test)]
    if metrics["diagnostic_model_pin"] == true {
        metrics["configured_model"] = home.model_metadata.clone();
        diagnostic_model_receipt(
            home,
            &created["configOptions"],
            metrics,
            "resolved_model_at_session_new",
        )?;
        metrics["materialized_system_sha256"] = json!(format!(
            "{:x}",
            Sha256::digest(home.expected_prompt.as_bytes())
        ));
        metrics["materialized_user_sha256"] =
            json!(format!("{:x}", Sha256::digest(prompt.as_bytes())));
    }
    let updates =
        ensure_low_thinking(home, wire, &session, &created["configOptions"], metrics).await?;
    #[cfg(test)]
    if metrics["diagnostic_model_pin"] == true && metrics["thinking_config_rpc_count"] == 0 {
        metrics["resolved_model_after_thinking"] = metrics["resolved_model_at_session_new"].clone();
        metrics["model_after_thinking_receipt_basis"] =
            json!("session_new_already_low_no_mutation_rpc");
    }
    let mut decoder = SegmentDecoder::new(request, generation)?;
    metrics["session_setup_ms"] = json!(setup_started.elapsed().as_secs_f64() * 1000.0);
    let thoughts_before_prompt = wire.thought_chunks_discarded;
    let started = Instant::now();
    phase(metrics, Phase::PromptWrite);
    metrics["prompt_write_attempted"] = json!(true);
    wire.send(json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":session,"prompt":[{"type":"text","text":prompt}]}})).await?;
    // This is a local pipe write receipt, not remote receipt or model acceptance.
    metrics["prompt_write_completed"] = json!(true);
    loop {
        phase(metrics, Phase::PromptResponse);
        let packet = wire.next(3).await?;
        if wire.thought_chunks_discarded > thoughts_before_prompt
            && metrics["first_thought_event_ms"].is_null()
        {
            // Timing only. Thought text is discarded and never retained.
            metrics["first_thought_event_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
        }
        match packet {
            Packet::Text(text) => {
                if !text.is_empty() && metrics["first_delta_ms"].is_null() {
                    metrics["first_delta_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
                }
                phase(metrics, Phase::SegmentValidation);
                let segments = match decoder.push(text.as_bytes()) {
                    Ok(segments) => segments,
                    Err(error) => {
                        metrics["contract_diagnostics"] = decoder.diagnostics();
                        return Err(error);
                    }
                };
                for segment in segments {
                    // A previously proven profile is not enough after a config
                    // notification: fold the current durable journal again.
                    verified_profile(home, &session, Some("low"), updates, metrics).await?;
                    phase(metrics, Phase::SegmentValidation);
                    if metrics["first_validated_segment_ms"].is_null() {
                        metrics["first_validated_segment_ms"] =
                            json!(started.elapsed().as_secs_f64() * 1000.0);
                    }
                    publish(sender, segment).await?;
                    metrics["segments_enqueued"] =
                        json!(metrics["segments_enqueued"].as_u64().unwrap_or(0) + 1);
                    if metrics["first_segment_enqueued_ms"].is_null() {
                        metrics["first_segment_enqueued_ms"] =
                            json!(started.elapsed().as_secs_f64() * 1000.0);
                    }
                }
            }
            Packet::Response(result) => {
                ensure!(result["stopReason"] == "end_turn", AcpFailure::Protocol);
                break;
            }
            Packet::Ignored => {}
        }
    }
    verified_profile(home, &session, Some("low"), updates, metrics).await?;
    phase(metrics, Phase::FinalValidation);
    metrics["contract_diagnostics"] = decoder.diagnostics();
    let delimiter = if metrics["contract_diagnostics"]["newline_final_validated"] == true {
        "newline"
    } else {
        "clean_end_turn_eof"
    };
    let reply = decoder.finish_clean_end()?;
    metrics["final_delimiter"] = json!(delimiter);
    metrics["final_validated_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    phase(metrics, Phase::SessionClose);
    wire.request(4, "session/close", json!({"sessionId":session}))
        .await?;
    Ok(reply)
}

/// The production workflow and its existing cancellation/deadline/stderr race.
/// A duration argument allows a bounded local fixture; production passes DEADLINE.
async fn await_workflow(
    operation: impl std::future::Future<Output = Result<ValidatedTutorReply>>,
    cancellation: &mut watch::Receiver<bool>,
    sender: &mpsc::Sender<TutorSegment>,
    stderr_task: &mut tokio::task::JoinHandle<Result<usize>>,
    timeout: Duration,
) -> (Result<ValidatedTutorReply>, bool, bool, Option<usize>) {
    tokio::pin!(operation);
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut stderr_done = false;
    let mut stderr_ok = true;
    let mut stderr_bytes = None;
    let result = loop {
        tokio::select! { biased;
            _=cancellation.changed()=>break Err(AcpFailure::Cancelled.into()),
            _=&mut deadline=>break Err(AcpFailure::Deadline.into()),
            result=&mut operation=>break result,
            _=sender.closed()=>break Err(AcpFailure::ConsumerClosed.into()),
            result=&mut *stderr_task,if !stderr_done=>{
                stderr_done=true;
                match result {
                    Ok(Ok(bytes)) => stderr_bytes=Some(bytes),
                    _ => { stderr_ok=false; break Err(AcpFailure::Stderr.into()); }
                }
            }
        }
    };
    (result, stderr_done, stderr_ok, stderr_bytes)
}

impl KimiProvider {
    /// Optional API; callers must consume a bounded channel concurrently and
    /// retain their delivery ledger on failure. No UI/Store/TTS writes occur.
    /// Cancel through this provider's cancel/cancel_owner, not task abortion.
    pub async fn generate_segments(
        &self,
        request: &Value,
        generation: u64,
        sender: mpsc::Sender<TutorSegment>,
    ) -> Result<ValidatedTutorReply> {
        let kimi = self.foreground.is_kimi();
        let mut metrics = if kimi {
            initial_metrics(request, generation)
        } else {
            let selected = self.foreground_status();
            json!({"transport":if selected["provider"] == "deepseek_flash" {"deepseek_http"} else {"foreground_unavailable"},
                "request_id":request["request_id"],"generation":generation,
                "requested_model":selected["model"],"stream_contract":selected["stream_contract"],
                "reply_text_origin":selected["reply_text_origin"],"operation_success":null,"success":false})
        };
        #[cfg(test)]
        if let Some(alias) = &self.diagnostic_model_alias {
            metrics["diagnostic_model_pin"] = json!(true);
            metrics["requested_model_alias"] = json!(alias);
        }
        let result = self
            .generate_segments_inner(request, generation, sender, &mut metrics)
            .await;
        // Preparation/spawn failures also replace the previous call's metrics.
        // Cleanup fields stay null when that outcome was not explicitly observed.
        if kimi {
            if metrics["operation_success"].is_null() {
                record_operation(&mut metrics, &result);
            }
        } else {
            metrics["operation_success"] = json!(result.is_ok());
            if let Err(error) = &result {
                if let Some(code) = contract_failure_code(error) {
                    metrics["failure_code"] = json!(code);
                } else if matches!(acp_failure(error), Some(AcpFailure::Cancelled)) {
                    metrics["failure_code"] = json!("request_cancelled");
                } else if metrics["failure_code"].is_null() {
                    metrics["failure_code"] = json!("foreground_operation_failed");
                }
            }
        }
        metrics["success"] = json!(result.is_ok());
        self.foreground.record(&metrics, result.is_ok());
        let mut state = self.state.lock().unwrap();
        state.metrics["last_foreground_segments"] = metrics.clone();
        if kimi {
            state.metrics["last_acp_segments"] = metrics;
        }
        result
    }
    async fn generate_segments_inner(
        &self,
        request: &Value,
        generation: u64,
        sender: mpsc::Sender<TutorSegment>,
        metrics: &mut Value,
    ) -> Result<ValidatedTutorReply> {
        phase(metrics, Phase::Prepare);
        if self.foreground.is_kimi() {
            ensure!(
                self.state.lock().unwrap().ready,
                "Kimi尚未完成原生无工具验证"
            );
        } else {
            self.foreground.ensure_ready()?;
        }
        ensure!(
            sender.max_capacity() <= SEGMENT_CHANNEL_CAPACITY,
            "教学片段通道容量不得超过2"
        );
        SegmentDecoder::new(request, generation)?;
        let rid = s(request, "request_id");
        let (_guard, _, mut cancellation) =
            self.register_call(rid, s(request, "learner_id"), false)?;
        let queued = Instant::now();
        phase(metrics, Phase::Queue);
        let _permit = tokio::select! { biased; _=cancellation.changed()=>bail!(AcpFailure::Cancelled), permit=self.gate.acquire()=>permit? };
        let acquired = Instant::now();
        ensure!(!*cancellation.borrow(), AcpFailure::Cancelled);
        #[cfg(test)]
        {
            let transport = self.scripted_segments.lock().unwrap().clone();
            if let Some(transport) = transport {
                return self
                    .run_scripted_segments(
                        transport,
                        request,
                        generation,
                        &sender,
                        &mut cancellation,
                    )
                    .await;
            }
        }
        if !self.foreground.is_kimi() {
            metrics["queue_wait_ms"] =
                json!(acquired.duration_since(queued).as_secs_f64() * 1000.0);
            return self
                .run_flash_stream(request, generation, sender, cancellation, metrics)
                .await;
        }
        phase(metrics, Phase::Prepare);
        let home = prepare_home(self, true, s(request, "learner_id"), Some(rid), false)
            .map_err(|_| AcpFailure::Prepare)?;
        let policy = fs::read_to_string(self.root.join("prompts/tutor_core.md"))
            .map_err(|_| AcpFailure::Prepare)?;
        let schema = serde_json::from_slice(&read_bounded_file(&self.root.join("implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json"),128*1024).map_err(|_| AcpFailure::Prepare)?)
            .map_err(|_| AcpFailure::Prepare)?;
        let prompt = stream_prompt(&policy, schema, request).map_err(|_| AcpFailure::Prepare)?;
        ensure!(!*cancellation.borrow(), AcpFailure::Cancelled);
        let launching = Instant::now();
        phase(metrics, Phase::Spawn);
        let mut child = isolated_command(self, &home)
            .spawn()
            .map_err(|_| AcpFailure::Spawn)?;
        let job = match ProcessJob::attach(&child) {
            Ok(job) => job,
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
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
        metrics["queue_wait_ms"] = json!(acquired.duration_since(queued).as_secs_f64() * 1000.0);
        metrics["preparation_ms"] =
            json!(launching.duration_since(acquired).as_secs_f64() * 1000.0);
        metrics["process_spawn_ms"] =
            json!(started.duration_since(launching).as_secs_f64() * 1000.0);
        metrics["prompt_chars"] = json!(prompt.chars().count());
        metrics["prompt_utf8_bytes"] = json!(prompt.len());
        self.increment("requests");
        self.increment("acp_segment_requests");
        let (result, stderr_done, mut stderr_ok, mut stderr_bytes) = {
            let operation = workflow(
                &home, &mut wire, request, generation, &prompt, &sender, metrics,
            );
            await_workflow(
                operation,
                &mut cancellation,
                &sender,
                &mut stderr_task,
                DEADLINE,
            )
            .await
        };
        record_operation(metrics, &result);
        if result.is_err() {
            wire.cancel().await;
        }
        let _ = tokio::time::timeout(Duration::from_millis(200), wire.writer.shutdown()).await;
        drop(job);
        let _ = child.kill().await;
        let reaped = matches!(
            tokio::time::timeout(Duration::from_secs(2), child.wait()).await,
            Ok(Ok(_))
        );
        if !stderr_done {
            match tokio::time::timeout(Duration::from_secs(1), &mut stderr_task).await {
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
        metrics["total_duration_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
        metrics["stderr_bytes"] = json!(stderr_bytes);
        let cleanup_ok = record_cleanup(metrics, reaped, cleaned, stderr_ok);
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
                ensure!(cleanup_ok, AcpFailure::Cleanup);
                self.increment("completed");
                Ok(reply)
            }
        }
    }
}

#[cfg(test)]
#[path = "kimi_acp_stream_tests.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "kimi_highspeed_diagnostic.rs"]
mod highspeed_diagnostic;

#[cfg(windows)]
#[path = "deepseek_client.rs"]
mod deepseek_client;

#[cfg(all(test, windows))]
#[path = "deepseek_flash_diagnostic.rs"]
mod deepseek_flash_diagnostic;

#[cfg(all(test, windows))]
#[path = "teaching_eval_diagnostic.rs"]
mod teaching_eval_diagnostic;
