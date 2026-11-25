//! Synthetic Store/HTTP/cancellation fixtures; never invoke a model or play audio.
use super::*;
use crate::kimi::tutor_segments::SegmentDecoder;

struct Fixture {
    _dir: tempfile::TempDir,
    app: Arc<Workstation>,
    sid: String,
    rid: String,
    tid: String,
    generation: i64,
    epoch: i64,
    request: Value,
    reply: Value,
}
impl Fixture {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let app = Workstation::new(dir.path().into(), "synthetic-delivery-token".into())?;
        let (sid, rid, tid, generation, request, reply) = {
            let stores = app.stores.lock().unwrap();
            let store = &stores["real"];
            let learner = store.create_learner("synthetic tutor delivery", "zh", "guided")?;
            let lid = s(&learner, "learner_id");
            let sid = s(&store.create_session(lid)?, "session_id").to_owned();
            let generation = store.bump(&sid, "generating")?;
            let event = store.record_education_event(
                lid,
                &sid,
                "learner_report",
                "learner_statement",
                &json!({"text":"我已经测过距离，结果稳定。"}),
            )?;
            let eid = s(&event, "event_id");
            let rid = uid("request");
            let tid = store.create_tutor_stream(&sid, &rid, generation, 0)?;
            let request = json!({"request_id":rid,"learner_id":lid,"session_id":sid,"generation":generation,"context":{"evidence":[{"event_id":eid}]}});
            let reply = json!({"schema_version":"1.0","request_id":rid,"reply_text":"first. second.","teaching_move":"give_hint",
                "task_update_candidates":[{"field":"goal","value":"synthetic verified goal","source_event_ids":[eid]}],
                "memory_candidates":[{"kind":"reported_progress","content":"synthetic confirmed report","scope":"current_activity","source_event_ids":[eid]}],
                "used_evidence_ids":[eid],"needs_clarification":false});
            let mut decoder = SegmentDecoder::new(&request, generation as u64)?;
            for (sequence, text) in ["first. ", "second."].iter().enumerate() {
                for segment in decoder.push(format!("{}\n",json!({"schema_version":"1.0","type":"segment","request_id":rid,"sequence":sequence,"text":text,"used_evidence_ids":[eid]})).as_bytes())? {
                    store.append_tutor_segment(&sid,&rid,&segment)?;
                }
            }
            (sid, rid, tid, generation, request, reply)
        };
        *app.current.lock().unwrap() = Some(("real".into(), sid.clone()));
        Ok(Self {
            _dir: dir,
            app,
            sid,
            rid,
            tid,
            generation,
            epoch: 0,
            request,
            reply,
        })
    }
    fn final_pending(&self) -> Result<()> {
        self.app.stores.lock().unwrap()["real"].mark_tutor_final(&self.sid, &self.rid)?;
        self.app.pending.lock().unwrap().insert(
            self.tid.clone(),
            Pending {
                sid: self.sid.clone(),
                generation: self.generation,
                epoch: self.epoch,
                reply: self.reply.clone(),
                _foreground: None,
            },
        );
        Ok(())
    }
}

