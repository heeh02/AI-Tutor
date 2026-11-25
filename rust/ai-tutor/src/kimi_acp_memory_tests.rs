use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

fn request() -> Value {
    json!({"request_id":"memory-test-request","learner_id":"synthetic-learner",
        "session_id":"synthetic-session","prompt_version":memory_contract::PROMPT_VERSION,
        "_memory_job_id":"PRIVATE_JOB_ID","_memory_attempt":2,"context":{"sources":[]}})
}

fn update(kind: &str, text: &str) -> Value {
    json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session",
        "update":{"sessionUpdate":kind,"content":{"type":"text","text":text}}}})
}

fn bytes(packets: &[Value]) -> Vec<u8> {
    packets
        .iter()
        .map(|v| format!("{v}\n"))
        .collect::<String>()
        .into_bytes()
}

fn pending_metrics() -> Value {
    let mut metrics = initial_metrics(&request());
    metrics["prompt_write_completed"] = json!(true);
    metrics["prompt_pending"] = json!(true);
    metrics
}

#[tokio::test]
async fn nonempty_thought_observation_is_separate_from_local_messages_and_metadata() -> Result<()> {
    let data = bytes(&[
        update("agent_thought_chunk", ""),
        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session",
            "update":{"sessionUpdate":"usage_update","used":1}}}),
        update("agent_message_chunk", "PRIVATE_LOCAL_NOTICE"),
        update("agent_thought_chunk", "PRIVATE_THOUGHT_NEVER_RETAIN"),
    ]);
    let mut wire = Wire::new(data.as_slice(), tokio::io::sink());
    wire.session = Some("session".into());
    let mut metrics = pending_metrics();
    assert!(matches!(wire.next(3).await?, Packet::Ignored));
    assert_eq!(wire.nonempty_thought_chunks, 0);
    assert!(matches!(wire.next(3).await?, Packet::Ignored));
    assert_eq!(metrics["agent_output_observed"], false);
    assert!(matches!(wire.next(3).await?, Packet::Text(_)));
    observe_output(&mut metrics, false, true, 4.0);
    assert_eq!(metrics["agent_output_observed"], true);
    assert_eq!(metrics["nonempty_thought_event_observed"], false);
    assert!(matches!(wire.next(3).await?, Packet::Ignored));
    assert_eq!(wire.thought_chunks_discarded, 2);
    assert_eq!(wire.nonempty_thought_chunks, 1);
    observe_output(&mut metrics, true, false, 8.0);
    assert_eq!(metrics["first_nonempty_thought_event_ms"], 8.0);
    assert!(!metrics.to_string().contains("PRIVATE_THOUGHT"));
    assert!(!metrics.to_string().contains("PRIVATE_LOCAL_NOTICE"));
    assert_eq!(metrics["remote_compute_stop_confirmed"], false);
    Ok(())
}

#[test]
fn complete_memory_object_is_required_and_private_job_metadata_stays_out_of_prompt() -> Result<()> {
    let valid = json!({"schema_version":memory_contract::SCHEMA_VERSION,"candidates":[]});
    assert_eq!(structured_reply(&valid.to_string())?, valid);
    for invalid in [
        "",
        "{\"schema_version\":",
        "{}",
        "[]",
        "{}{}",
        "{\"schema_version\":\"wrong\",\"candidates\":[]}",
        "{\"schema_version\":\"educational-memory-v2\",\"candidates\":[],\"PRIVATE_EXTRA\":1}",
    ] {
        let error = structured_reply(invalid).unwrap_err();
        assert!(!error.to_string().contains("PRIVATE_EXTRA"));
    }
    let req = request();
    let metrics = initial_metrics(&req);
    assert_eq!(metrics["job_id"], "PRIVATE_JOB_ID");
    assert_eq!(metrics["attempt_no"], 2);
    let prompt = build_prompt(memory_contract::POLICY, &memory_contract::schema(), &req)?;
    assert!(!prompt.contains("PRIVATE_JOB_ID") && !prompt.contains("_memory_attempt"));
    let without = initial_metrics(&json!({"request_id":"other"}));
    assert!(without["job_id"].is_null() && without["attempt_no"].is_null());
    Ok(())
}

