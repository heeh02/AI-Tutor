use super::*;

fn update(session: &str, kind: &str, text: &str) -> Value {
    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":session,
        "update":{"sessionUpdate":kind,"content":{"type":"text","text":text}}}})
}
fn wire_bytes(packets: &[Value]) -> Vec<u8> {
    packets
        .iter()
        .map(|v| format!("{v}\n"))
        .collect::<String>()
        .into_bytes()
}

#[tokio::test]
async fn wire_discards_thoughts_and_exposes_only_assistant_text_events() -> Result<()> {
    let bytes = wire_bytes(&[
        update(
            "current",
            "agent_thought_chunk",
            "PRIVATE_THOUGHT_NEVER_DELIVER",
        ),
        update("current", "agent_message_chunk", "中文 assistant"),
        json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}),
    ]);
    let mut wire = Wire::new(bytes.as_slice(), tokio::io::sink());
    wire.session = Some("current".into());
    ensure!(matches!(wire.next(3).await?, Packet::Ignored));
    ensure!(matches!(wire.next(3).await?,Packet::Text(text) if text == "中文 assistant"));
    ensure!(
        matches!(wire.next(3).await?,Packet::Response(value) if value["stopReason"] == "end_turn")
    );
    ensure!(wire.thought_chunks_discarded == 1 && wire.message_chunks == 1);
    Ok(())
}

#[tokio::test]
async fn wire_rejects_stale_session_tools_unknown_events_and_response_ids() -> Result<()> {
    for packet in [
        update("stale", "agent_message_chunk", "x"),
        update("current", "tool_call", "x"),
        update("current", "plan", "x"),
        update("current", "unknown", "x"),
        json!({"jsonrpc":"2.0","id":9,"result":{}}),
        json!({"jsonrpc":"2.0","id":3,"error":{"message":"PRIVATE_ERROR"}}),
    ] {
        let bytes = wire_bytes(&[packet]);
        let mut wire = Wire::new(bytes.as_slice(), tokio::io::sink());
        wire.session = Some("current".into());
        let error = wire
            .next(3)
            .await
            .err()
            .ok_or_else(|| anyhow!("invalid packet accepted"))?;
        ensure!(!error.to_string().contains("PRIVATE_ERROR"));
    }
    Ok(())
}

#[tokio::test]
async fn reverse_permission_is_explicitly_denied_and_other_capabilities_rejected() -> Result<()> {
    for (method, permission) in [
        ("session/request_permission", true),
        ("fs/read_text_file", false),
        ("terminal/create", false),
    ] {
        let bytes = wire_bytes(&[json!({"jsonrpc":"2.0","id":42,"method":method,"params":{}})]);
        let (writer, mut received) = tokio::io::duplex(4096);
        let mut wire = Wire::new(bytes.as_slice(), writer);
        ensure!(wire.next(3).await.is_err());
        let mut buf = [0u8; 4096];
        let n = received.read(&mut buf).await?;
        let response: Value = serde_json::from_slice(&buf[..n])?;
        ensure!(response["id"] == 42);
        if permission {
            ensure!(response["result"]["outcome"]["outcome"] == "cancelled");
        } else {
            ensure!(response["error"]["code"] == -32601);
        }
    }
    Ok(())
}

