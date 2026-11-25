use super::*;

fn row() -> Row {
    Row {
        ordinal: 0,
        id: "00000".into(),
        pcm_file: "pcm/0000-00000.pcm".into(),
        pcm_sha256: "synthetic_not_used_on_wire".into(),
        pcm_bytes: 3200,
        sample_count: 1600,
        sample_rate: 16000,
    }
}

#[test]
fn empty_final_is_valid_and_not_run_rows_keep_original_denominator_identity() -> Result<()> {
    let mut revision = 0;
    let mut text = String::new();
    let value = json!({"protocol_version":1,"session_id":"synthetic-session","recording_generation":1,
        "revision":0,"text":"","final":true,"sequence":5});
    validate(
        &value,
        "synthetic-session",
        1,
        Some(5),
        true,
        &mut revision,
        &mut text,
    )
    .map_err(anyhow::Error::msg)?;
    assert!(text.is_empty());
    let original = row();
    let output = result_stub(&original, "accuracy");
    assert_eq!(output["id"], "00000");
    assert_eq!(output["ordinal"], 0);
    assert_eq!(output["status"], "not_run");
    assert_eq!(output["attempted"], false);
    assert_eq!(output["hypothesis"], "");
    assert_eq!(output["final_received"], false);
    Ok(())
}

#[tokio::test]
async fn preexisting_stop_records_no_start_attempt_and_no_cancel_request() -> Result<()> {
    let directory = tempfile::tempdir()?;
    fs::write(directory.path().join("stop-request.json"), b"{}")?;
    let client = Arc::new(AsrClient::new(
        "a".repeat(32),
        "http://127.0.0.1:9".into(),
        "b".repeat(32),
    )?);
    // The ready stop branch wins before any HTTP future is polled. No server,
    // model, device, descriptor or fallback is constructed by this test.
    let output = utterance(
        client,
        &row(),
        vec![0; 3200],
        "accuracy",
        directory.path(),
        tokio::time::Instant::now() + Duration::from_secs(60),
    )
    .await;
    assert_eq!(output["status"], "not_run");
    assert_eq!(output["attempted"], false);
    assert_eq!(output["error_code"], "operator_stop");
    assert_eq!(output["failure_origin"], "operator_stop");
    assert_eq!(output["failure_detail"], "operator_stop");
    assert!(output["failure_sequence"].is_null());
    assert!(output["failure_payload_bytes"].is_null());
    assert_eq!(output["failure_last_valid_revision"], 0);
    assert_eq!(output["client_unavailable_after_error"], false);
    assert!(output["cancel_request_attempted"].is_null());
    assert_eq!(output["pcm_bytes_send_attempted"], 0);
    assert_eq!(output["pcm_bytes_acknowledged"], 0);
    assert_eq!(output["all_pcm_acknowledged"], false);
    assert!(output["first_nonempty_partial_ms"].is_null());
    Ok(())
}

#[test]
fn inference_rows_ignore_reference_text_and_method_separates_fast_and_paced_scope() -> Result<()> {
    let value = json!({"ordinal":0,"id":"00000","pcm_file":"pcm/0000-00000.pcm","pcm_sha256":"hash",
        "pcm_bytes":3200,"sample_count":1600,"sample_rate":16000,
        "reference":"PRIVATE_REFERENCE_NOT_A_MODEL_HINT","language":"mixed","reference_uncertain":false});
    let parsed: Row = serde_json::from_value(value)?;
    let output = result_stub(&parsed, "realtime");
    assert!(!output.to_string().contains("PRIVATE_REFERENCE"));
    assert_eq!(output["language_hint"], "auto");
    let method: Value = serde_json::from_str(METHOD)?;
    assert_eq!(method["attempts"]["total_corpus_utterance_ceiling"], 1327);
    assert_eq!(method["phases"][0]["utterance_ceiling"], 1315);
    assert_eq!(method["phases"][1]["utterance_ceiling"], 12);
    assert_eq!(method["input"]["wire_chunk_samples"], CHUNK_SAMPLES);
    assert_eq!(method["attempts"]["whisper_fallback"], false);
    Ok(())
}

