//! Fake transports and temporary policy files only; no DPAPI/network/device access.
use super::*;
use std::sync::Arc;

fn request(id: &str) -> Value {
    json!({"request_id":id,"learner_id":"synthetic","generation":1,"context":{"evidence":[]}})
}
fn records(request: &Value) -> Vec<Value> {
    vec![
        json!({"schema_version":"1.0","type":"segment","request_id":request["request_id"],
            "sequence":0,"text":"Observe one variable.","used_evidence_ids":[]}),
        json!({"schema_version":"1.0","type":"final_metadata","request_id":request["request_id"],"segment_count":1,
            "reply":{"schema_version":"1.0","request_id":request["request_id"],
            "teaching_move":"give_hint","task_update_candidates":[],"memory_candidates":[],"used_evidence_ids":[],"needs_clarification":false}}),
    ]
}
fn fake(root: &Path) -> Result<(Arc<KimiProvider>, mpsc::Receiver<ScriptedSegmentRequest>)> {
    let mut provider = KimiProvider::new(root)?;
    let transport = provider.scripted_segments_for_test();
    provider.select_scripted_flash_for_test();
    Ok((Arc::new(provider), transport))
}

#[test]
fn policy_is_bounded_explicit_and_does_not_silently_fall_back() -> Result<()> {
    let dir = tempfile::tempdir()?;
    assert!(ForegroundRuntime::load(dir.path()).is_kimi());
    assert_eq!(
        parse_policy(br#"{"protocol_version":1,"provider":"kimi"}"#)?,
        Selection::Kimi
    );
    for (mode, expected) in [
        ("non_thinking", Selection::FlashOff),
        ("thinking_low", Selection::FlashLow),
    ] {
        assert_eq!(
            parse_policy(&serde_json::to_vec(
                &json!({"protocol_version":1,"provider":"deepseek_flash","thinking":mode})
            )?)?,
            expected
        );
    }
    for value in [
        json!({"protocol_version":1,"provider":"deepseek_flash"}),
        json!({"protocol_version":1,"provider":"deepseek_flash","thinking":"high"}),
        json!({"protocol_version":1,"provider":"kimi","thinking":"non_thinking"}),
        json!({"protocol_version":2,"provider":"kimi"}),
        json!({"protocol_version":1,"provider":"kimi","api_key":"PRIVATE_CANARY"}),
    ] {
        assert!(parse_policy(&serde_json::to_vec(&value)?).is_err());
    }
    fs::create_dir(dir.path().join("runtime"))?;
    fs::write(dir.path().join(POLICY_PATH), b"not json PRIVATE_CANARY")?;
    let provider = KimiProvider::new_for_workstation(dir.path().into())?;
    assert_eq!(provider.foreground_status()["provider"], "invalid");
    assert_eq!(provider.foreground_status()["available"], false);
    assert!(
        !provider
            .foreground_status()
            .to_string()
            .contains("PRIVATE_CANARY")
    );
    fs::write(
        dir.path().join(POLICY_PATH),
        br#"{"protocol_version":1,"provider":"kimi"}"#,
    )?;
    assert_eq!(provider.foreground_status()["provider"], "invalid"); // Startup fixed.
    assert!(KimiProvider::new(dir.path())?.foreground.is_kimi()); // Explicit Kimi APIs unchanged.
    fs::write(dir.path().join(POLICY_PATH), vec![b' '; 2049])?;
    assert_eq!(
        ForegroundRuntime::load(dir.path()).selected,
        Selection::Invalid
    );
    Ok(())
}

#[tokio::test]
async fn invalid_selection_blocks_foreground_before_any_registration() -> Result<()> {
    let dir = tempfile::tempdir()?;
    fs::create_dir(dir.path().join("runtime"))?;
    fs::write(dir.path().join(POLICY_PATH), b"{}")?;
    let provider = KimiProvider::new_for_workstation(dir.path().into())?;
    let (send, _receive) = mpsc::channel(2);
    assert!(
        provider
            .generate_segments(&request("invalid"), 1, send)
            .await
            .is_err()
    );
    assert!(provider.calls.lock().unwrap().is_empty());
    assert_eq!(provider.status()["metrics"]["requests"], 0);
    Ok(())
}

#[tokio::test]
async fn selected_flash_preempts_background_without_a_second_gate() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (provider, mut transport) = fake(dir.path())?;
    assert_eq!(provider.status()["available"], false);
    assert_eq!(provider.foreground_status()["available"], true);
    let (background, permit, mut cancelled) =
        provider.register_call("background", "synthetic", true)?;
    let (send, mut receive) = mpsc::channel(2);
    let task = tokio::spawn({
        let provider = provider.clone();
        async move {
            provider
                .generate_segments(&request("foreground"), 1, send)
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(1), cancelled.changed()).await??;
    assert!(*cancelled.borrow());
    assert!(transport.try_recv().is_err()); // Gate still belongs to the old background.
    assert!(
        provider
            .register_call("overtaking", "synthetic", true)
            .is_err()
    );
    drop(permit);
    drop(background);
    let call = tokio::time::timeout(Duration::from_secs(1), transport.recv())
        .await?
        .unwrap();
    assert_eq!(provider.calls.lock().unwrap().len(), 1);
    call.response
        .send(records(&call.request))
        .map_err(|_| anyhow!("fake response lost"))?;
    tokio::time::timeout(Duration::from_secs(1), task).await???;
    assert!(receive.recv().await.is_some());
    assert!(provider.calls.lock().unwrap().is_empty());
    assert_eq!(provider.foreground_status()["running"], 0);
    Ok(())
}

#[tokio::test]
async fn whole_reply_consumes_the_selected_stream_with_one_registration() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (provider, mut transport) = fake(dir.path())?;
    let req = request("whole");
    let (result, sent) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(provider.generate(&req), async {
            let call = transport
                .recv()
                .await
                .ok_or_else(|| anyhow!("fake call missing"))?;
            assert_eq!(provider.calls.lock().unwrap().len(), 1);
            call.response
                .send(records(&call.request))
                .map_err(|_| anyhow!("fake response lost"))
        })
    })
    .await?;
    sent?;
    assert_eq!(result?["reply_text"], "Observe one variable.");
    assert!(transport.try_recv().is_err());
    assert!(provider.calls.lock().unwrap().is_empty());
    assert_eq!(provider.status()["metrics"]["requests"], 1);
    Ok(())
}

#[tokio::test]
async fn close_cancels_the_exact_foreground_and_rejects_unretired_calls() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (provider, mut transport) = fake(dir.path())?;
    let (send, _receive) = mpsc::channel(2);
    let task = tokio::spawn({
        let provider = provider.clone();
        async move { provider.generate_segments(&request("close"), 1, send).await }
    });
    let call = tokio::time::timeout(Duration::from_secs(1), transport.recv())
        .await?
        .unwrap();
    provider.close().await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await??
            .is_err()
    );
    assert!(call.response.is_closed());
    assert!(provider.calls.lock().unwrap().is_empty());
    let (held, _permit, cancelled) = provider.register_call("unretired", "synthetic", false)?;
    assert!(provider.close().await.is_err());
    assert!(*cancelled.borrow());
    assert!(provider.calls.lock().unwrap().contains_key("unretired"));
    assert!(provider.cleanup_owned("synthetic").await.is_err());
    drop(held);
    provider.close().await?;
    Ok(())
}