#[tokio::test]
async fn handshake_cannot_leak_text_and_input_write_is_bounded() -> Result<()> {
    let bytes = wire_bytes(&[update("current", "agent_message_chunk", "must not expose")]);
    let mut wire = Wire::new(bytes.as_slice(), tokio::io::sink());
    wire.session = Some("current".into());
    ensure!(wire.request(1, "initialize", json!({})).await.is_err());
    ensure!(
        wire.send(json!({"huge":"x".repeat(INPUT_LIMIT)}))
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn full_two_segment_queue_is_cancellable_without_delivering_a_third() -> Result<()> {
    let request = json!({"request_id":"bounded","context":{"evidence":[]}});
    let mut decoder = SegmentDecoder::new(&request, 1)?;
    let mut segments = vec![];
    for sequence in 0..3 {
        segments.extend(
            decoder.push(
                format!(
                    "{}\n",
                    json!({"schema_version":"1.0","type":"segment",
            "request_id":"bounded","sequence":sequence,"text":"x","used_evidence_ids":[]})
                )
                .as_bytes(),
            )?,
        );
    }
    let (sender, mut receiver) = mpsc::channel(SEGMENT_CHANNEL_CAPACITY);
    publish(&sender, segments[0].clone()).await?;
    publish(&sender, segments[1].clone()).await?;
    let (cancel, mut cancelled) = watch::channel(false);
    let pending = async {
        tokio::select! { biased;
            _=cancelled.changed()=>Err(anyhow!("cancelled")),
            result=publish(&sender,segments[2].clone())=>result,
        }
    };
    let cancel_later = async {
        tokio::task::yield_now().await;
        cancel.send(true).unwrap();
    };
    let (result, ()) = tokio::join!(pending, cancel_later);
    ensure!(result.is_err());
    ensure!(receiver.try_recv()?.sequence() == 0 && receiver.try_recv()?.sequence() == 1);
    ensure!(receiver.try_recv().is_err());
    drop(receiver);
    ensure!(publish(&sender, segments[2].clone()).await.is_err());
    Ok(())
}

#[test]
fn opt_in_prompt_changes_only_output_format_and_keeps_private_request_fields_out() -> Result<()> {
    let policy = concat!(
        "Grounding remains.\n",
        "Return one JSON object only, exactly matching the supplied TutorReply schema.\n",
        "Never add tool requests."
    );
    let request = json!({"request_id":"p","_private":"SECRET_INTERNAL_MARKER","context":{"reply_schema":"SHOULD_BE_REMOVED","evidence":[]}});
    let prompt = stream_prompt(policy, json!({"type":"object"}), &request)?;
    ensure!(prompt.contains("Grounding remains.") && prompt.contains("Never add tool requests."));
    ensure!(prompt.contains("newline-terminated JSONL") && prompt.contains("exact concatenation"));
    ensure!(!prompt.contains("SECRET_INTERNAL_MARKER") && !prompt.contains("SHOULD_BE_REMOVED"));
    ensure!(stream_prompt("changed policy", json!({}), &request).is_err());
    Ok(())
}

fn thinking_options(level: &str) -> Value {
    json!([{"id":"thinking","type":"select","currentValue":level,
        "options":[{"value":"low"},{"value":"high"}]}])
}

#[test]
fn metadata_prompt_changes_only_the_final_record_schema_and_keeps_request_data() -> Result<()> {
    let policy = concat!(
        "KEEP_DOMAIN_CANARY\n",
        "Return one JSON object only, exactly matching the supplied TutorReply schema.\n",
        "KEEP_TOOL_BOUNDARY_CANARY"
    );
    let schema: Value = serde_json::from_str(include_str!(
        "../../../implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json"
    ))?;
    let request = json!({"request_id":"metadata-prompt","generation":4,"_private":"PRIVATE_CANARY",
        "context":{"user_input":"Literal reply_text and final are source text.","evidence":[],"reply_schema":"DUPLICATE_SCHEMA_CANARY"}});
    let legacy = stream_prompt(policy, schema.clone(), &request)?;
    let metadata = stream_prompt_metadata(policy, schema.clone(), &request)?;
    let parts = |value: &str| -> Result<(String, Value, Value)> {
        let (policy, rest) = value
            .split_once("\n\nOUTPUT_SCHEMA:\n")
            .ok_or_else(|| anyhow!("schema section absent"))?;
        let (schema, data) = rest
            .split_once("\n\nREQUEST_DATA:\n")
            .ok_or_else(|| anyhow!("request section absent"))?;
        Ok((
            policy.into(),
            serde_json::from_str(schema)?,
            serde_json::from_str(data)?,
        ))
    };
    let (legacy_policy, legacy_schema, legacy_request) = parts(&legacy)?;
    let (metadata_policy, metadata_schema, metadata_request) = parts(&metadata)?;
    assert_eq!(legacy_request, metadata_request);
    assert_eq!(
        metadata_request["context"]["user_input"],
        request["context"]["user_input"]
    );
    assert!(metadata_request.get("_private").is_none());
    assert!(metadata_request["context"].get("reply_schema").is_none());
    for text in [&legacy_policy, &metadata_policy] {
        assert!(
            text.starts_with("KEEP_DOMAIN_CANARY\n")
                && text.ends_with("\nKEEP_TOOL_BOUNDARY_CANARY")
        );
    }
    assert_eq!(legacy_schema["oneOf"][1]["properties"]["reply"], schema);
    let mut expected = legacy_schema;
    expected["oneOf"][1]["properties"]["type"]["const"] = json!("final_metadata");
    let reply = &mut expected["oneOf"][1]["properties"]["reply"];
    reply["properties"]
        .as_object_mut()
        .unwrap()
        .remove("reply_text");
    reply["required"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v.as_str() != Some("reply_text"));
    assert_eq!(metadata_schema, expected); // All other source/schema constraints remain identical.
    assert!(schema["properties"].get("reply_text").is_some());
    Ok(())
}

#[test]
fn metadata_prompt_rejects_incompatible_reply_schema_without_legacy_fallback() -> Result<()> {
    let policy = "Return one JSON object only, exactly matching the supplied TutorReply schema.";
    let request = json!({"request_id":"metadata-prompt","context":{"evidence":[]}});
    for schema in [
        json!({"type":"object"}),
        json!({"type":"object","additionalProperties":true,"properties":{"reply_text":{}},"required":["reply_text"]}),
        json!({"type":"object","additionalProperties":false,"properties":{"reply_text":{}},"required":[]}),
    ] {
        assert!(stream_prompt_metadata(policy, schema, &request).is_err());
    }
    assert!(stream_prompt_metadata("missing format", json!({}), &request).is_err());
    assert!(stream_prompt(policy, json!({"type":"object"}), &request).is_ok());
    Ok(())
}

fn low_setup_fixture(level: &str) -> Result<(tempfile::TempDir, AcpHome, PathBuf)> {
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("run-rust-low-setup");
    let home = path.join("home");
    let journal = home.join("sessions/synthetic/session/agents/main/wire.jsonl");
    fs::create_dir_all(journal.parent().unwrap())?;
    fs::write(
        &journal,
        format!(
            "{}\n",
            json!({"type":"profile.bind","agentId":"main",
            "profileName":"agent","activeToolNames":[],"subagents":[],
            "systemPrompt":"synthetic exact no-tools policy","thinkingEffort":level})
        ),
    )?;
    let acp = AcpHome {
        run: OwnedRun {
            path: path.clone(),
            runtime: temporary.path().to_owned(),
        },
        home,
        work: path.join("work"),
        expected_prompt: "synthetic exact no-tools policy".into(),
        canary: None,
        model_metadata: json!({}),
    };
    Ok((temporary, acp, journal))
}

#[tokio::test]
async fn already_low_reuses_verified_profile_without_sending_configuration_rpc() -> Result<()> {
    let (_temporary, home, _) = low_setup_fixture("low")?;
    let (writer, mut output) = tokio::io::duplex(4096);
    let mut wire = Wire::new(&b""[..], writer);
    wire.session = Some("session".into());
    let mut metrics = json!({});
    let updates = ensure_low_thinking(
        &home,
        &mut wire,
        "session",
        &thinking_options("low"),
        &mut metrics,
    )
    .await?;
    wire.writer.shutdown().await?;
    let mut requests = vec![];
    output.read_to_end(&mut requests).await?;
    ensure!(requests.is_empty() && updates == 0);
    ensure!(metrics["thinking_config_rpc_count"] == 0 && metrics["profile_verifications"] == 1);
    ensure!(metrics["effective_thinking"] == "low");
    Ok(())
}

#[tokio::test]
async fn low_option_claim_still_requires_matching_durable_no_tools_profile() -> Result<()> {
    for corrupt_tools in [false, true] {
        let (_temporary, home, journal) =
            low_setup_fixture(if corrupt_tools { "low" } else { "high" })?;
        if corrupt_tools {
            let mut profile: Value = serde_json::from_slice(&fs::read(&journal)?)?;
            profile["activeToolNames"] = json!(["Bash"]);
            fs::write(journal, format!("{profile}\n"))?;
        }
        let (writer, mut output) = tokio::io::duplex(4096);
        let mut wire = Wire::new(&b""[..], writer);
        wire.session = Some("session".into());
        let mut metrics = json!({});
        ensure!(
            ensure_low_thinking(
                &home,
                &mut wire,
                "session",
                &thinking_options("low"),
                &mut metrics
            )
            .await
            .is_err()
        );
        wire.writer.shutdown().await?;
        let mut requests = vec![];
        output.read_to_end(&mut requests).await?;
        ensure!(requests.is_empty() && metrics["effective_thinking"].is_null());
    }
    Ok(())
}

async fn exercise_non_low_configuration(append_proof: bool) -> Result<()> {
    let (_temporary, home, journal) = low_setup_fixture("high")?;
    let (client, server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(client);
    let mut wire = Wire::new(reader, writer);
    wire.session = Some("session".into());
    let mut metrics = json!({});
    let options = thinking_options("high");
    let setup = ensure_low_thinking(&home, &mut wire, "session", &options, &mut metrics);
    let fake_server = async {
        use tokio::io::AsyncBufReadExt;
        let (reader, mut writer) = tokio::io::split(server);
        let mut reader = tokio::io::BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let request: Value = serde_json::from_str(&line)?;
        ensure!(request["id"] == 5 && request["method"] == "session/set_config_option");
        ensure!(
            request["params"] == json!({"sessionId":"session","configId":"thinking","value":"low"})
        );
        if append_proof {
            let mut file = fs::OpenOptions::new().append(true).open(&journal)?;
            std::io::Write::write_all(
                &mut file,
                format!(
                    "{}\n",
                    json!({"type":"config.update","agentId":"main","thinkingEffort":"low"})
                )
                .as_bytes(),
            )?;
        }
        writer.write_all(format!("{}\n",json!({"jsonrpc":"2.0","id":5,"result":{"configOptions":thinking_options("low")}})).as_bytes()).await?;
        Ok::<_, anyhow::Error>(())
    };
    let (result, server_result) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(setup, fake_server)
    })
    .await
    .map_err(|_| anyhow!("synthetic configuration exchange did not finish"))?;
    server_result?;
    if append_proof {
        ensure!(result? == 1);
        ensure!(metrics["profile_verifications"] == 2 && metrics["effective_thinking"] == "low");
    } else {
        ensure!(result.is_err() && metrics["effective_thinking"].is_null());
    }
    ensure!(metrics["thinking_config_rpc_count"] == 1);
    Ok(())
}

#[tokio::test]
async fn non_low_session_changes_effort_and_verifies_the_new_durable_record() -> Result<()> {
    exercise_non_low_configuration(true).await
}

#[tokio::test]
async fn low_configuration_receipt_without_durable_change_is_rejected() -> Result<()> {
    exercise_non_low_configuration(false).await
}

#[tokio::test]
async fn wire_failures_keep_only_typed_codes_and_numeric_remote_code() -> Result<()> {
    let cases = [
        (vec![], "acp_unexpected_eof", None),
        (b"PRIVATE_INVALID_JSON\n".to_vec(), "acp_invalid_json", None),
        (
            wire_bytes(&[
                json!({"jsonrpc":"2.0","id":9,"error":{"code":-42,"message":"PRIVATE_REMOTE"}}),
            ]),
            "acp_response_id_mismatch",
            None,
        ),
        (
            wire_bytes(&[
                json!({"jsonrpc":"2.0","id":3,"error":{"code":-32001,"message":"PRIVATE_REMOTE","data":{"nested":"PRIVATE_DATA"}}}),
            ]),
            "acp_remote_rpc_error",
            Some(-32001),
        ),
        (
            wire_bytes(&[
                json!({"jsonrpc":"2.0","id":3,"error":{"code":"PRIVATE_CODE","message":"PRIVATE_REMOTE"}}),
            ]),
            "acp_remote_rpc_error",
            None,
        ),
    ];
    for (bytes, code, remote) in cases {
        let mut wire = Wire::new(bytes.as_slice(), tokio::io::sink());
        let error = wire
            .next(3)
            .await
            .err()
            .ok_or_else(|| anyhow!("invalid wire accepted"))?;
        ensure!(!error.to_string().contains("PRIVATE"));
        ensure!(!error.to_string().contains("acp_"));
        // Even nested, untrusted diagnostic context must not be serialized.
        let result: Result<()> = Err(error.context("PRIVATE_CONTEXT").context("PRIVATE_OUTER"));
        let mut metrics = initial_metrics(&json!({"request_id":"typed-wire"}), 4);
        phase(&mut metrics, Phase::PromptResponse);
        record_operation(&mut metrics, &result);
        ensure!(metrics["failure_code"] == code && metrics["remote_rpc_code"] == json!(remote));
        ensure!(metrics["failure_phase"] == "prompt_response");
        ensure!(!metrics.to_string().contains("PRIVATE"));
    }
    Ok(())
}

fn workflow_setup() -> Result<(tempfile::TempDir, AcpHome, Value, Vec<Value>)> {
    let (temporary, mut home, _) = low_setup_fixture("low")?;
    home.model_metadata = json!({"configured_alias":"fixture-model"});
    let request = json!({"request_id":"flow","context":{"evidence":[]}});
    let mut options = thinking_options("low").as_array().unwrap().clone();
    options.push(json!({"id":"model","currentValue":"fixture-model"}));
    let packets = vec![
        json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"version":VERSION}}}),
        json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"session","configOptions":options}}),
    ];
    Ok((temporary, home, request, packets))
}

