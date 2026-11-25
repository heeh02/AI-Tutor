use super::*;

fn client(id: char) -> Arc<AsrClient> {
    Arc::new(
        AsrClient::new(
            id.to_string().repeat(32),
            "http://127.0.0.1:49152".into(),
            "synthetic-token-000000000000".into(),
        )
        .unwrap(),
    )
}
fn running(runtime: &LocalAsrRuntime, epoch: u64) {
    let mut control = runtime.control.lock().unwrap();
    control.epoch = epoch;
    control.running_epoch = Some(epoch);
    control.stopping = false;
    control.status = json!({"mode":"managed","phase":"warming","ready":true,
        "warmup_completed":false,"active":false});
}
#[test]
fn cancelled_preparation_cannot_install_after_stop_returns() {
    let runtime = LocalAsrRuntime::default();
    let (audio, _owner) = AudioService::configuration_test_channel(0);
    running(&runtime, 1);
    runtime.preempt(&audio);
    assert!(!runtime.install(1, &audio, client('a')));
    assert!(audio.streaming_asr_snapshot().is_none());
    assert_eq!(runtime.status()["active"], false);
}
#[test]
fn capture_snapshot_and_old_cleanup_cannot_replace_new_instance() {
    let runtime = LocalAsrRuntime::default();
    let (audio, _owner) = AudioService::configuration_test_channel(0);
    running(&runtime, 1);
    let first = client('a');
    assert!(runtime.install(1, &audio, first.clone()));
    let capture = audio.streaming_asr_snapshot().unwrap();
    runtime.preempt(&audio);
    runtime.finish(1, &audio, Some(json!({"cleanup_confirmed":true})));
    running(&runtime, 3);
    let second = client('b');
    assert!(runtime.install(3, &audio, second.clone()));
    assert!(!audio.clear_streaming_asr(&first.instance_id));
    runtime.finish(1, &audio, Some(json!({"cleanup_confirmed":true})));
    first.mark_unavailable();
    assert_eq!(capture.instance_id, first.instance_id);
    assert_eq!(
        audio.streaming_asr_snapshot().unwrap().instance_id,
        second.instance_id
    );
    assert_eq!(runtime.status()["active"], true);
}
#[test]
fn asr_stop_detaches_future_client_without_invalidating_active_recording() {
    let runtime = LocalAsrRuntime::default();
    let (audio, _owner) = AudioService::configuration_test_channel(0);
    running(&runtime, 1);
    assert!(runtime.install(1, &audio, client('a')));
    let generation = audio.status()["recording_generation"].clone();
    runtime.preempt(&audio);
    assert!(audio.streaming_asr_snapshot().is_none());
    assert_eq!(audio.status()["recording_generation"], generation);
    assert_eq!(runtime.status()["phase"], "stopping");
}
#[test]
fn failed_cleanup_disallows_retry_while_confirmed_service_failure_allows_it() {
    let runtime = LocalAsrRuntime::default();
    let (audio, _owner) = AudioService::configuration_test_channel(0);
    running(&runtime, 1);
    runtime.finish(1, &audio, Some(json!({"cleanup_confirmed":false})));
    assert_eq!(runtime.status()["phase"], "cleanup_failed");
    assert_eq!(runtime.status()["retry_available"], false);
    running(&runtime, 2);
    runtime.finish(2, &audio, Some(json!({"cleanup_confirmed":true})));
    assert_eq!(runtime.status()["phase"], "degraded");
    assert_eq!(runtime.status()["retry_available"], true);
    assert_eq!(runtime.status()["busy"], false);
}
#[tokio::test]
async fn dropped_stop_waiter_does_not_leave_finished_lifecycle_busy() {
    let runtime = Arc::new(LocalAsrRuntime::default());
    let (audio, _owner) = AudioService::configuration_test_channel(0);
    let audio = Arc::new(audio);
    running(&runtime, 1);
    let (release, receive) = tokio::sync::oneshot::channel();
    *runtime.task.lock().unwrap() = Some((
        1,
        tokio::spawn({
            let runtime = runtime.clone();
            let audio = audio.clone();
            async move {
                let _ = receive.await;
                runtime.finish(1, &audio, Some(json!({"cleanup_confirmed":true})));
            }
        }),
    ));
    let waiting = tokio::spawn({
        let runtime = runtime.clone();
        let audio = audio.clone();
        async move {
            runtime.preempt(&audio);
            runtime.drain(&audio).await;
        }
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while runtime.task.lock().unwrap().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    waiting.abort();
    let _ = waiting.await;
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while runtime.status()["busy"] == true {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(runtime.status()["phase"], "stopped");
    assert_eq!(runtime.status()["retry_available"], true);
}
#[tokio::test]
async fn default_workstation_never_loads_external_descriptor_or_starts_asr() -> Result<()> {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join("runtime"))?;
    std::fs::write(
        dir.path().join("runtime/speech-service.json"),
        "external fixture left untouched",
    )?;
    let app = Workstation::new(dir.path().into(), "synthetic".into())?;
    app.start_local_asr_runtime();
    assert_eq!(app.local_asr_status()["phase"], "disabled");
    assert_eq!(app.local_asr_status()["busy"], false);
    assert!(app.local_asr.task.lock().unwrap().is_none());
    assert_eq!(app.audio.status()["asr_ready"], false);
    app.close().await;
    assert_eq!(
        std::fs::read_to_string(dir.path().join("runtime/speech-service.json"))?,
        "external fixture left untouched"
    );
    Ok(())
}
