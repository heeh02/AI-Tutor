//! Bind an editable transcript to the exact session and language that recorded it.
//! The single capture owner still lives in AudioService; this is only its context.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RecordingBinding {
    namespace: String,
    learner_id: String,
    learner_epoch: i64,
    session_id: String,
    session_generation: i64,
    language: String,
    recording_generation: u64,
}

struct RecordingTicket<'a> {
    session_id: &'a str,
    recording_generation: u64,
    context: Option<(i64, &'a str)>,
}
impl<'a> RecordingTicket<'a> {
    fn parse(data: &'a Value) -> Result<Self> {
        allowed_keys(
            data,
            &[
                "session_id",
                "session_generation",
                "language",
                "recording_generation",
            ],
        )?;
        let context = if data.get("session_generation").is_some() || data.get("language").is_some()
        {
            let generation = data["session_generation"]
                .as_i64()
                .filter(|v| *v >= 0)
                .ok_or_else(|| anyhow!("缺少有效会话代次"))?;
            let language = text(data, "language", 2, 5)?;
            ensure!(
                ["zh", "en", "zh-CN", "en-US"].contains(&language),
                "录音语言无效"
            );
            Some((generation, language))
        } else {
            // Legacy manual clients still send the audio token, but the server
            // must have a registered start binding; never infer a fresh one here.
            None
        };
        Ok(Self {
            session_id: text(data, "session_id", 1, 80)?,
            recording_generation: data["recording_generation"]
                .as_u64()
                .ok_or_else(|| anyhow!("缺少有效录音代次"))?,
            context,
        })
    }
    fn matches(&self, binding: &RecordingBinding) -> bool {
        self.session_id == binding.session_id
            && self.recording_generation == binding.recording_generation
            && self.context.is_none_or(|(generation, language)| {
                generation == binding.session_generation && language == binding.language
            })
    }
}

pub(super) fn expected_generation(data: &Value) -> Result<Option<i64>> {
    data.get("expected_generation")
        .map(|value| {
            value
                .as_i64()
                .filter(|v| *v >= 0)
                .ok_or_else(|| anyhow!("会话代次无效"))
        })
        .transpose()
}
pub(super) fn check_expected_generation(session: &Value, expected: Option<i64>) -> Result<()> {
    ensure!(
        expected.is_none_or(|generation| session["generation"] == generation),
        "会话已变化，请核对当前内容后重试"
    );
    Ok(())
}

impl RecordingBinding {
    fn stamp(&self, value: &mut Value) {
        value["session_id"] = json!(self.session_id);
        value["session_generation"] = json!(self.session_generation);
        value["language"] = json!(self.language);
        value["recording_generation"] = json!(self.recording_generation);
    }
}

impl Workstation {
    /// The barge-in owner already has an open, promoted input. Caller holds
    /// workstation control and the matching lease while committing this ticket.
    pub(super) fn adopt_barge_in_recording(
        &self,
        sid: &str,
        old_generation: i64,
        token: u64,
        input: &crate::audio::InputBinding,
        response: &mut Value,
    ) -> Result<()> {
        let (namespace, session) = self.writable(sid)?;
        ensure!(
            session["generation"] == old_generation,
            "打断录音的会话已变化"
        );
        ensure!(
            self.audio.status()["recording_generation"] == token,
            "打断录音已取消"
        );
        self.audio
            .validate_input_selection(&crate::audio::InputSelection::Bound(input.clone()))?;
        ensure!(
            self.recording.lock().unwrap().is_none(),
            "已有另一段绑定录音"
        );
        let new_generation = self.stores.lock().unwrap()[&namespace].bump(sid, "idle")?;
        ensure!(new_generation == old_generation + 1, "打断录音代次不连续");
        let binding = self.recording_context(sid, token)?;
        self.recording_is_current(&binding)?;
        binding.stamp(response);
        *self.recording.lock().unwrap() = Some(binding);
        Ok(())
    }