#[tokio::test]
async fn workflow_distinguishes_prompt_response_eof_rpc_and_segment_contract_failure() -> Result<()>
{
    for (packet, code, expected_phase) in [
        (None, "acp_unexpected_eof", "prompt_response"),
        (
            Some(
                json!({"jsonrpc":"2.0","id":3,"error":{"code":-32000,"message":"PRIVATE_REMOTE","data":"PRIVATE_BODY"}}),
            ),
            "acp_remote_rpc_error",
            "prompt_response",
        ),
        (
            Some(update(
                "session",
                "agent_message_chunk",
                "{ PRIVATE_INVALID_JSON\n",
            )),
            "invalid_json",
            "segment_validation",
        ),
    ] {
        let (_temporary, home, request, mut packets) = workflow_setup()?;
        packets.extend(packet);
        let bytes = wire_bytes(&packets);
        let mut wire = Wire::new(bytes.as_slice(), tokio::io::sink());
        let (sender, _receiver) = mpsc::channel(2);
        let mut metrics = initial_metrics(&request, 7);
        let result = workflow(
            &home,
            &mut wire,
            &request,
            7,
            "PRIVATE_PROMPT",
            &sender,
            &mut metrics,
        )
        .await;
        ensure!(result.is_err());
        record_operation(&mut metrics, &result);
        ensure!(
            metrics["prompt_write_attempted"] == true && metrics["prompt_write_completed"] == true
        );
        ensure!(metrics["session_setup_ms"].is_number());
        ensure!(metrics["failure_code"] == code && metrics["failure_phase"] == expected_phase);
        ensure!(!metrics.to_string().contains("PRIVATE"));
    }
    Ok(())
}

