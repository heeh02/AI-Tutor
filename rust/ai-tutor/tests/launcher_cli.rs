use std::{
    io::{Read, Write},
    net::TcpListener,
    process::Command,
    time::{Duration, Instant},
};

fn executable() -> &'static str {
    env!("CARGO_BIN_EXE_ai-tutor")
}

#[test]
fn help_and_invalid_arguments_never_start_a_service() {
    let output = Command::new(executable()).arg("--help").output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--verify-kimi"));
    for args in [
        ["--probe-kimi", "--no-browser"],
        ["--unknown", "--no-browser"],
    ] {
        let output = Command::new(executable()).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("--help"));
    }
}

#[test]
fn occupied_port_has_safe_local_diagnostic_without_device_or_model_startup() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("runtime")).unwrap();
    std::fs::write(
        directory.path().join("runtime/server.json"),
        br#"{"token":"test-secret-token","private_prompt":"private-context"}"#,
    )
    .unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let output = Command::new(executable())
        .args(["--no-browser", "--root"])
        .arg(directory.path())
        .args(["--port", &listener.local_addr().unwrap().port().to_string()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let diagnostic =
        std::fs::read_to_string(directory.path().join("runtime/startup-error.json")).unwrap();
    assert!(diagnostic.contains("local_port_unavailable"));
    for secret in ["test-secret-token", "private-context", "?token="] {
        assert!(!diagnostic.contains(secret));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    }
    assert!(!directory.path().join("runtime/data").exists());
    assert!(!directory.path().join("runtime/kimi").exists());
}

fn one_local_response(listener: TcpListener, response: String) -> std::thread::JoinHandle<String> {
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("local test request missing: {error}"),
            }
        };
        // Windows may inherit the listener's nonblocking mode on accept. The
        // request reader below deliberately uses a bounded blocking socket.
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let count = socket.read(&mut buffer).unwrap();
            assert!(count > 0);
            request.extend_from_slice(&buffer[..count]);
            assert!(request.len() < 8192);
        }
        socket.write_all(response.as_bytes()).unwrap();
        String::from_utf8(request).unwrap()
    })
}

#[test]
fn headless_start_reuses_existing_project_even_with_another_port_and_proxy() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("runtime")).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let original = format!(r#"{{"pid":123,"port":{port},"token":"local-test-token"}}"#);
    let descriptor = directory.path().join("runtime/server.json");
    std::fs::write(&descriptor, &original).unwrap();
    let server = one_local_response(
        listener,
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".into(),
    );
    let output = Command::new(executable())
        .args(["--no-browser", "--port", "8766", "--root"])
        .arg(directory.path())
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("http_proxy", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("all_proxy", "http://127.0.0.1:1")
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .output()
        .unwrap();
    let request = server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(request.starts_with("GET /api/state "));
    assert!(request.contains("ai_tutor_local=local-test-token"));
    assert_eq!(std::fs::read_to_string(descriptor).unwrap(), original);
    assert!(!directory.path().join("runtime/data").exists());
    assert!(!directory.path().join("runtime/kimi").exists());
}

#[test]
fn project_lock_prevents_concurrent_start_before_initialization_and_recovers_on_close() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("runtime")).unwrap();
    let lock_path = directory.path().join("runtime/server.lock");
    let lock = std::fs::File::create(&lock_path).unwrap();
    lock.lock().unwrap();
    let output = Command::new(executable())
        .args(["--no-browser", "--root"])
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let diagnostic =
        std::fs::read_to_string(directory.path().join("runtime/startup-error.json")).unwrap();
    assert!(diagnostic.contains("project_already_running"));
    assert!(!directory.path().join("runtime/server.json").exists());
    assert!(!directory.path().join("runtime/data").exists());
    drop(lock);
    let recovered = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    recovered.try_lock().unwrap();
}

#[test]
fn stop_rejects_redirect_without_contacting_the_redirect_target() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("runtime")).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let target = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    target.set_nonblocking(true).unwrap();
    std::fs::write(
        directory.path().join("runtime/server.json"),
        format!(
            r#"{{"port":{},"token":"local-test-token"}}"#,
            listener.local_addr().unwrap().port()
        ),
    )
    .unwrap();
    let server = one_local_response(
        listener,
        format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:{}/unexpected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            target.local_addr().unwrap().port()
        ),
    );
    let output = Command::new(executable())
        .args(["--stop", "--root"])
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(server.join().unwrap().starts_with("POST /api/shutdown "));
    assert!(!output.status.success());
    assert_eq!(
        target.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
