use super::*;

fn request(sequence: u64, body: Value) -> String {
    let mut value = json!({"version":1,"sequence":sequence,"id":format!("r{sequence}")});
    for (key, value_part) in body.as_object().unwrap() {
        value[key] = value_part.clone();
    }
    value.to_string()
}

#[tokio::test]
async fn model_free_session_scopes_replays_and_shutdown() {
    let root = tempfile::tempdir().unwrap();
    let (input, receive) = mpsc::channel(16);
    let (output, events) = std::sync::mpsc::sync_channel(64);
    for (sequence, body) in [
        (1, json!({"op":"status"})),
        (2, json!({"op":"bind","scope":{"owner":"a","generation":1}})),
        (
            2,
            json!({"op":"bind","scope":{"owner":"replayed","generation":2}}),
        ),
        (3, json!({"op":"bind","scope":{"owner":"b","generation":2}})),
        (
            4,
            json!({"op":"speak","scope":{"owner":"a","generation":1},"text":"Hello.","language":"en"}),
        ),
        (5, json!({"op":"bind","scope":{"owner":"a","generation":1}})),
        (6, json!({"op":"status"})),
        (7, json!({"op":"shutdown"})),
    ] {
        input.send(Ok(request(sequence, body))).await.unwrap();
    }
    // Keep input open: explicit shutdown must not wait for EOF.
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        run(root.path().into(), receive, output),
    )
    .await
    .unwrap()
    .unwrap();
    let events: Vec<_> = events.try_iter().collect();
    assert_eq!(events[0]["event"], "hello");
    assert_eq!(events[0]["data"]["asr"]["max_duration_seconds"], 20);
    assert_eq!(events[1]["data"]["backend"]["phase"], "idle");
    assert_eq!(events[3]["data"]["error"], "voice_request_sequence");
    assert_eq!(events[5]["data"]["error"], "voice_scope_stale");
    assert_eq!(events[6]["data"]["error"], "voice_scope_generation");
    assert_eq!(events[7]["data"]["scope"]["owner"], "b");
    assert_eq!(events.last().unwrap()["data"]["cleanup_confirmed"], true);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn eof_before_queued_prepare_starts_opens_no_services() {
    let root = tempfile::tempdir().unwrap();
    let (input, receive) = mpsc::channel(4);
    let (output, events) = std::sync::mpsc::sync_channel(64);
    input
        .send(Ok(request(1, json!({"op":"prepare"}))))
        .await
        .unwrap();
    drop(input);
    run(root.path().into(), receive, output).await.unwrap();
    let events: Vec<_> = events.try_iter().collect();
    assert_eq!(events[1]["event"], "accepted");
    let cleanup = &events.last().unwrap()["data"]["cleanup"];
    assert_eq!(cleanup["tts"]["process_started"], false);
    assert_eq!(cleanup["asr"]["process_started"], false);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn failed_output_and_malformed_framing_exit_cleanly() {
    let root = tempfile::tempdir().unwrap();
    let (_input, receive) = mpsc::channel(4);
    let (output, events) = std::sync::mpsc::sync_channel(1);
    drop(events);
    assert!(run(root.path().into(), receive, output).await.is_err());
    let (input, receive) = mpsc::channel(4);
    let (output, events) = std::sync::mpsc::sync_channel(8);
    input
        .send(Err(anyhow!("voice_input_frame_limit")))
        .await
        .unwrap();
    assert!(run(root.path().into(), receive, output).await.is_err());
    assert_eq!(events.try_iter().count(), 1);
}

#[tokio::test]
async fn prepare_requires_explicit_local_policies() {
    let root = tempfile::tempdir().unwrap();
    let backend = VoiceBackend::new(root.path().into()).unwrap();
    assert!(backend.prepare().await.is_err());
    assert_eq!(backend.status()["ready"], false);
    assert_eq!(backend.shutdown().await["cleanup_confirmed"], true);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