struct FailThirdWrite(usize);
impl tokio::io::AsyncWrite for FailThirdWrite {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.0 += 1;
        std::task::Poll::Ready(if self.0 == 3 {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "PRIVATE_IO_PATH",
            ))
        } else {
            Ok(bytes.len())
        })
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn prompt_write_attempt_does_not_claim_completion_when_pipe_write_fails() -> Result<()> {
    let (_temporary, home, request, packets) = workflow_setup()?;
    let bytes = wire_bytes(&packets);
    let mut wire = Wire::new(bytes.as_slice(), FailThirdWrite(0));
    let (sender, _receiver) = mpsc::channel(2);
    let mut metrics = initial_metrics(&request, 8);
    let result = workflow(
        &home,
        &mut wire,
        &request,
        8,
        "PRIVATE_PROMPT",
        &sender,
        &mut metrics,
    )
    .await;
    record_operation(&mut metrics, &result);
    ensure!(
        metrics["failure_code"] == "acp_write_failed" && metrics["failure_phase"] == "prompt_write"
    );
    ensure!(
        metrics["prompt_write_attempted"] == true && metrics["prompt_write_completed"] == false
    );
    ensure!(!metrics.to_string().contains("PRIVATE"));
    Ok(())
}

#[tokio::test]
async fn complete_local_workflow_keeps_receipts_and_separate_cleanup_failure() -> Result<()> {
    let (_temporary, home, request, mut packets) = workflow_setup()?;
    let text = "中文。 Next.";
    let segment = json!({"schema_version":"1.0","type":"segment","request_id":"flow","sequence":0,"text":text,"used_evidence_ids":[]});
    let final_record = json!({"schema_version":"1.0","type":"final","request_id":"flow","segment_count":1,
        "reply":{"schema_version":"1.0","request_id":"flow","reply_text":text,"teaching_move":"answer","task_update_candidates":[],"memory_candidates":[],"used_evidence_ids":[],"needs_clarification":false}});
    packets.extend([
        update("session", "agent_thought_chunk", "PRIVATE_THOUGHT"),
        update(
            "session",
            "agent_message_chunk",
            &format!("{segment}\n{final_record}\n"),
        ),
        json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}),
        json!({"jsonrpc":"2.0","id":4,"result":{}}),
    ]);
    let bytes = wire_bytes(&packets);
    let mut wire = Wire::new(bytes.as_slice(), tokio::io::sink());
    let (sender, mut receiver) = mpsc::channel(2);
    let mut metrics = initial_metrics(&request, 9);
    let result = workflow(
        &home,
        &mut wire,
        &request,
        9,
        "PRIVATE_PROMPT",
        &sender,
        &mut metrics,
    )
    .await;
    record_operation(&mut metrics, &result);
    ensure!(result?.reply()["reply_text"] == text && receiver.try_recv()?.text() == text);
    ensure!(metrics["operation_success"] == true && metrics["failure_code"].is_null());
    ensure!(metrics["prompt_write_completed"] == true && metrics["phase"] == "session_close");
    ensure!(wire.message_chunks == 1 && wire.thought_chunks_discarded == 1);
    // Cleanup failure is reported separately from the validated operation.
    ensure!(!record_cleanup(&mut metrics, false, true, true));
    ensure!(metrics["operation_success"] == true && metrics["operation_failure_code"].is_null());
    ensure!(
        metrics["failure_code"] == "acp_cleanup_failed" && metrics["failure_phase"] == "cleanup"
    );
    ensure!(!metrics.to_string().contains("PRIVATE"));
    Ok(())
}