#[test]
fn text_stream_requires_explicit_bounded_configuration() -> Result<()> {
    let dir = tempfile::tempdir()?;
    ensure!(DeliveryMode::load(dir.path())? == DeliveryMode::WholeReply);
    std::fs::create_dir(dir.path().join("runtime"))?;
    let path = dir.path().join("runtime/tutor-delivery.json");
    for invalid in [
        r#"{"protocol_version":2,"mode":"segments"}"#.to_owned(),
        r#"{"protocol_version":1,"mode":"segments","surprise":true}"#.to_owned(),
        "x".repeat(2049),
    ] {
        std::fs::write(&path, invalid)?;
        ensure!(DeliveryMode::load(dir.path()).is_err());
    }
    std::fs::write(path, r#"{"protocol_version":1,"mode":"segments"}"#)?;
    ensure!(DeliveryMode::load(dir.path())? == DeliveryMode::Segments);
    Ok(())
}

#[tokio::test]
async fn invalid_foreground_policy_does_not_prevent_workstation_construction() -> Result<()> {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir(dir.path().join("runtime"))?;
    std::fs::write(
        dir.path().join("runtime/teaching-provider-policy.json"),
        b"{}",
    )?;
    let app = Workstation::new(dir.path().into(), "synthetic-provider-policy".into())?;
    assert_eq!(app.provider.foreground_status()["provider"], "invalid");
    assert_eq!(app.provider.foreground_status()["available"], false);
    assert_eq!(app.provider.status()["metrics"]["requests"], 0);
    app.close().await;
    Ok(())
}

#[tokio::test]
async fn selected_flash_late_generation_cannot_apply_candidates_or_renew_delivery() -> Result<()> {
    let mut f = Fixture::new()?;
    let mut transport = {
        let app =
            Arc::get_mut(&mut f.app).ok_or_else(|| anyhow!("fixture app shared too early"))?;
        let provider = Arc::get_mut(&mut app.provider)
            .ok_or_else(|| anyhow!("fixture provider shared too early"))?;
        let transport = provider.scripted_segments_for_test();
        provider.select_scripted_flash_for_test();
        transport
    };
    // Start a fresh, empty stream; the fixture's earlier prefix is not reused.
    {
        let stores = f.app.stores.lock().unwrap();
        let store = &stores["real"];
        f.generation = store.bump(&f.sid, "generating")?;
        f.rid = uid("request");
        f.tid = store.create_tutor_stream(&f.sid, &f.rid, f.generation, f.epoch)?;
        f.request["request_id"] = json!(f.rid);
        f.request["generation"] = json!(f.generation);
        f.reply["request_id"] = json!(f.rid);
    }
    let before = f.app.stores.lock().unwrap()["real"].session(&f.sid)?["state"].clone();
    let foreground = f.app.memory_worker.foreground();
    let (_cancel, mut cancelled) = watch::channel(false);
    let operation = f.app.generate_stream(
        "real",
        &f.sid,
        &f.request,
        f.epoch,
        foreground,
        &mut cancelled,
    );
    let (result, sent) = tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(operation, async {
        let call = transport.recv().await.ok_or_else(|| anyhow!("fake selected call missing"))?;
        f.app.stores.lock().unwrap()["real"].bump(&f.sid, "idle")?;
        let evidence = f.reply["used_evidence_ids"].clone();
        let mut metadata = f.reply.clone();
        metadata.as_object_mut().unwrap().remove("reply_text");
        let segments = vec![
            json!({"schema_version":"1.0","type":"segment","request_id":f.rid,"sequence":0,"text":"first. ","used_evidence_ids":evidence}),
            json!({"schema_version":"1.0","type":"segment","request_id":f.rid,"sequence":1,"text":"second.","used_evidence_ids":evidence}),
            json!({"schema_version":"1.0","type":"final_metadata","request_id":f.rid,"segment_count":2,"reply":metadata}),
        ];
        call.response.send(segments).map_err(|_| anyhow!("fake response lost"))
    }) }).await?;
    sent?;
    assert!(result.is_err());
    {
        let stores = f.app.stores.lock().unwrap();
        let store = &stores["real"];
        let session = store.session(&f.sid)?;
        assert_eq!(session["generation"], f.generation + 1);
        assert_eq!(session["state"]["goal"], before["goal"]);
        assert!(store.memories(s(&f.request, "learner_id"))?.is_empty());
        assert!(f.app.pending.lock().unwrap().is_empty());
    }
    assert_eq!(f.app.provider.foreground_status()["running"], 0);
    f.app.close().await;
    Ok(())
}

#[test]
fn segment_speech_requires_explicit_configuration_without_enabling_whole_reply_autoplay()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir(dir.path().join("runtime"))?;
    let path = dir.path().join("runtime/tutor-delivery.json");
    for config in [
        json!({"protocol_version":1,"mode":"segments"}),
        json!({"protocol_version":1,"mode":"segments","segment_speech":"disabled"}),
    ] {
        std::fs::write(&path, config.to_string())?;
        let mode = DeliveryMode::load(dir.path())?;
        ensure!(mode.segments() && !mode.segment_speech());
        ensure!(mode.status()["automatic_speech"] == false);
    }
    std::fs::write(
        &path,
        json!({"protocol_version":1,"mode":"segments","segment_speech":"after_display"})
            .to_string(),
    )?;
    let mode = DeliveryMode::load(dir.path())?;
    ensure!(mode.segments() && mode.segment_speech());
    ensure!(
        mode.status()["segment_speech"] == "after_display"
            && mode.status()["automatic_speech"] == false
    );
    std::fs::write(
        &path,
        json!({"protocol_version":1,"mode":"whole_reply","segment_speech":"after_display"})
            .to_string(),
    )?;
    ensure!(DeliveryMode::load(dir.path()).is_err());
    Ok(())
}