#[tokio::test]
async fn cancelled_terminal_is_observed_before_any_close_and_late_chunks_cannot_become_a_reply()
-> Result<()> {
    let (client, server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(client);
    let mut wire = Wire::new(reader, writer);
    wire.session = Some("session".into());
    let mut metrics = pending_metrics();
    let observe = cancel_and_observe(&mut wire, &mut metrics, Duration::from_secs(1));
    let agent = async {
        let (reader, mut writer) = tokio::io::split(server);
        let mut reader = tokio::io::BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let sent: Value = serde_json::from_str(&line)?;
        assert_eq!(sent["method"], "session/cancel");
        assert!(sent.get("id").is_none());
        assert_eq!(sent["params"]["sessionId"], "session");
        writer
            .write_all(&bytes(&[
                update("agent_message_chunk", "PRIVATE_LATE_REPLY"),
                update("agent_thought_chunk", "PRIVATE_LATE_THOUGHT"),
                json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"cancelled"}}),
            ]))
            .await?;
        Ok::<(), anyhow::Error>(())
    };
    let ((), agent_result) = tokio::join!(observe, agent);
    agent_result?;
    assert_eq!(metrics["agent_cancel_terminal_observed"], true);
    assert_eq!(
        metrics["cancel_observation_code"],
        "matching_prompt_cancelled_before_close"
    );
    assert_eq!(metrics["agent_output_observed"], false);
    assert_eq!(metrics["nonempty_thought_event_observed"], false);
    assert!(!metrics.to_string().contains("PRIVATE_LATE"));
    assert_eq!(metrics["remote_compute_stop_confirmed"], false);
    Ok(())
}

#[tokio::test]
async fn end_turn_wrong_id_eof_and_timeout_do_not_claim_cancel_confirmation() -> Result<()> {
    for packets in [
        vec![json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}})],
        vec![json!({"jsonrpc":"2.0","id":4,"result":{"stopReason":"cancelled"}})],
        vec![json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"PRIVATE_UNKNOWN_REASON"}})],
        vec![],
    ] {
        let data = bytes(&packets);
        let mut wire = Wire::new(data.as_slice(), tokio::io::sink());
        wire.session = Some("session".into());
        let mut metrics = pending_metrics();
        cancel_and_observe(&mut wire, &mut metrics, Duration::from_millis(50)).await;
        assert_eq!(metrics["agent_cancel_terminal_observed"], false);
        assert!(!metrics.to_string().contains("PRIVATE_UNKNOWN_REASON"));
    }
    let (reader, _unresponsive_peer) = tokio::io::duplex(1024);
    let mut wire = Wire::new(reader, tokio::io::sink());
    wire.session = Some("session".into());
    let mut metrics = pending_metrics();
    cancel_and_observe(&mut wire, &mut metrics, Duration::from_millis(10)).await;
    assert_eq!(metrics["cancel_observation_code"], "cancel_receipt_timeout");
    assert_eq!(metrics["agent_cancel_terminal_observed"], false);
    Ok(())
}

#[tokio::test]
async fn duplicate_request_rejection_and_dropped_progress_preserve_other_observations() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let provider = KimiProvider::new(directory.path())?;
    let metrics = initial_metrics(&request());
    provider.state.lock().unwrap().metrics["active_acp_memory"] = metrics.clone();
    provider.state.lock().unwrap().ready = true;
    let (owner, permit, _) =
        provider.register_call("memory-test-request", "synthetic-learner", true)?;
    let (started, started_receipt) = watch::channel(false);
    assert!(provider.consolidate_acp(&request(), started).await.is_err());
    assert!(!*started_receipt.borrow());
    assert_eq!(provider.status()["metrics"]["active_acp_memory"], metrics);
    drop(permit);
    drop(owner);
    let rejected_duplicate = initial_metrics(&request());
    assert_ne!(
        metrics["observation_id"],
        rejected_duplicate["observation_id"]
    );
    {
        let _unowned_scope = ProgressScope {
            provider: &provider,
            observation_id: s(&rejected_duplicate, "observation_id").to_owned(),
            finished: false,
        };
    }
    assert_eq!(provider.status()["metrics"]["active_acp_memory"], metrics);
    {
        let _scope = ProgressScope {
            provider: &provider,
            observation_id: s(&metrics, "observation_id").to_owned(),
            finished: false,
        };
    }
    let state = provider.status();
    assert!(state["metrics"]["active_acp_memory"].is_null());
    assert_eq!(
        state["metrics"]["last_acp_memory"]["failure_code"],
        "acp_memory_future_dropped"
    );
    assert!(state["metrics"]["last_acp_memory"]["process_reaped"].is_null());
    assert!(state["metrics"]["last_acp_memory"]["cleanup_confirmed"].is_null());
    let reused_request_id = initial_metrics(&request());
    provider.state.lock().unwrap().metrics["active_acp_memory"] = reused_request_id.clone();
    {
        let _scope = ProgressScope {
            provider: &provider,
            observation_id: s(&metrics, "observation_id").to_owned(),
            finished: false,
        };
    }
    assert_eq!(
        provider.status()["metrics"]["active_acp_memory"],
        reused_request_id
    );
    Ok(())
}