    fn recording_context(&self, sid: &str, recording_generation: u64) -> Result<RecordingBinding> {
        let (namespace, session) = self.writable(sid)?;
        let learner_id = s(&session, "learner_id").to_owned();
        let learner = self.stores.lock().unwrap()[&namespace].learner(&learner_id)?;
        Ok(RecordingBinding {
            namespace,
            learner_id,
            learner_epoch: learner["epoch"]
                .as_i64()
                .ok_or_else(|| anyhow!("档案版本无效"))?,
            session_id: sid.into(),
            session_generation: session["generation"]
                .as_i64()
                .ok_or_else(|| anyhow!("会话代次无效"))?,
            language: s(&learner, "language").to_owned(),
            recording_generation,
        })
    }
    fn recording_is_current(&self, binding: &RecordingBinding) -> Result<()> {
        ensure!(!self.shutting_down.load(Ordering::SeqCst), "工作台正在关闭");
        ensure!(
            self.audio.status()["recording_generation"] == binding.recording_generation,
            "录音请求已过期，请重新录音"
        );
        ensure!(
            self.recording_context(&binding.session_id, binding.recording_generation)? == *binding,
            "录音所用会话或语言已变化，请重新录音"
        );
        Ok(())
    }
    fn recording_for_ticket(&self, ticket: &RecordingTicket<'_>) -> Result<RecordingBinding> {
        self.recording
            .lock()
            .unwrap()
            .as_ref()
            .filter(|binding| ticket.matches(binding))
            .cloned()
            .ok_or_else(|| anyhow!("录音请求已过期，请重新录音"))
    }
    pub(super) async fn start_bound_recording(&self, data: &Value) -> Result<Value> {
        allowed_keys(
            data,
            &[
                "session_id",
                "expected_generation",
                "device_id",
                "device_key",
                "expected_input_topology_revision",
                "endpoint_mode",
                "voice_loop",
            ],
        )?;
        let sid = text(data, "session_id", 1, 80)?;
        let expected = expected_generation(data)?;
        let loop_tag = voice_loop::LoopTag::parse(data.get("voice_loop"))?;
        let mode: crate::audio::EndpointMode = data
            .get("endpoint_mode")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()?
            .unwrap_or_default();
        let device = data
            .get("device_id")
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|v| usize::try_from(v).ok())
                    .ok_or_else(|| anyhow!("录音设备编号无效"))
            })
            .transpose()?;
        let selection = match (
            data.get("device_key"),
            data.get("expected_input_topology_revision"),
        ) {
            (None, None) => crate::audio::InputSelection::Legacy(device),
            (Some(_), Some(revision)) => {
                ensure!(device.is_none(), "稳定设备绑定不能混用旧设备编号");
                let key = text(data, "device_key", 1, 96)?;
                ensure!(key.is_ascii(), "麦克风设备标识无效");
                let topology_revision = revision
                    .as_u64()
                    .filter(|v| *v > 0)
                    .ok_or_else(|| anyhow!("麦克风设备版本无效"))?;
                crate::audio::InputSelection::Bound(crate::audio::InputBinding {
                    device_key: key.to_owned(),
                    topology_revision,
                })
            }
            _ => bail!("麦克风设备标识和版本必须同时提供"),
        };
        let _guard = self.control.lock().await;
        ensure!(!self.shutting_down.load(Ordering::SeqCst), "工作台正在关闭");
        let (_, session) = self.writable(sid)?;
        check_expected_generation(&session, expected)?;
        // A stale selection cannot cancel a teaching turn or reserve audio.
        self.audio.validate_endpoint_selection(&selection, mode)?;
        let status = self.audio.status();
        ensure!(status["recording"] != true, "已有录音，请先松开或停止");
        ensure!(
            status["capture_pending_result"] != true,
            "上一段录音尚未处理，请先停止或丢弃"
        );
        ensure!(
            status["state"] != "transcribing",
            "本地转写尚未结束，请稍候"
        );
        // This is the only cancellation/bump performed by a new recording.
        let mut loop_operation = None;
        if let Some(tag) = &loop_tag {
            ensure!(expected.is_some(), "语音轮换缺少会话代次");
            self.begin_loop_recording(tag, sid, &selection, mode)?;
            loop_operation = Some(self.loop_operation(tag));
            self.cancel_turn().await?;
            self.loop_generation(tag, sid)?;
        } else {
            self.cancel().await?;
        }
        *self.recording.lock().unwrap() = None;
        let mut response = self
            .audio
            .start_recording_with_endpoint_reserved(selection.clone(), mode, |token| {
                loop_tag
                    .as_ref()
                    .is_none_or(|tag| self.loop_recording_reserved(tag, token))
            })
            .await?;
        let token = response["recording_generation"]
            .as_u64()
            .ok_or_else(|| anyhow!("录音回执缺少代次"))?;
        let binding = self.recording_context(sid, token)?;
        // Shutdown invalidates audio synchronously before waiting for control.
        // Never publish a late Start receipt after that invalidation.
        if let Err(error) = self
            .recording_is_current(&binding)
            .and_then(|_| self.audio.validate_input_selection(&selection))
        {
            let _ = self.audio.discard_recording_expected(token);
            return Err(error);
        }
        binding.stamp(&mut response);
        if let Some(tag) = &loop_tag {
            if let Err(error) = self.loop_recording_started(tag, token) {
                let _ = self.audio.discard_recording_expected(token);
                return Err(error);
            }
            response["voice_loop"] = self.voice_loop_echo(tag)?;
        }
        *self.recording.lock().unwrap() = Some(binding);
        if let Some(operation) = &mut loop_operation {
            operation.complete();
        }
        self.notify("audio.changed");
        Ok(response)
    }
    pub(super) async fn discard_bound_recording(&self, data: &Value) -> Result<Value> {
        let ticket = RecordingTicket::parse(data)?;
        let _guard = self.control.lock().await;
        let binding = self.recording_for_ticket(&ticket)?;
        // A delayed Start acknowledgement may clean up its own capture even
        // after the teaching generation changed. Its audio CAS cannot touch a
        // newer capture, and cleanup never returns a transcript.
        self.audio
            .discard_recording_expected(binding.recording_generation)?;
        self.pause_voice_loop_recording(binding.recording_generation, "discarded");
        *self.recording.lock().unwrap() = None;
        let mut response = json!({"ok":true});
        binding.stamp(&mut response);
        Ok(response)
    }
    pub(super) async fn stop_bound_recording(&self, data: &Value) -> Result<Value> {
        let ticket = RecordingTicket::parse(data)?;
        let binding = {
            let _guard = self.control.lock().await;
            let binding = self.recording_for_ticket(&ticket)?;
            self.recording_is_current(&binding)?;
            self.loop_recording_finalizing(binding.recording_generation);
            binding
        };
        // No workstation/database lock is held while CPU/service ASR runs.
        let response = match self
            .audio
            .stop_recording_expected(&binding.language, binding.recording_generation)
            .await
        {
            Ok(response) => response,
            Err(error) => {
                self.pause_voice_loop_recording(
                    binding.recording_generation,
                    "transcription_failed",
                );
                return Err(error);
            }
        };
        let _guard = self.control.lock().await;
        let result = self.finish_bound_recording(&binding, response);
        if let Ok(response) = &result {
            self.loop_recording_final(binding.recording_generation, response);
        } else {
            self.pause_voice_loop_recording(binding.recording_generation, "transcript_stale");
        }
        result
    }
    fn finish_bound_recording(
        &self,
        binding: &RecordingBinding,
        mut response: Value,
    ) -> Result<Value> {
        self.recording_is_current(binding)?;
        let mut active = self.recording.lock().unwrap();
        ensure!(
            active.as_ref() == Some(binding),
            "转写请求已过期，请重新录音"
        );
        binding.stamp(&mut response);
        *active = None;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::recording_test::TestRecordingOwner;
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };

    async fn pump<F: Future>(owner: &mut TestRecordingOwner, future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        tokio::time::timeout(
            Duration::from_secs(3),
            poll_fn(|cx| {
                if let Poll::Ready(value) = future.as_mut().poll(cx) {
                    return Poll::Ready(value);
                }
                owner.pump();
                cx.waker().wake_by_ref();
                Poll::Pending
            }),
        )
        .await
        .expect("fake owner operation must finish without a device")
    }
    async fn fixture() -> Result<(
        tempfile::TempDir,
        Arc<Workstation>,
        String,
        String,
        TestRecordingOwner,
    )> {
        let dir = tempfile::tempdir()?;
        let mut app = Workstation::new(dir.path().into(), "recording-fixture".into())?;
        app.audio.close().await;
        let (audio, owner) = AudioService::recording_test_channel(dir.path().into());
        Arc::get_mut(&mut app).unwrap().audio = Arc::new(audio);
        let (sid, lid) = {
            let stores = app.stores.lock().unwrap();
            let store = &stores["simulator"];
            let learner = store.create_learner("recording fixture", "zh", "guided")?;
            let lid = s(&learner, "learner_id").to_owned();
            (
                s(&store.create_session(&lid)?, "session_id").to_owned(),
                lid,
            )
        };
        *app.current.lock().unwrap() = Some(("simulator".into(), sid.clone()));
        Ok((dir, app, sid, lid, owner))
    }
    async fn start(app: &Workstation, sid: &str, owner: &mut TestRecordingOwner) -> Result<Value> {
        let generation =
            app.stores.lock().unwrap()["simulator"].session(sid)?["generation"].clone();
        let response = pump(
            owner,
            app.start_bound_recording(&json!({"session_id":sid,
            "expected_generation":generation})),
        )
        .await?;
        Ok(
            json!({"session_id":response["session_id"],"session_generation":response["session_generation"],
            "language":response["language"],"recording_generation":response["recording_generation"]}),
        )
    }

    #[tokio::test]
    async fn recording_start_binds_actual_context_and_bumps_only_once() -> Result<()> {
        let (_dir, app, sid, _, mut owner) = fixture().await?;
        let ticket = start(&app, &sid, &mut owner).await?;
        assert_eq!(ticket["session_generation"], 1);
        assert_eq!(ticket["language"], "zh");
        assert_eq!(ticket["recording_generation"], 1);
        assert_eq!(owner.starts, 1);
        let before = app.audio.status();
        assert!(
            app.start_bound_recording(&json!({"session_id":sid,"expected_generation":1}))
                .await
                .is_err()
        );
        assert_eq!(
            app.stores.lock().unwrap()["simulator"].session(&sid)?["generation"],
            1
        );
        assert_eq!(
            app.audio.status()["recording_generation"],
            before["recording_generation"]
        );
        pump(&mut owner, app.close()).await;
        Ok(())
    }

    #[tokio::test]
    async fn invalid_stable_device_binding_never_cancels_teaching_or_reserves_audio() -> Result<()>
    {
        let (_dir, app, sid, _, mut owner) = fixture().await?;
        let before = app.audio.status()["recording_generation"].clone();
        for fields in [
            json!({"device_key":"foreign"}),
            json!({"expected_input_topology_revision":1}),
            json!({"device_key":"foreign","expected_input_topology_revision":1}),
            json!({"device_key":"foreign","expected_input_topology_revision":1,"device_id":0}),
            json!({"device_key":"foreign","expected_input_topology_revision":null}),
        ] {
            let mut request = fields;
            request["session_id"] = json!(sid);
            request["expected_generation"] = json!(0);
            assert!(app.start_bound_recording(&request).await.is_err());
            assert_eq!(
                app.stores.lock().unwrap()["simulator"].session(&sid)?["generation"],
                0
            );
            assert_eq!(app.audio.status()["recording_generation"], before);
            assert!(app.recording.lock().unwrap().is_none());
        }
        owner.pump();
        assert_eq!(owner.starts, 0);
        assert_eq!(owner.discards, 0);
        pump(&mut owner, app.close()).await;
        Ok(())
    }

    #[tokio::test]
    async fn stale_start_and_text_submit_do_not_cancel_or_change_input_debounce() -> Result<()> {
        let (_dir, app, sid, _, mut owner) = fixture().await?;
        app.stores.lock().unwrap()["simulator"].bump(&sid, "idle")?;
        let before = app.audio.status();
        let last_input = *app.last_input.lock().unwrap();
        for invalid in [json!(0), json!(-1), json!(null), json!("1")] {
            let request = json!({"session_id":sid,"expected_generation":invalid});
            assert!(app.start_bound_recording(&request).await.is_err());
            assert!(
                dispatch(
                    app.clone(),
                    &Method::POST,
                    &format!("/api/sessions/{sid}/turns"),
                    &json!({"text":"synthetic text","expected_generation":invalid})
                )
                .await
                .is_err()
            );
        }
        assert_eq!(
            app.stores.lock().unwrap()["simulator"].session(&sid)?["generation"],
            1
        );
        assert_eq!(
            app.audio.status()["recording_generation"],
            before["recording_generation"]
        );
        assert_eq!(*app.last_input.lock().unwrap(), last_input);
        owner.pump();
        assert_eq!((owner.starts, owner.stops, owner.discards), (0, 0, 0));
        pump(&mut owner, app.close()).await;
        Ok(())
    }

    #[tokio::test]
    async fn recording_partial_or_mismatched_context_never_consumes_capture() -> Result<()> {
        let (_dir, app, sid, _, mut owner) = fixture().await?;
        let ticket = start(&app, &sid, &mut owner).await?;
        for (field, replacement) in [
            ("session_generation", Some(json!(0))),
            ("language", Some(json!("en"))),
            ("language", None),
            ("session_generation", None),
            ("session_generation", Some(Value::Null)),
            ("session_id", Some(json!("old-session"))),
        ] {
            let mut invalid = ticket.clone();
            if let Some(value) = replacement {
                invalid[field] = value;
            } else {
                invalid.as_object_mut().unwrap().remove(field);
            }
            assert!(app.stop_bound_recording(&invalid).await.is_err());
            assert!(app.discard_bound_recording(&invalid).await.is_err());
        }
        owner.pump();
        assert_eq!((owner.stops, owner.discards), (0, 0));
        assert_eq!(app.audio.status()["recording"], true);
        let response = pump(&mut owner, app.stop_bound_recording(&ticket)).await?;
        for field in [
            "session_id",
            "session_generation",
            "language",
            "recording_generation",
        ] {
            assert_eq!(response[field], ticket[field]);
        }
        assert_eq!(response["text"], ""); // Quiet fake capture, no recognizer was loaded.
        pump(&mut owner, app.close()).await;
        Ok(())
    }

    #[tokio::test]
    async fn legacy_recording_stop_uses_registered_context_without_inventing_it() -> Result<()> {
        let (_dir, app, sid, _, mut owner) = fixture().await?;
        assert!(
            app.stop_bound_recording(&json!({"session_id":sid,"recording_generation":0}))
                .await
                .is_err()
        );
        let ticket = start(&app, &sid, &mut owner).await?;
        let legacy =
            json!({"session_id":sid,"recording_generation":ticket["recording_generation"]});
        let response = pump(&mut owner, app.stop_bound_recording(&legacy)).await?;
        assert_eq!(response["session_generation"], 1);
        assert_eq!(response["language"], "zh");
        pump(&mut owner, app.close()).await;
        Ok(())
    }

    #[tokio::test]
    async fn old_recording_controls_and_final_cannot_consume_or_clear_new_binding() -> Result<()> {
        let (_dir, app, sid, _, mut owner) = fixture().await?;
        let old_ticket = start(&app, &sid, &mut owner).await?;
        let old_binding = app.recording.lock().unwrap().clone().unwrap();
        app.discard_bound_recording(&old_ticket).await?;
        owner.pump();
        let new_ticket = start(&app, &sid, &mut owner).await?;
        let discards = owner.discards;
        assert!(app.stop_bound_recording(&old_ticket).await.is_err());
        assert!(app.discard_bound_recording(&old_ticket).await.is_err());
        assert!(
            app.finish_bound_recording(&old_binding, json!({"text":"late synthetic transcript"}))
                .is_err()
        );
        owner.pump();
        assert_eq!(owner.discards, discards);
        assert_eq!(owner.stops, 0);
        assert_eq!(app.audio.status()["recording"], true);
        assert_eq!(
            app.recording
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .recording_generation,
            new_ticket["recording_generation"].as_u64().unwrap()
        );
        pump(&mut owner, app.stop_bound_recording(&new_ticket)).await?;
        pump(&mut owner, app.close()).await;
        Ok(())
    }

    #[tokio::test]
    async fn final_rechecks_session_language_epoch_and_owner_after_unlocked_asr() -> Result<()> {
        for changed in ["generation", "language", "epoch", "owner"] {
            let (_dir, app, sid, lid, mut owner) = fixture().await?;
            let ticket = start(&app, &sid, &mut owner).await?;
            owner.hold_stop = true;
            let stopping = tokio::spawn({
                let app = app.clone();
                let ticket = ticket.clone();
                async move { app.stop_bound_recording(&ticket).await }
            });
            tokio::time::timeout(Duration::from_secs(3), async {
                while owner.stops == 0 {
                    owner.pump();
                    tokio::task::yield_now().await;
                }
            })
            .await?;
            {
                // Acquiring this while ASR is paused proves the control lock
                // is released, so settings/cancel can invalidate its result.
                let _guard =
                    tokio::time::timeout(Duration::from_secs(1), app.control.lock()).await?;
                let stores = app.stores.lock().unwrap();
                let store = &stores["simulator"];
                match changed {
                    "generation" => {
                        store.bump(&sid, "idle")?;
                    }
                    "language" => {
                        store.settings(&lid, "en", "guided")?;
                    }
                    "epoch" => {
                        store.db.execute(
                            "UPDATE learners SET epoch=epoch+1 WHERE learner_id=?",
                            [&lid],
                        )?;
                    }
                    "owner" => {
                        *app.current.lock().unwrap() = None;
                    }
                    _ => unreachable!(),
                }
            }
            owner.release_stop();
            assert!(
                stopping.await?.is_err(),
                "must reject stale final after {changed} changed"
            );
            assert_eq!(
                app.audio.status()["recording_generation"],
                ticket["recording_generation"]
            );
            pump(&mut owner, app.close()).await;
        }
        Ok(())
    }

    #[tokio::test]
    async fn invalidated_binding_does_not_block_next_recording_start() -> Result<()> {
        let (_dir, app, sid, _, mut owner) = fixture().await?;
        let old_ticket = start(&app, &sid, &mut owner).await?;
        app.stores.lock().unwrap()["simulator"].bump(&sid, "idle")?;
        app.audio.discard_recording();
        owner.pump();
        let new_ticket = start(&app, &sid, &mut owner).await?;
        assert_ne!(
            old_ticket["recording_generation"],
            new_ticket["recording_generation"]
        );
        assert_eq!(new_ticket["session_generation"], 3);
        assert_eq!(owner.starts, 2);
        pump(&mut owner, app.close()).await;
        Ok(())
    }

    #[tokio::test]
    async fn exact_discard_can_clean_its_capture_after_teaching_generation_changed() -> Result<()> {
        let (_dir, app, sid, _, mut owner) = fixture().await?;
        let ticket = start(&app, &sid, &mut owner).await?;
        app.stores.lock().unwrap()["simulator"].bump(&sid, "idle")?;
        assert!(app.stop_bound_recording(&ticket).await.is_err());
        app.discard_bound_recording(&ticket).await?;
        owner.pump();
        assert_eq!(app.audio.status()["recording"], false);
        assert!(app.recording.lock().unwrap().is_none());
        pump(&mut owner, app.close()).await;
        Ok(())
    }
}