#[tokio::test]
async fn final_application_failure_keeps_actual_prefix_and_retries_once_without_audio() -> Result<()>
{
    let f = Fixture::new()?;
    f.final_pending()?;
    let audio_epoch = f.app.audio.speech_epoch();
    f.app
        .display_segment(&f.sid, &f.rid, f.generation, 0)
        .await?;
    f.app.stores.lock().unwrap()["real"].db.execute_batch("CREATE TEMP TRIGGER fail_stream_memory BEFORE INSERT ON memories WHEN NEW.content='synthetic confirmed report' BEGIN SELECT RAISE(ABORT,'synthetic delivery failure'); END;")?;
    ensure!(
        f.app
            .display_segment(&f.sid, &f.rid, f.generation, 1)
            .await
            .is_err()
    );
    {
        let stores = f.app.stores.lock().unwrap();
        let store = &stores["real"];
        let session = store.session(&f.sid)?;
        ensure!(session["turns"][0]["content"] == "first. second.");
        ensure!(session["turns"][0]["stream_phase"] == "final_pending");
        ensure!(
            session["state"]["goal"] == ""
                && store.memories(s(&f.request, "learner_id"))?.is_empty()
        );
        ensure!(f.app.pending.lock().unwrap().contains_key(&f.tid));
        store.db.execute_batch("DROP TRIGGER fail_stream_memory")?;
    }
    let completed = f
        .app
        .display_segment(&f.sid, &f.rid, f.generation, 1)
        .await?;
    ensure!(completed["phase"] == "completed");
    f.app
        .display_segment(&f.sid, &f.rid, f.generation, 1)
        .await?;
    {
        let stores = f.app.stores.lock().unwrap();
        let store = &stores["real"];
        ensure!(store.session(&f.sid)?["state"]["goal"] == "synthetic verified goal");
        ensure!(store.memories(s(&f.request, "learner_id"))?.len() == 1);
        ensure!(!f.app.pending.lock().unwrap().contains_key(&f.tid));
    }
    ensure!(f.app.audio.speech_epoch() == audio_epoch);
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn final_failure_preserves_displayed_prefix_and_marks_next_context_incomplete() -> Result<()>
{
    let f = Fixture::new()?;
    f.app
        .display_segment(&f.sid, &f.rid, f.generation, 0)
        .await?;
    {
        let stores = f.app.stores.lock().unwrap();
        let store = &stores["real"];
        store.fail_tutor_stream(&f.sid, &f.rid, "failed", "final_text_mismatch")?;
        let snapshot = store.tutor_stream_snapshot(&f.sid, &f.rid)?;
        ensure!(
            snapshot["segments"].as_array().unwrap().len() == 1
                && !snapshot.to_string().contains("second.")
        );
        let learner = store.learner(s(&f.request, "learner_id"))?;
        let event = store.record_education_event(
            s(&learner, "learner_id"),
            &f.sid,
            "learner_report",
            "learner_statement",
            &json!({"text":"再解释一下"}),
        )?;
        let context = build_context(
            store,
            &store.session(&f.sid)?,
            &learner,
            "再解释一下",
            s(&event, "event_id"),
            &Value::Null,
        )?;
        let delivered = context["recent_delivered_dialogue"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["role"] == "assistant")
            .unwrap();
        ensure!(delivered["stream_phase"] == "failed" && delivered["reply_complete"] == false);
        ensure!(!context.to_string().contains("second."));
        ensure!(store.memories(s(&f.request, "learner_id"))?.is_empty());
    }
    // A painted second segment's already-in-flight ACK remains a real display
    // fact even when the model has failed. It never applies final candidates.
    let late = f
        .app
        .display_segment(&f.sid, &f.rid, f.generation, 1)
        .await?;
    ensure!(
        late["phase"] == "failed" && late["shown_through"] == 1 && late["final_validated"] == false
    );
    ensure!(f.app.deliver(&f.sid, &f.tid).await.is_err());
    ensure!(f.app.playback(&f.sid, &f.tid).await.is_err());
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn final_evidence_upgrade_preserves_the_displayed_reply_during_its_own_correction()
-> Result<()> {
    let mut f = Fixture::new()?;
    let old = f.request["context"]["evidence"][0]["event_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let current = {
        let stores = f.app.stores.lock().unwrap();
        let store = &stores["real"];
        let lid = s(&f.request, "learner_id");
        ensure!(store.add_memory(lid,&json!({"kind":"reported_progress","content":"距离测量结果稳定","scope":"current_activity","source_event_ids":[old]}),0)?.is_some());
        s(
            &store.record_education_event(
                lid,
                &f.sid,
                "learner_report",
                "learner_statement",
                &json!({"text":"刚才说错了，距离测量结果并不稳定。"}),
            )?,
            "event_id",
        )
        .to_owned()
    };
    f.request["context"]["evidence"]
        .as_array_mut()
        .unwrap()
        .push(json!({"event_id":current}));
    f.reply["used_evidence_ids"] = json!([old, current]);
    f.reply["task_update_candidates"] = json!([]);
    f.reply["memory_candidates"] = json!([{"kind":"correction","content":"距离测量结果并不稳定","scope":"current_activity","source_event_ids":[old,current]}]);
    validate_reply(&f.reply, &f.request)?;
    f.final_pending()?;
    f.app
        .display_segment(&f.sid, &f.rid, f.generation, 0)
        .await?;
    {
        let stores = f.app.stores.lock().unwrap();
        let turn = &stores["real"].session(&f.sid)?["turns"][0];
        ensure!(turn["evidence"] == json!([old]));
        ensure!(turn["content"] == "first. ");
    }
    let completed = f
        .app
        .display_segment(&f.sid, &f.rid, f.generation, 1)
        .await?;
    ensure!(completed["phase"] == "completed");
    {
        let stores = f.app.stores.lock().unwrap();
        let store = &stores["real"];
        let session = store.session(&f.sid)?;
        ensure!(store.learner(s(&f.request, "learner_id"))?["epoch"] == 1);
        ensure!(session["turns"].as_array().unwrap().len() == 1);
        ensure!(
            session["turns"][0]["turn_id"] == f.tid
                && session["turns"][0]["content"] == "first. second."
        );
        ensure!(session["turns"][0]["stream_phase"] == "completed");
        ensure!(session["turns"][0]["evidence"] == json!([current]));
    }
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn incomplete_last_hint_keeps_its_phase_after_dialogue_window_trimming() -> Result<()> {
    let f = Fixture::new()?;
    f.app
        .display_segment(&f.sid, &f.rid, f.generation, 0)
        .await?;
    {
        let stores = f.app.stores.lock().unwrap();
        let store = &stores["real"];
        let lid = s(&f.request, "learner_id");
        store.fail_tutor_stream(&f.sid, &f.rid, "failed", "invalid_json")?;
        for index in 0..12 {
            let text = format!("历史报告 {index}");
            let event = store.record_education_event(
                lid,
                &f.sid,
                "learner_report",
                "learner_statement",
                &json!({"text":text}),
            )?;
            store.add_turn(
                &f.sid,
                &uid("request"),
                f.generation,
                "user",
                &text,
                "shown",
                &[s(&event, "event_id").to_owned()],
            )?;
        }
        let event = store.record_education_event(
            lid,
            &f.sid,
            "learner_report",
            "learner_statement",
            &json!({"text":"现在继续"}),
        )?;
        let context = build_context(
            store,
            &store.session(&f.sid)?,
            &store.learner(lid)?,
            "现在继续",
            s(&event, "event_id"),
            &Value::Null,
        )?;
        ensure!(
            !context["recent_delivered_dialogue"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["role"] == "assistant")
        );
        ensure!(context["task"]["last_delivered_hint"] == "first. ");
        ensure!(
            context["task"]["last_delivered_stream_phase"] == "failed"
                && context["task"]["last_delivered_reply_complete"] == false
        );
        ensure!(!context.to_string().contains("second."));
    }
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn cancellation_interrupts_segment_waiting_for_control_without_aborting_cleanup() -> Result<()>
{
    let f = Fixture::new()?;
    let mut decoder = SegmentDecoder::new(&f.request, f.generation as u64)?;
    let segment=decoder.push(format!("{}\n",json!({"schema_version":"1.0","type":"segment","request_id":f.rid,"sequence":0,"text":"unused","used_evidence_ids":[]})).as_bytes())?.remove(0);
    let (cancel, mut cancelled) = watch::channel(false);
    let guard = f.app.control.lock().await;
    let task = {
        let app = f.app.clone();
        let sid = f.sid.clone();
        let rid = f.rid.clone();
        tokio::spawn(async move {
            app.persist_segment("real", &sid, &rid, &segment, &mut cancelled)
                .await
        })
    };
    tokio::task::yield_now().await;
    cancel.send(true)?;
    ensure!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await??
            .is_err()
    );
    drop(guard);
    // Workstation cancellation signals, but does not abort or remove a running
    // task before that task has executed its cleanup stage.
    let (cancel, mut cancelled) = watch::channel(false);
    let (cleaned_send, cleaned_receive) = tokio::sync::oneshot::channel();
    let (finished_send, finished_receive) = tokio::sync::oneshot::channel();
    let task = {
        let app = f.app.clone();
        let rid = f.rid.clone();
        tokio::spawn(async move {
            let _ = cancelled.changed().await;
            let _ = cleaned_send.send(());
            let _guard = app.control.lock().await;
            app.running.lock().unwrap().remove(&rid);
            let _ = finished_send.send(());
        })
    };
    f.app.running.lock().unwrap().insert(
        f.rid.clone(),
        Running {
            sid: f.sid.clone(),
            _task: task,
            cancellation: cancel,
        },
    );
    let guard = f.app.control.lock().await;
    f.app.cancel().await?;
    tokio::time::timeout(Duration::from_secs(1), cleaned_receive).await??;
    ensure!(f.app.running.lock().unwrap().contains_key(&f.rid));
    drop(guard);
    tokio::time::timeout(Duration::from_secs(1), finished_receive).await??;
    f.app.close().await;
    Ok(())
}

#[tokio::test]
async fn real_loopback_poll_and_display_require_auth_origin_and_current_generation() -> Result<()> {
    let f = Fixture::new()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let routes = router(f.app.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    let base = format!("http://{address}");
    let path = format!("{base}/api/sessions/{}/streams/{}", f.sid, f.rid);
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()?;
    ensure!(client.get(&path).send().await?.status() == 401);
    ensure!(
        client
            .get(&path)
            .header("Cookie", "ai_tutor_local=synthetic-delivery-token")
            .header("Origin", "http://example.invalid")
            .send()
            .await?
            .status()
            == 403
    );
    let snapshot: Value = client
        .get(&path)
        .header("Cookie", "ai_tutor_local=synthetic-delivery-token")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    ensure!(
        snapshot["segments"].as_array().unwrap().len() == 2 && snapshot["shown_through"].is_null()
    );
    let acknowledgement: Value = client
        .post(format!("{path}/display"))
        .header("Cookie", "ai_tutor_local=synthetic-delivery-token")
        .json(&json!({"generation":f.generation,"sequence":0}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    ensure!(acknowledgement["shown_through"] == 0);
    f.app.stores.lock().unwrap()["real"].bump(&f.sid, "idle")?;
    ensure!(
        client
            .get(&path)
            .header("Cookie", "ai_tutor_local=synthetic-delivery-token")
            .send()
            .await?
            .status()
            == 400
    );
    ensure!(
        client
            .post(format!("{path}/display"))
            .header("Cookie", "ai_tutor_local=synthetic-delivery-token")
            .json(&json!({"generation":f.generation,"sequence":1}))
            .send()
            .await?
            .status()
            == 400
    );
    server.abort();
    f.app.close().await;
    Ok(())
}
