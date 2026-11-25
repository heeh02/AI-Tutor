//! Complete permission/capture/ASR/submit/display/speech/rearm integration.
//! Only I/O owners and provider transport are fake; no lease phase is assigned.
//! This verifies protocol and mutual exclusion, not VAD/acoustic/model quality.
use super::*;
use crate::{
    audio::{
        InputBinding,
        voice_loop_test::{AsrFixture, VoiceLoopOwner},
    },
    kimi::ScriptedSegmentRequest,
};
use std::{
    future::{Future, poll_fn},
    task::Poll,
};
use tokio::sync::mpsc;

async fn pump<F: Future>(owner: &mut VoiceLoopOwner, future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    tokio::time::timeout(
        Duration::from_secs(10),
        poll_fn(|cx| {
            owner.pump();
            let result = future.as_mut().poll(cx);
            if result.is_pending() {
                cx.waker().wake_by_ref();
            }
            result
        }),
    )
    .await
    .expect("synthetic owner operation timed out")
}

struct Fixture {
    _dir: tempfile::TempDir,
    app: Arc<Workstation>,
    owner: VoiceLoopOwner,
    input: InputBinding,
    asr: AsrFixture,
    provider: mpsc::Receiver<ScriptedSegmentRequest>,
    sid: String,
    tag: Value,
}
impl Fixture {
    async fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("runtime"))?;
        std::fs::write(
            dir.path().join("runtime/tutor-delivery.json"),
            r#"{"protocol_version":1,"mode":"segments","segment_speech":"after_display"}"#,
        )?;
        let mut app = Workstation::new(dir.path().into(), "synthetic-loop-integration".into())?;
        // Replace the idle native command owners before any API request. No
        // native input is enumerated, opened, or configured by this fixture.
        app.audio.close().await;
        let (audio, owner, input) = AudioService::voice_loop_test_channel(dir.path().into());
        let asr = AsrFixture::new().await;
        audio.install_streaming_asr(asr.managed_client());
        Arc::get_mut(&mut app).unwrap().audio = Arc::new(audio);
        let provider = app.provider.scripted_segments_for_test();
        let sid = {
            let stores = app.stores.lock().unwrap();
            let store = &stores["simulator"];
            let learner = store.create_learner("Synthetic complete voice loop", "en", "guided")?;
            s(
                &store.create_session(s(&learner, "learner_id"))?,
                "session_id",
            )
            .to_owned()
        };
        *app.current.lock().unwrap() = Some(("simulator".into(), sid.clone()));
        Ok(Self {
            _dir: dir,
            app,
            owner,
            input,
            asr,
            provider,
            sid,
            tag: Value::Null,
        })
    }
    fn generation(&self) -> Result<Value> {
        Ok(self.app.stores.lock().unwrap()["simulator"].session(&self.sid)?["generation"].clone())
    }
    async fn call(&mut self, method: Method, path: String, body: Value) -> Result<Value> {
        let app = self.app.clone();
        pump(&mut self.owner, dispatch(app, &method, &path, &body)).await
    }
    async fn enable(&mut self) -> Result<()> {
        let enabled = self.call(Method::POST, format!("/api/sessions/{}/voice-loop/enable", self.sid),
            json!({"client_loop_id":"synthetic-complete-loop", "expected_generation":self.generation()?,
                "language":"en", "device_key":self.input.device_key,
                "expected_input_topology_revision":self.input.topology_revision})).await?;
        assert_eq!(enabled["phase"], "armed");
        self.tag =
            json!({"client_loop_id":enabled["client_loop_id"],"loop_epoch":enabled["loop_epoch"]});
        assert_eq!(
            self.owner.opened, 0,
            "enable is not capture permission execution"
        );
        Ok(())
    }
    fn start_body(&self) -> Result<Value> {
        Ok(
            json!({"session_id":self.sid,"expected_generation":self.generation()?,
            "device_key":self.input.device_key,"expected_input_topology_revision":self.input.topology_revision,
            "endpoint_mode":"voice_endpoint_v1","voice_loop":self.tag}),
        )
    }
    async fn start(&mut self) -> Result<Value> {
        self.call(
            Method::POST,
            "/api/audio/record/start".into(),
            self.start_body()?,
        )
        .await
    }
    async fn heartbeat(&mut self) -> Result<Value> {
        self.call(
            Method::POST,
            format!(
                "/api/sessions/{}/voice-loop/{}/heartbeat",
                self.sid,
                s(&self.tag, "client_loop_id")
            ),
            json!({"loop_epoch":self.tag["loop_epoch"]}),
        )
        .await
    }
    async fn pause(&mut self) -> Result<Value> {
        self.call(
            Method::POST,
            format!(
                "/api/sessions/{}/voice-loop/{}/stop",
                self.sid,
                s(&self.tag, "client_loop_id")
            ),
            json!({"loop_epoch":self.tag["loop_epoch"],"reason":"synthetic_explicit_stop"}),
        )
        .await
    }
    async fn snapshot(&mut self, reply: &Value) -> Result<Value> {
        self.call(
            Method::GET,
            format!(
                "/api/sessions/{}/streams/{}",
                self.sid,
                s(reply, "request_id")
            ),
            Value::Null,
        )
        .await
    }
    async fn display(&mut self, reply: &Value, sequence: usize) -> Result<Value> {
        self.call(
            Method::POST,
            format!(
                "/api/sessions/{}/streams/{}/display",
                self.sid,
                s(reply, "request_id")
            ),
            json!({"generation":reply["generation"],"sequence":sequence}),
        )
        .await
    }
    async fn wait_for(
        &mut self,
        reply: &Value,
        predicate: impl Fn(&Value) -> bool,
    ) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let snapshot = self.snapshot(reply).await?;
            if predicate(&snapshot) {
                return Ok(snapshot);
            }
            ensure!(
                Instant::now() < deadline,
                "stream did not reach expected synthetic state: {snapshot}"
            );
            self.heartbeat().await?;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    async fn wait_for_speaker(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.owner.is_speaking() {
            self.owner.pump();
            ensure!(
                Instant::now() < deadline,
                "synthetic speaker did not receive segment"
            );
            self.heartbeat().await?;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(!self.owner.input_is_open());
        Ok(())
    }
    async fn rearm(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut observed_cooldown = false;
        loop {
            let status = self.heartbeat().await?;
            observed_cooldown |= status["phase"] == "cooldown";
            if status["rearm_ready"] == true {
                assert_eq!(status["phase"], "armed");
                assert!(
                    observed_cooldown,
                    "rearm must pass the real continuous tail interval"
                );
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "complete reply did not rearm: {status}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    async fn await_input_debounce(&mut self) -> Result<()> {
        // The production clock/debounce is unchanged. Keep the real lease alive
        // while respecting it, rather than assigning a synthetic last_input.
        while self.app.last_input.lock().unwrap().elapsed() <= Duration::from_millis(1550) {
            self.heartbeat().await?;
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        Ok(())
    }
    fn user_turns(&self) -> Result<Vec<Value>> {
        let session = self.app.stores.lock().unwrap()["simulator"].session(&self.sid)?;
        Ok(session["turns"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|turn| turn["role"] == "user")
            .cloned()
            .collect())
    }
    async fn close(&mut self) {
        let app = self.app.clone();
        pump(&mut self.owner, app.close()).await;
    }
}

fn ticket(start: &Value) -> Value {
    json!({"session_id":start["session_id"],"session_generation":start["session_generation"],
        "language":start["language"],"recording_generation":start["recording_generation"]})
}
fn scripted_reply(call: ScriptedSegmentRequest) {
    let rid = s(&call.request, "request_id");
    assert_eq!(call.request["generation"], call.generation);
    let texts = [
        "Compare one condition at a time. ",
        "Record what you observe.",
    ];
    let mut records = vec![];
    for (sequence, text) in texts.iter().enumerate() {
        records.push(
            json!({"schema_version":"1.0","type":"segment","request_id":rid,
            "sequence":sequence,"text":text,"used_evidence_ids":[]}),
        );
    }
    records.push(
        json!({"schema_version":"1.0","type":"final","request_id":rid,"segment_count":2,
        "reply":{"schema_version":"1.0","request_id":rid,"reply_text":texts.concat(),
            "teaching_move":"give_hint","task_update_candidates":[],"memory_candidates":[],
            "used_evidence_ids":[],"needs_clarification":false}}),
    );
    call.response
        .send(records)
        .expect("registered real provider consumer remains live");
}

#[tokio::test]
async fn dispatch_two_voice_rounds_are_exclusive_and_submit_each_bound_final_once() -> Result<()> {
    let mut f = Fixture::new().await?;
    f.enable().await?;
    let mut previous_ticket = None;
    for round in 1..=2usize {
        let start = f.start().await?;
        let bound = ticket(&start);
        assert_eq!(start["voice_loop"]["phase"], "listening");
        assert!(f.owner.input_is_open());
        assert!(!f.owner.is_speaking());
        assert_eq!(f.owner.opened, round);
        assert!(
            f.start().await.is_err(),
            "duplicate Start cannot open another capture"
        );
        assert_eq!(f.heartbeat().await?["phase"], "listening");
        if let Some(old) = previous_ticket.take() {
            let token = f.app.audio.status()["recording_generation"].clone();
            assert!(
                f.call(Method::POST, "/api/audio/record/discard".into(), old)
                    .await
                    .is_err()
            );
            assert_eq!(f.app.audio.status()["recording_generation"], token);
            assert!(f.owner.input_is_open());
        }
        f.owner.speech_end();
        assert!(!f.owner.input_is_open());
        let final_text = f
            .call(Method::POST, "/api/audio/record/stop".into(), bound.clone())
            .await?;
        assert_eq!(final_text["text"], "最终 editable text");
        assert_eq!(final_text["recording_endpoint"]["reason"], "speech_end");
        assert_eq!(final_text["recording_endpoint"]["input_closed"], true);
        assert_eq!(f.heartbeat().await?["phase"], "ready_to_submit");
        assert!(
            f.call(Method::POST, "/api/audio/record/stop".into(), bound.clone())
                .await
                .is_err()
        );
        f.await_input_debounce().await?;
        let submit = json!({"text":final_text["text"],"expected_generation":final_text["session_generation"],
            "voice_loop":f.tag,"recording_generation":bound["recording_generation"],"auto_speak":true,
            "client_submission_id":format!("synthetic-voice-round-{round}")});
        let reply = f
            .call(
                Method::POST,
                format!("/api/sessions/{}/turns", f.sid),
                submit.clone(),
            )
            .await?;
        assert_eq!(reply["voice_loop"]["phase"], "reply");
        assert!(
            f.call(
                Method::POST,
                format!("/api/sessions/{}/turns", f.sid),
                submit
            )
            .await
            .is_err()
        );
        assert_eq!(f.user_turns()?.len(), round);
        assert_eq!(
            f.user_turns()?
                .iter()
                .filter(|turn| turn["request_id"] == reply["request_id"])
                .count(),
            1
        );
        let call = pump(&mut f.owner, f.provider.recv())
            .await
            .expect("one registered provider request");
        assert_eq!(call.request["request_id"], reply["request_id"]);
        scripted_reply(call);
        let final_snapshot = f.wait_for(&reply, |s| s["final_validated"] == true).await?;
        assert_eq!(final_snapshot["speech_complete"], false);
        assert!(!f.owner.is_speaking(), "speech waits for display ACK");
        assert_eq!(f.heartbeat().await?["rearm_ready"], false);
        assert!(f.start().await.is_err(), "text final cannot permit capture");
        f.display(&reply, 0).await?;
        f.wait_for_speaker().await?;
        let all_displayed = f.display(&reply, 1).await?;
        assert_eq!(all_displayed["phase"], "completed");
        assert_eq!(all_displayed["speech_complete"], false);
        assert_eq!(f.heartbeat().await?["rearm_ready"], false);
        assert!(
            f.start().await.is_err(),
            "complete text cannot overlap active output"
        );
        f.owner.complete_speech();
        f.wait_for_speaker().await?;
        assert_eq!(f.owner.speeches, round * 2);
        assert_eq!(f.heartbeat().await?["rearm_ready"], false);
        f.owner.complete_speech();
        f.wait_for(&reply, |s| s["speech_complete"] == true).await?;
        f.rearm().await?;
        assert_eq!(f.owner.opened, round, "heartbeat never opens input itself");
        assert_eq!(f.asr.request_counts(), (round, round));
        assert_eq!(f.app.provider.status()["metrics"]["requests"], round);
        assert!(
            f.provider.try_recv().is_err(),
            "duplicate Send must not invoke provider twice"
        );
        previous_ticket = Some(bound);
    }
    assert_eq!(
        (
            f.owner.opened,
            f.owner.stops,
            f.owner.speeches,
            f.owner.completed_speeches
        ),
        (2, 2, 4, 4)
    );
    assert_eq!(f.user_turns()?.len(), 2);
    assert_eq!(f.pause().await?["phase"], "paused");
    assert!(f.start().await.is_err());
    assert_eq!(f.owner.opened, 2);
    f.close().await;
    Ok(())
}

#[tokio::test]
async fn dispatch_loop_stop_after_reserved_token_prevents_late_capture_open() -> Result<()> {
    let mut f = Fixture::new().await?;
    f.enable().await?;
    f.owner.hold_start = true;
    let body = f.start_body()?;
    let app = f.app.clone();
    let mut start = std::pin::pin!(dispatch(
        app,
        &Method::POST,
        "/api/audio/record/start",
        &body
    ));
    tokio::time::timeout(
        Duration::from_secs(3),
        poll_fn(|cx| {
            assert!(
                start.as_mut().poll(cx).is_pending(),
                "Start must await its fake input owner"
            );
            f.owner.pump();
            if f.owner.has_held_start() {
                Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }),
    )
    .await?;
    assert_eq!(f.heartbeat().await?["phase"], "starting");
    let reserved = f.app.audio.status()["recording_generation"]
        .as_u64()
        .unwrap();
    assert_eq!(f.pause().await?["phase"], "paused");
    assert_eq!(f.app.audio.status()["recording_generation"], reserved + 1);
    assert_eq!(f.owner.opened, 0);
    f.owner.release_start();
    assert!(pump(&mut f.owner, start.as_mut()).await.is_err());
    assert_eq!(f.owner.start_requests, 1);
    assert_eq!(
        f.owner.opened, 0,
        "late owner request cannot open revoked capture"
    );
    assert!(!f.owner.input_is_open());
    assert!(f.start().await.is_err());
    assert_eq!(f.owner.start_requests, 1);
    assert!(f.user_turns()?.is_empty());
    assert_eq!(f.app.provider.status()["metrics"]["requests"], 0);
    assert!(f.provider.try_recv().is_err());
    assert_eq!(
        f.asr.request_counts().1,
        0,
        "cancelled start cannot produce a final"
    );
    f.close().await;
    Ok(())
}