#[test]
fn failure_prefix_is_a_new_bounded_diagnostic_not_an_accuracy_repair() -> Result<()> {
    let method: Value = serde_json::from_str(FAILURE_METHOD)?;
    assert_eq!(
        method["original_ordinals"],
        json!({"first":0,"last_inclusive":362,"count":363})
    );
    assert_eq!(method["attempts_per_row"], 1);
    assert_eq!(method["maximum_corpus_attempts"], FAILURE_PREFIX_ROWS);
    assert_eq!(method["automatic_retries"], 0);
    assert_eq!(method["original_accuracy_completion_credit"], false);
    let source: Vec<_> = (0..1315).collect();
    let selected: Vec<_> = source.iter().take(FAILURE_PREFIX_ROWS).copied().collect();
    assert_eq!(selected.len(), 363);
    assert_eq!(selected.first(), Some(&0));
    assert_eq!(selected.last(), Some(&362));
    Ok(())
}

mod failure_observation {
    use super::*;
    use axum::{
        Json, Router,
        body::Bytes,
        extract::State,
        http::{StatusCode, Uri},
        response::{IntoResponse, Response},
    };

    #[tokio::test]
    async fn connection_failure_records_flags_without_url_or_credentials() -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        drop(listener);
        let client = AsrClient::new(
            "a".repeat(32),
            format!("http://{address}"),
            "PRIVATE_TOKEN_123456789012345".into(),
        )?;
        let control = Control {
            cancelled: AtomicBool::new(false),
            failure: Mutex::new(None),
            generation: Arc::new(AtomicU64::new(1)),
            token: 1,
            info: Arc::new(Mutex::new(json!({}))),
        };
        let mut observed = None;
        let error = response_observed(
            client
                .post("/PRIVATE_PATH?key=PRIVATE_QUERY")
                .body("PRIVATE_BODY"),
            &control,
            Some(&client),
            Some(&mut observed),
        )
        .await
        .unwrap_err();
        assert_eq!(error, "本地流式识别服务未响应");
        assert!(client.unavailable());
        let observed = observed.expect("request failure observation");
        assert_eq!(observed["is_connect"], true);
        // On Windows the existing 500 ms connect deadline can elapse before
        // the closed-port refusal is delivered. Both are connect failures.
        println!("numeric connect-failure observation: {observed}");
        assert!(observed["http_status"].is_null());
        assert!(!observed.to_string().contains("PRIVATE_"));
        assert!(!observed.to_string().contains("127.0.0.1"));
        assert!(observed["source_chain_depth"].as_u64().unwrap() <= 8);
        Ok(())
    }

    #[derive(Clone, Copy)]
    enum Case {
        WrongInstance,
        WrongRevision,
        Http409,
        InvalidJson,
    }
    struct Fake {
        case: Case,
        session: String,
        generation: u64,
        chunks: usize,
    }

    async fn endpoint(State(state): State<Arc<Mutex<Fake>>>, uri: Uri, body: Bytes) -> Response {
        let mut state = state.lock().unwrap();
        if uri.path() == "/v1/asr/start" {
            let request: Value = serde_json::from_slice(&body).unwrap();
            state.session = s(&request, "session_id").into();
            state.generation = request["recording_generation"].as_u64().unwrap();
            return Json(
                json!({"instance_id":"a".repeat(32),"protocol_version":1,"session_id":state.session,
                "recording_generation":state.generation,"revision":0,"text":"","final":false}),
            )
            .into_response();
        }
        if uri.path().ends_with("/cancel") {
            return Json(
                json!({"instance_id":"a".repeat(32),"protocol_version":1,"session_id":state.session,
                "recording_generation":state.generation,"canceled":true}),
            )
            .into_response();
        }
        assert!(uri.path().ends_with("/chunk"));
        assert_eq!(uri.query(), Some("sequence=0"));
        assert_eq!(body.len(), 640);
        state.chunks += 1;
        let mut reply=match state.case {
            Case::Http409=>(StatusCode::CONFLICT,Json(json!({"error":"PRIVATE_BODY_NOT_RETAIN"}))).into_response(),
            Case::InvalidJson=>(StatusCode::OK,"PRIVATE_INVALID_JSON_NOT_RETAIN").into_response(),
            case=>Json(json!({"instance_id":if matches!(case,Case::WrongInstance){"c".repeat(32)}else{"a".repeat(32)},
                "protocol_version":1,"session_id":state.session,"recording_generation":state.generation,
                "revision":0,"text":if matches!(case,Case::WrongRevision){"PRIVATE_REJECTED_TEXT"}else{""},
                "final":false,"sequence":0})).into_response(),
        };
        for (name, value) in [
            ("x-asr-connection-id", "9"),
            ("x-asr-request-id", "2"),
            ("connection", "keep-alive"),
        ] {
            reply
                .headers_mut()
                .insert(name, axum::http::HeaderValue::from_static(value));
        }
        reply
    }

    #[tokio::test]
    async fn real_client_preserves_response_or_validation_error_without_private_payload()
    -> Result<()> {
        for (case, origin, detail, unavailable) in [
            (
                Case::WrongInstance,
                "response",
                "本地流式识别实例不匹配",
                true,
            ),
            (
                Case::WrongRevision,
                "validate",
                "流式识别文本修订无效或超限",
                true,
            ),
            (Case::Http409, "response", "本地流式识别返回状态 409", false),
            (
                Case::InvalidJson,
                "response",
                "本地流式识别响应格式无效",
                true,
            ),
        ] {
            let temporary = tempfile::tempdir()?;
            let state = Arc::new(Mutex::new(Fake {
                case,
                session: String::new(),
                generation: 0,
                chunks: 0,
            }));
            let router = Router::new().fallback(endpoint).with_state(state.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let task = tokio::spawn(async move {
                let _ = axum::serve(listener, router).await;
            });
            let client = Arc::new(AsrClient::new(
                "a".repeat(32),
                format!("http://{address}"),
                "b".repeat(32),
            )?);
            let output = utterance(
                client,
                &row(),
                vec![0; 3200],
                "failure_prefix",
                temporary.path(),
                tokio::time::Instant::now() + Duration::from_secs(10),
            )
            .await;
            task.abort();
            let _ = task.await;
            assert_eq!(output["status"], "failed");
            assert_eq!(output["error_code"], "chunk_response_failed");
            assert_eq!(output["failure_origin"], origin);
            assert_eq!(output["failure_detail"], detail);
            assert_eq!(output["failure_sequence"], 0);
            assert_eq!(output["failure_payload_bytes"], 640);
            assert_eq!(output["failure_last_valid_revision"], 0);
            assert_eq!(output["client_unavailable_after_error"], unavailable);
            assert!(output["request_send_error"].is_null());
            assert_eq!(output["transport_summary"]["validated_response_count"], 1);
            assert_eq!(
                output["transport_summary"]["responses_with_missing_headers"],
                1
            );
            assert_eq!(
                output["transport_summary"]["complete_transport_response_count"],
                0
            );
            assert_eq!(output["cancel_receipt_validated"], false);
            assert_eq!(
                output["cancel_transport_observation"]["missing_header_fields"],
                3
            );
            assert_eq!(state.lock().unwrap().chunks, 1);
            assert_eq!(output["hypothesis"], "");
            assert_eq!(output["partial_at_failure"], "");
            assert!(!output.to_string().contains("PRIVATE_"));
        }
        Ok(())
    }
}