#[tokio::test]
async fn production_workflow_wait_classifies_cancel_deadline_and_stderr_failure() -> Result<()> {
    for reason in ["cancel", "deadline", "stderr"] {
        let (cancel, mut cancellation) = watch::channel(false);
        let (sender, _receiver) = mpsc::channel(2);
        let mut stderr = if reason == "stderr" {
            tokio::spawn(async { Err(anyhow!("PRIVATE_STDERR")) })
        } else {
            tokio::spawn(std::future::pending::<Result<usize>>())
        };
        if reason == "cancel" {
            cancel.send(true)?;
        }
        let (result, done, ok, _) = tokio::time::timeout(
            Duration::from_secs(2),
            await_workflow(
                std::future::pending(),
                &mut cancellation,
                &sender,
                &mut stderr,
                if reason == "deadline" {
                    Duration::ZERO
                } else {
                    Duration::from_secs(30)
                },
            ),
        )
        .await?;
        stderr.abort();
        let mut metrics = initial_metrics(&json!({"request_id":"wait"}), 10);
        phase(&mut metrics, Phase::PromptResponse);
        record_operation(&mut metrics, &result);
        let expected = match reason {
            "cancel" => "request_cancelled",
            "deadline" => "acp_deadline_exceeded",
            _ => "acp_stderr_failed_or_limited",
        };
        ensure!(metrics["failure_code"] == expected);
        ensure!(!metrics.to_string().contains("PRIVATE"));
        if reason == "stderr" {
            ensure!(done && !ok);
        }
    }
    Ok(())
}

