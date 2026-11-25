use super::*;

const INSTANCE: &str = "0123456789abcdef0123456789abcdef";
const ENDPOINT: &str = "http://127.0.0.1:57431";

fn bytes(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn preparing(state: &str) -> Value {
    match state {
        "starting" => json!({"protocol_version":1,"instance_id":INSTANCE,"state":state,
            "pid":2345,"process_group":2345,"ready":false,"warmup_completed":false}),
        "awaiting_owner" => json!({"protocol_version":1,"instance_id":INSTANCE,"state":state,
            "ready":false,"warmup_completed":false}),
        _ => json!({"protocol_version":1,"instance_id":INSTANCE,"state":state,
            "base_url":ENDPOINT,"model_ready":state != "loading","ready":state == "warm_ready",
            "warmup_completed":state == "warm_ready","busy":false,"closing":false,
            "inference_mode":"offline_final"}),
    }
}

fn stopped() -> Value {
    json!({"protocol_version":1,"instance_id":INSTANCE,"state":"owned_stopped",
        "reason":"owner_stop","forced_group_stop":false,"service_exit_code":0,
        "linux_cleanup_confirmed":true,"process_started":true})
}

fn waiting() -> Value {
    json!({"protocol_version":1,"instance_id":INSTANCE,"state":"waiting_gpu",
        "ready":false,"warmup_completed":false})
}

#[test]
fn owned_final_gpu_wait_is_optional_once_before_start_and_never_ready() -> Result<()> {
    let mut state = Lifecycle::default();
    let queued = lifecycle_event(&bytes(&waiting())?, INSTANCE)?;
    state.observe(&queued)?;
    assert!(!state.running());
    assert!(state.observe(&queued).is_err());
    for phase in [
        "starting",
        "awaiting_owner",
        "loading",
        "warming",
        "warm_ready",
    ] {
        state.observe(&lifecycle_event(&bytes(&preparing(phase))?, INSTANCE)?)?;
    }
    assert!(state.running());
    assert!(state.observe(&queued).is_err());
    for field in ["ready", "warmup_completed"] {
        let mut value = waiting();
        value[field] = json!(true);
        assert!(lifecycle_event(&bytes(&value)?, INSTANCE).is_err());
    }
    for field in ["pid", "process_group"] {
        let mut value = waiting();
        value[field] = json!(2345);
        assert!(lifecycle_event(&bytes(&value)?, INSTANCE).is_err());
    }
    Ok(())
}

#[test]
fn owned_final_wait_cancellation_can_confirm_no_child_without_starting_receipt() -> Result<()> {
    let mut state = Lifecycle::default();
    state.observe(&lifecycle_event(&bytes(&waiting())?, INSTANCE)?)?;
    let mut terminal = stopped();
    terminal["process_started"] = json!(false);
    terminal["service_exit_code"] = json!(1); // Existing no-child sentinel, not a child exit.
    state.observe(&lifecycle_event(&bytes(&terminal)?, INSTANCE)?)?;
    assert!(!state.running());
    let proof = cleanup_receipt(INSTANCE, Some(0), state.terminal.as_ref().unwrap(), false)?;
    assert_eq!(proof["linux_cleanup_confirmed"], true);
    assert_eq!(proof["receipt"]["process_started"], false);
    assert_eq!(state.phase, 0);
    Ok(())
}

#[test]
fn owned_final_starting_binds_instance_and_process_group_without_forwarding_unknown_fields()
-> Result<()> {
    let mut input = preparing("starting");
    input["bearer_token"] = json!("must-not-enter-progress");
    let clean = lifecycle_event(&bytes(&input)?, INSTANCE)?;
    assert!(clean.get("bearer_token").is_none());
    assert_eq!(clean["process_group"], 2345);
    assert!(lifecycle_event(&bytes(&input)?, "other-instance").is_err());
    input["process_group"] = json!(999);
    assert!(lifecycle_event(&bytes(&input)?, INSTANCE).is_err());
    input["process_group"] = json!(2345);
    input["ready"] = json!(true);
    assert!(lifecycle_event(&bytes(&input)?, INSTANCE).is_err());
    Ok(())
}

#[test]
fn owned_final_needs_warming_before_warm_ready_and_a_stable_endpoint() -> Result<()> {
    let mut state = Lifecycle::default();
    for phase in ["starting", "awaiting_owner", "loading"] {
        state.observe(&lifecycle_event(&bytes(&preparing(phase))?, INSTANCE)?)?;
    }
    assert!(!state.running());
    assert!(
        state
            .observe(&lifecycle_event(
                &bytes(&preparing("warm_ready"))?,
                INSTANCE
            )?)
            .is_err()
    );
    let mut changed = preparing("warming");
    changed["base_url"] = json!("http://127.0.0.1:57432");
    assert!(
        state
            .observe(&lifecycle_event(&bytes(&changed)?, INSTANCE)?)
            .is_err()
    );
    state.observe(&lifecycle_event(&bytes(&preparing("warming"))?, INSTANCE)?)?;
    assert!(!state.running());
    state.observe(&lifecycle_event(
        &bytes(&preparing("warm_ready"))?,
        INSTANCE,
    )?)?;
    assert!(state.running());
    assert!(
        state
            .observe(&lifecycle_event(
                &bytes(&preparing("warm_ready"))?,
                INSTANCE
            )?)
            .is_err()
    );
    Ok(())
}

#[test]
fn owned_final_rejects_early_ready_flags_and_nonlocal_endpoints() -> Result<()> {
    for field in ["ready", "warmup_completed"] {
        let mut input = preparing("warming");
        input[field] = json!(true);
        assert!(lifecycle_event(&bytes(&input)?, INSTANCE).is_err());
    }
    for endpoint in [
        "http://localhost:57431",
        "https://127.0.0.1:57431",
        "http://127.0.0.1",
        "http://user:secret@127.0.0.1:57431",
        "http://127.0.0.1:57431/path",
        "http://127.0.0.1:57431?token=x",
        "http://127.0.0.1:57431#token",
        "http://192.0.2.1:57431",
    ] {
        let mut input = preparing("loading");
        input["base_url"] = json!(endpoint);
        assert!(
            lifecycle_event(&bytes(&input)?, INSTANCE).is_err(),
            "{endpoint}"
        );
    }
    assert!(lifecycle_event(&bytes(&preparing("warm_ready"))?, INSTANCE).is_ok());
    Ok(())
}

#[test]
fn child_stop_and_pending_events_never_prove_linux_group_cleanup() -> Result<()> {
    let input = json!({"protocol_version":1,"instance_id":INSTANCE,"state":"stopped",
        "ready":false,"warmup_completed":false,"http_closed":true,"worker_retired":true,
        "process_exit_still_required":true,"linux_cleanup_confirmed":true,"pid":2345});
    let child = lifecycle_event(&bytes(&input)?, INSTANCE)?;
    assert!(child.get("linux_cleanup_confirmed").is_none());
    assert!(child.get("pid").is_none());
    assert!(cleanup_receipt(INSTANCE, Some(0), &child, false).is_err());
    let pending = json!({"protocol_version":1,"instance_id":INSTANCE,"state":"cleanup_pending",
        "ready":false,"warmup_completed":false,"worker_retired":false});
    assert!(
        cleanup_receipt(
            INSTANCE,
            Some(0),
            &lifecycle_event(&bytes(&pending)?, INSTANCE)?,
            false
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn owned_final_preflight_failure_retains_proven_cleanup_without_success() -> Result<()> {
    let mut input = stopped();
    input["reason"] = json!("startup_failed");
    input["process_started"] = json!(false);
    input["service_exit_code"] = json!(1);
    let terminal = lifecycle_event(&bytes(&input)?, INSTANCE)?;
    let mut state = Lifecycle::default();
    state.observe(&terminal)?;
    assert!(!state.running());
    let cleanup = cleanup_receipt(INSTANCE, Some(1), state.terminal.as_ref().unwrap(), false)?;
    assert_eq!(cleanup["cleanup_confirmed"], true);
    assert_eq!(cleanup["normal_owner_stop"], false);
    assert_eq!(cleanup["receipt"]["process_started"], false);
    assert!(state.observe(&terminal).is_err());
    Ok(())
}

#[test]
fn owned_final_cleanup_requires_matching_explicit_owner_proof() -> Result<()> {
    let mut input = stopped();
    let clean = lifecycle_event(&bytes(&input)?, INSTANCE)?;
    assert_eq!(
        cleanup_receipt(INSTANCE, Some(0), &clean, false)?["normal_owner_stop"],
        true
    );
    assert!(lifecycle_event(&bytes(&input)?, "old-instance").is_err());
    input["linux_cleanup_confirmed"] = json!(false);
    assert!(lifecycle_event(&bytes(&input)?, INSTANCE).is_err());
    input["linux_cleanup_confirmed"] = json!(true);
    input.as_object_mut().unwrap().remove("process_started");
    assert!(lifecycle_event(&bytes(&input)?, INSTANCE).is_err());
    let failed = lifecycle_event(
        &bytes(&json!({"protocol_version":1,"instance_id":INSTANCE,
        "state":"cleanup_failed","linux_cleanup_confirmed":false}))?,
        INSTANCE,
    )?;
    assert!(cleanup_receipt(INSTANCE, Some(0), &failed, false).is_err());
    Ok(())
}

#[tokio::test]
async fn owned_final_lifecycle_reader_bounds_bytes_and_requires_a_complete_line() -> Result<()> {
    let input = bytes(&preparing("starting"))?;
    let mut reader = BufReader::new(input.as_slice());
    assert_eq!(
        read_line(&mut reader).await?.as_deref(),
        Some(input.as_slice())
    );
    assert!(read_line(&mut reader).await?.is_none());
    let mut reader = BufReader::new(&b"{\"state\":\"starting\"}"[..]);
    assert!(read_line(&mut reader).await.is_err());
    let oversized = vec![b'x'; 4097];
    let mut reader = BufReader::new(oversized.as_slice());
    assert!(read_line(&mut reader).await.is_err());
    Ok(())
}

#[tokio::test]
async fn owned_final_cancellation_observes_existing_true_and_closed_controller() -> Result<()> {
    let (_send, mut receive) = watch::channel(true);
    tokio::time::timeout(Duration::from_millis(100), wait_cancelled(&mut receive)).await?;
    let (send, mut receive) = watch::channel(false);
    drop(send);
    tokio::time::timeout(Duration::from_millis(100), wait_cancelled(&mut receive)).await?;
    Ok(())
}

#[test]
fn owned_final_config_requires_explicit_linux_paths_without_host_defaults() {
    let valid = FinalAsrConfig {
        distribution: "Ubuntu".into(),
        python_executable: "/opt/tutor/python".into(),
        cache_directory: "/mnt/c/tutor-cache".into(),
    };
    assert!(valid.validate().is_ok());
    for path in [
        "python3",
        r"C:\Python\python.exe",
        "/",
        "/opt/python\n--other",
    ] {
        let mut config = valid.clone();
        config.python_executable = path.into();
        assert!(config.validate().is_err());
    }
    for distribution in ["", "-d", "Ubuntu\n--exec", " Ubuntu"] {
        let mut config = valid.clone();
        config.distribution = distribution.into();
        assert!(config.validate().is_err());
    }
}

#[cfg(windows)]
#[test]
fn owned_final_wsl_root_handles_extended_drive_paths_and_rejects_unc() -> Result<()> {
    let ordinary = Path::new(r"C:\Users\Example\AI TUTOR");
    assert_eq!(wsl_path_argument(ordinary)?, ordinary);
    assert_eq!(
        wsl_path_argument(Path::new(r"\\?\C:\Users\Example\AI TUTOR"))?,
        ordinary
    );
    assert_eq!(
        wsl_path_argument(Path::new(r"\\?\d:\教学 项目"))?,
        Path::new(r"D:\教学 项目")
    );
    for path in [
        r"\\server\share\AI TUTOR",
        r"\\?\UNC\server\share\AI TUTOR",
        r"C:AI TUTOR",
        r"\\.\C:\AI TUTOR",
    ] {
        assert!(wsl_path_argument(Path::new(path)).is_err());
    }
    Ok(())
}