#[tokio::test]
async fn successful_response_preserves_value_and_observes_only_fixed_headers() -> Result<()> {
    let router=axum::Router::new().route("/",axum::routing::post(||async {
        ([("x-asr-connection-id","7"),("x-asr-request-id","11"),("connection","keep-alive"),("x-private","PRIVATE_HEADER")],
        axum::Json(json!({"instance_id":"a".repeat(32),"protocol_version":1,"session_id":"header-test",
            "recording_generation":1,"revision":0,"text":"","final":false})))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let client = AsrClient::new("a".repeat(32), format!("http://{address}"), "b".repeat(32))?;
    let control = Control {
        cancelled: AtomicBool::new(false),
        failure: Mutex::new(None),
        generation: Arc::new(AtomicU64::new(1)),
        token: 1,
        info: Arc::new(Mutex::new(json!({}))),
    };
    let mut send_failure = None;
    let observed = response_observed(
        client.post("/"),
        &control,
        Some(&client),
        Some(&mut send_failure),
    )
    .await;
    let ordinary = response(client.post("/"), &control, Some(&client)).await;
    task.abort();
    let _ = task.await;
    let observed = observed.map_err(anyhow::Error::msg)?;
    assert_eq!(ordinary.map_err(anyhow::Error::msg)?, observed.value);
    assert_eq!(
        (
            observed.transport.connection_id,
            observed.transport.request_id
        ),
        (Some(7), Some(11))
    );
    assert_eq!(
        observed.transport.connection_policy,
        Some(ConnectionPolicy::KeepAlive)
    );
    let mut revision = 0;
    let mut text = String::new();
    validate(
        &observed.value,
        "header-test",
        1,
        None,
        false,
        &mut revision,
        &mut text,
    )
    .map_err(anyhow::Error::msg)?;
    let mut summary = TransportSummary::default();
    summary.observe(observed.transport);
    assert_eq!(summary.value()["complete_transport_response_count"], 1);
    assert_eq!(
        summary.value()["all_validated_responses_have_complete_transport"],
        true
    );
    assert!(!summary.value().to_string().contains("PRIVATE"));
    assert!(send_failure.is_none());
    assert!(!client.unavailable());
    Ok(())
}

#[test]
fn transport_headers_reject_missing_duplicate_invalid_and_private_fields() {
    use reqwest::header::{HeaderMap, HeaderValue};
    let mut headers = HeaderMap::new();
    let empty = observed_transport(&headers);
    assert_eq!(
        (empty.missing_header_fields, empty.invalid_header_fields),
        (3, 0)
    );
    headers.insert("x-asr-connection-id", HeaderValue::from_static("12"));
    headers.insert("x-asr-request-id", HeaderValue::from_static("34"));
    headers.insert("connection", HeaderValue::from_static("Keep-Alive"));
    headers.insert("authorization", HeaderValue::from_static("PRIVATE_TOKEN"));
    headers.insert("x-private", HeaderValue::from_static("PRIVATE_URL_BODY"));
    let valid = observed_transport(&headers);
    assert_eq!(
        (valid.connection_id, valid.request_id),
        (Some(12), Some(34))
    );
    assert_eq!(valid.connection_policy, Some(ConnectionPolicy::KeepAlive));
    assert!(!serde_json::to_string(&valid).unwrap().contains("PRIVATE"));
    for bad in ["0", "-1", "+1", "1,2", " 1", "18446744073709551616"] {
        headers.insert("x-asr-request-id", HeaderValue::from_str(bad).unwrap());
        let observed = observed_transport(&headers);
        assert_eq!(observed.request_id, None, "{bad}");
        assert_eq!(
            (
                observed.missing_header_fields,
                observed.invalid_header_fields
            ),
            (0, 1)
        );
    }
    headers.insert("x-asr-request-id", HeaderValue::from_static("34"));
    headers.append("x-asr-request-id", HeaderValue::from_static("34"));
    headers.insert("connection", HeaderValue::from_static("keep-alive, close"));
    let invalid = observed_transport(&headers);
    assert_eq!(invalid.request_id, None);
    assert_eq!(invalid.connection_policy, None);
    assert_eq!(invalid.invalid_header_fields, 2);
}

#[test]
fn transport_summary_reports_coverage_without_bridging_unknown_connections() {
    use reqwest::header::{HeaderMap, HeaderValue};
    let sample = |connection: u64, request: u64| {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-asr-connection-id",
            HeaderValue::from_str(&connection.to_string()).unwrap(),
        );
        headers.insert(
            "x-asr-request-id",
            HeaderValue::from_str(&request.to_string()).unwrap(),
        );
        headers.insert("connection", HeaderValue::from_static("keep-alive"));
        observed_transport(&headers)
    };
    let mut stats = TransportSummary::default();
    assert_eq!(
        stats.value()["all_validated_responses_have_complete_transport"],
        false
    );
    for item in [
        sample(1, 2),
        sample(1, 7),
        observed_transport(&HeaderMap::new()),
        sample(2, 1),
        sample(1, 7),
        sample(1, 6),
    ] {
        stats.observe(item);
    }
    let value = stats.value();
    assert_eq!(value["validated_response_count"], 6);
    assert_eq!(value["complete_transport_response_count"], 5);
    assert_eq!(value["incomplete_transport_response_count"], 1);
    assert_eq!(value["missing_header_field_count"], 3);
    assert_eq!(value["distinct_connection_count"], 2);
    assert_eq!(value["adjacent_connection_switches"], 1);
    assert_eq!(value["same_connection_request_nonincreasing"], 2);
    assert_eq!(value["first_connection_id"], 1);
    assert_eq!(value["first_request_id"], 2);
    assert_eq!(value["last_request_id"], 6);
    assert_eq!(
        value["all_validated_responses_have_complete_transport"],
        false
    );
}

#[test]
fn paraformer_profile_is_explicit_and_cannot_reuse_another_method_or_prefix() -> Result<()> {
    for profile in [
        Profile::Zipformer,
        Profile::FailurePrefix,
        Profile::Paraformer,
    ] {
        let manifest = json!({"evaluation_profile":profile.name(),"method":serde_json::from_str::<Value>(profile.method())?,
            "backend":profile.backend(),"model_directory":profile.model_directory(),"model_final_right_context_samples":profile.right_context_samples()});
        for mode in ["accuracy", "realtime", "failure_prefix"] {
            assert_eq!(
                profile_for_run(&manifest, mode).is_ok(),
                if profile == Profile::FailurePrefix {
                    mode == "failure_prefix"
                } else {
                    mode != "failure_prefix"
                }
            );
        }
    }
    assert!(
        profile_for_run(
            &json!({"method":serde_json::from_str::<Value>(METHOD)?}),
            "accuracy"
        )
        .is_err()
    );
    assert!(profile_for_run(&json!({"evaluation_profile":"paraformer","method":serde_json::from_str::<Value>(METHOD)?}),"accuracy").is_err());
    let mut wrong = json!({"evaluation_profile":"paraformer","method":serde_json::from_str::<Value>(PARAFORMER_METHOD)?,
        "backend":"zipformer","model_directory":Profile::Paraformer.model_directory(),"model_final_right_context_samples":10560});
    assert!(profile_for_run(&wrong, "accuracy").is_err());
    wrong["backend"] = json!("paraformer");
    wrong["model_final_right_context_samples"] = json!(9600);
    assert!(profile_for_run(&wrong, "accuracy").is_err());
    assert_eq!(Profile::Zipformer.backend(), "zipformer");
    assert_eq!(Profile::FailurePrefix.backend(), "zipformer");
    assert_eq!(Profile::Paraformer.backend(), "paraformer");
    assert_eq!(Profile::Zipformer.right_context_samples(), 9600);
    assert_eq!(Profile::Paraformer.right_context_samples(), 10560);
    Ok(())
}

#[test]
fn paraformer_method_retains_population_scoring_and_fixed_asset_identity() -> Result<()> {
    let original: Value = serde_json::from_str(METHOD)?;
    let candidate: Value = serde_json::from_str(PARAFORMER_METHOD)?;
    for field in [
        "dataset",
        "reference_partition",
        "normalization",
        "phases",
        "attempts",
        "deadlines",
    ] {
        assert_eq!(candidate[field], original[field], "{field}");
    }
    let download_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(Profile::Paraformer.model_directory())
        .join("download-manifest.json");
    assert_eq!(
        hash(&download_path)?,
        candidate["model"]["asset_manifest"]["sha256"]
    );
    let download = read(&download_path)?;
    let assets = candidate["model"]["assets"].as_array().unwrap();
    let names: HashSet<_> = assets.iter().map(|asset| s(asset, "file")).collect();
    assert_eq!(
        names,
        HashSet::from(["decoder.int8.onnx", "encoder.int8.onnx", "tokens.txt"])
    );
    assert_eq!(assets.len(), 3);
    for asset in assets {
        assert!(download["files"].as_array().unwrap().contains(asset));
    }
    assert_eq!(
        candidate["model"]["asset_manifest"]["sha256"],
        "48e1b7cff691d593b549265d5a7fda280bdeeaeef592514035061fd5efafd96f"
    );
    assert_eq!(candidate["model"]["backend"], "paraformer");
    assert_eq!(
        candidate["attempts"]["total_corpus_utterance_ceiling"],
        1327
    );
    assert_eq!(candidate["input"]["wire_chunk_samples"], CHUNK_SAMPLES);
    Ok(())
}