#[test]
fn cleanup_does_not_overwrite_operation_failure_or_copy_unknown_error_context() -> Result<()> {
    let result: Result<()> = Err(anyhow!("PRIVATE_UNCLASSIFIED_PATH").context("PRIVATE_OUTER"));
    let mut metrics = initial_metrics(&json!({"request_id":"fallback"}), 11);
    phase(&mut metrics, Phase::Spawn);
    record_operation(&mut metrics, &result);
    ensure!(!record_cleanup(&mut metrics, true, false, true));
    ensure!(
        metrics["failure_code"] == "acp_operation_failed" && metrics["failure_phase"] == "spawn"
    );
    ensure!(metrics["cleanup_failure_code"] == "acp_cleanup_failed");
    ensure!(!metrics.to_string().contains("PRIVATE"));
    Ok(())
}

#[tokio::test]
async fn early_rejected_call_replaces_previous_metrics_with_its_own_binding() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let provider = KimiProvider::new(temporary.path())?;
    provider.state.lock().unwrap().metrics["last_acp_segments"] =
        json!({"request_id":"old","success":true,"citation_diagnostic":{"stale":true}});
    let request = json!({"request_id":"new","context":{"evidence":[]}});
    let (sender, _receiver) = mpsc::channel(2);
    ensure!(
        provider
            .generate_segments(&request, 12, sender)
            .await
            .is_err()
    );
    let metrics = provider.status()["metrics"]["last_acp_segments"].clone();
    ensure!(metrics["request_id"] == "new" && metrics["generation"] == 12);
    ensure!(metrics["success"] == false && metrics["failure_code"] == "acp_operation_failed");
    ensure!(metrics["prompt_write_attempted"] == false && metrics["process_reaped"].is_null());
    ensure!(metrics["citation_diagnostic"].is_null());
    Ok(())
}