#[tokio::test]
async fn structured_memory_workflow_uses_verified_profile_and_exact_acp_text() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let provider = KimiProvider::new(directory.path())?;
    let path = provider.runtime.join("run-rust-acp-memory-test");
    let home_path = path.join("home");
    let journal = home_path.join("sessions/synthetic/session/agents/main/wire.jsonl");
    fs::create_dir_all(journal.parent().unwrap())?;
    fs::write(
        &journal,
        format!(
            "{}\n",
            json!({"type":"profile.bind","agentId":"main",
        "profileName":"agent","activeToolNames":[],"subagents":[],
        "systemPrompt":"SYNTHETIC_NO_TOOLS","thinkingEffort":"low"})
        ),
    )?;
    let home = AcpHome {
        run: OwnedRun {
            path: path.clone(),
            runtime: provider.runtime.clone(),
        },
        home: home_path,
        work: path.join("work"),
        expected_prompt: "SYNTHETIC_NO_TOOLS".into(),
        canary: None,
        model_metadata: json!({"configured_alias":"synthetic","configured_model_id":"synthetic"}),
    };
    let response =
        json!({"schema_version":memory_contract::SCHEMA_VERSION,"candidates":[]}).to_string();
    let (first, second) = response.split_at(response.len() / 2);
    let data = bytes(&[
        json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"version":VERSION}}}),
        json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"session","configOptions":[
            {"id":"model","currentValue":"synthetic"},
            {"id":"thinking","type":"select","currentValue":"low","options":[{"value":"low"}]}]}}),
        update("agent_thought_chunk", "PRIVATE_GENERATION_BODY"),
        update("agent_message_chunk", first),
        update("agent_message_chunk", second),
        json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}),
        json!({"jsonrpc":"2.0","id":4,"result":null}),
    ]);
    let (writer, mut emitted) = tokio::io::duplex(8192);
    let mut wire = Wire::new(data.as_slice(), writer);
    let mut metrics = initial_metrics(&request());
    provider.state.lock().unwrap().metrics["active_acp_memory"] = metrics.clone();
    let reply = workflow(
        &provider,
        &home,
        &mut wire,
        "SYNTHETIC_MEMORY_PROMPT",
        &mut metrics,
        Instant::now(),
    )
    .await?;
    assert_eq!(
        reply,
        json!({"schema_version":memory_contract::SCHEMA_VERSION,"candidates":[]})
    );
    assert_eq!(metrics["nonempty_thought_event_observed"], true);
    assert_eq!(metrics["agent_message_text_observed"], true);
    assert_eq!(metrics["prompt_pending"], false);
    assert_eq!(metrics["prompt_stop_reason"], "end_turn");
    assert_eq!(metrics["agent_cancel_terminal_observed"], false);
    assert!(
        !provider
            .status()
            .to_string()
            .contains("PRIVATE_GENERATION_BODY")
    );
    wire.writer.shutdown().await?;
    let mut written = vec![];
    emitted.read_to_end(&mut written).await?;
    let sent: Vec<Value> = String::from_utf8(written)?
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    assert_eq!(
        sent.iter().map(|v| s(v, "method")).collect::<Vec<_>>(),
        vec![
            "initialize",
            "session/new",
            "session/prompt",
            "session/close"
        ]
    );
    assert_eq!(sent[0]["params"]["clientCapabilities"], json!({}));
    assert_eq!(sent[1]["params"]["mcpServers"], json!([]));
    home.run.cleanup()?;
    Ok(())
}