#[tokio::test]
async fn workflow_keeps_segment_and_clean_eof_final_citation_causes_without_text() -> Result<()> {
    for final_failure in [false, true] {
        let (_temporary, home, request, mut packets) = workflow_setup()?;
        let mut segment = json!({"schema_version":"1.0","type":"segment","request_id":"flow","sequence":0,
            "text":"PRIVATE_SYNTHETIC_TEXT","used_evidence_ids":[]});
        let final_record = json!({"schema_version":"1.0","type":"final","request_id":"flow","segment_count":1,
            "reply":{"schema_version":"1.0","request_id":"flow","reply_text":"PRIVATE_SYNTHETIC_TEXT",
            "teaching_move":"answer","used_evidence_ids":["PRIVATE_ID"],"needs_clarification":false,
            "task_update_candidates":[],"memory_candidates":[]}});
        let text = if final_failure {
            format!("{segment}\n{final_record}")
        } else {
            segment["used_evidence_ids"] = json!(["PRIVATE_ID"]);
            format!("{segment}\n")
        };
        packets.extend([
            update("session", "agent_thought_chunk", "PRIVATE_THOUGHT"),
            update("session", "agent_message_chunk", &text),
            json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}),
        ]);
        let bytes = wire_bytes(&packets);
        let mut wire = Wire::new(bytes.as_slice(), tokio::io::sink());
        let (sender, mut receiver) = mpsc::channel(2);
        let mut metrics = initial_metrics(&request, 24);
        let result = workflow(
            &home,
            &mut wire,
            &request,
            24,
            "PRIVATE_PROMPT",
            &sender,
            &mut metrics,
        )
        .await;
        ensure!(result.is_err());
        record_operation(&mut metrics, &result);
        ensure!(
            metrics["failure_code"]
                == if final_failure {
                    "final_schema_invalid"
                } else {
                    "segment_evidence"
                }
        );
        ensure!(
            metrics["failure_phase"]
                == if final_failure {
                    "final_validation"
                } else {
                    "segment_validation"
                }
        );
        ensure!(
            metrics["citation_diagnostic"]["field"]
                == if final_failure {
                    "used_evidence_ids"
                } else {
                    "segment.used_evidence_ids"
                }
        );
        ensure!(metrics["citation_diagnostic"]["invalid_reference_count"] == 1);
        ensure!(receiver.try_recv().is_ok() == final_failure);
        ensure!(!metrics.to_string().contains("PRIVATE"));
        // Cleanup failure must not discard the original bounded evidence diagnosis.
        let diagnosis = metrics["citation_diagnostic"].clone();
        record_cleanup(&mut metrics, true, false, true);
        ensure!(
            metrics["citation_diagnostic"] == diagnosis
                && metrics["cleanup_failure_code"] == "acp_cleanup_failed"
        );
    }
    Ok(())
}

#[tokio::test]
async fn provider_scripted_transport_binds_citation_failure_then_clears_on_success() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let provider = KimiProvider::new(temporary.path())?;
    let mut transport = provider.scripted_segments_for_test();
    for generation in [31, 32] {
        let rid = format!("citation-{generation}");
        let request = json!({"request_id":rid,"learner_id":"synthetic","context":{"evidence":[]}});
        let segment = json!({"schema_version":"1.0","type":"segment","request_id":rid,"sequence":0,
            "text":"Synthetic.","used_evidence_ids":[]});
        let final_record = json!({"schema_version":"1.0","type":"final","request_id":rid,"segment_count":1,
            "reply":{"schema_version":"1.0","request_id":rid,"reply_text":"Synthetic.","teaching_move":"answer",
            "used_evidence_ids":if generation==31 {json!(["event_22222222222222222222222222222222"])} else {json!([])},
            "needs_clarification":false,"task_update_candidates":[],"memory_candidates":[]}});
        let (sender, mut output) = mpsc::channel(2);
        let (result, sent) = tokio::join!(
            provider.generate_segments(&request, generation, sender),
            async {
                let call = transport
                    .recv()
                    .await
                    .ok_or_else(|| anyhow!("scripted call absent"))?;
                ensure!(call.request["request_id"] == rid && call.generation == generation);
                call.response
                    .send(vec![segment, final_record])
                    .map_err(|_| anyhow!("scripted response receiver gone"))
            }
        );
        sent?;
        ensure!(result.is_ok() == (generation == 32));
        let metric = provider.status()["metrics"]["last_acp_segments"].clone();
        ensure!(metric["request_id"] == rid && metric["generation"] == generation);
        ensure!(metric["citation_diagnostic"].is_null() == (generation == 32));
        if generation == 31 {
            ensure!(metric["failure_code"] == "final_schema_invalid");
        }
        ensure!(provider.calls.lock().unwrap().is_empty());
        ensure!(output.try_recv().is_ok()); // A validated segment is not a display or playback receipt.
    }
    Ok(())
}
