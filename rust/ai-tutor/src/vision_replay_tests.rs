//! Deterministic detection-trajectory replay: real ByteTrack and production
//! ambiguity/selection reducers, without opening a camera or issuing PTZ I/O.
use super::*;
use edgefirst_tracker::ByteTrack;

struct Replay {
    tracker: ByteTrack<FaceDetection>,
    labels: HashMap<Uuid, u32>,
    serial: u32,
    state: State,
    base: Instant,
    frame_id: u64,
    epoch: String,
}
impl Replay {
    fn new() -> Self {
        Self {
            tracker: ByteTrackBuilder::new()
                .track_high_conf(0.4)
                .track_iou(0.25)
                .track_update(0.25)
                .track_extra_lifespan(600_000_000)
                .build::<FaceDetection>(),
            labels: HashMap::new(),
            serial: 0,
            state: State::new(),
            base: Instant::now(),
            frame_id: 0,
            epoch: "fixture-camera-A".into(),
        }
    }
    fn step(&mut self, ms: u64, boxes: &[[f32; 4]]) -> Value {
        let detections: Vec<_> = boxes
            .iter()
            .map(|bbox| FaceDetection {
                bbox: *bbox,
                score: 0.9,
            })
            .collect();
        let infos = self.tracker.update(&detections, (ms + 1) * 1_000_000);
        let flags = ambiguous_detections(&detections);
        let (tracks, keys) = tracked_faces(
            &self.epoch,
            &detections,
            &infos,
            &flags,
            &mut self.labels,
            &mut self.serial,
        );
        self.frame_id += 1;
        let meta = json!({
            "camera_session_id":self.epoch,"frame_id":self.frame_id,"frame_age_ms":0,
            "width":1280,"height":720,"tracks":tracks,
            "association_ambiguity_scope":TRACK_AMBIGUITY_SCOPE,
            "ambiguous_track_keys":keys,
            "scene_association_ambiguous":flags.iter().any(|flag| *flag),
            "association_ambiguous":flags.iter().any(|flag| *flag),
        });
        let at = self.base + Duration::from_millis(ms);
        assert!(self.state.accept_frame(meta, vec![], at));
        self.state.latest_meta(at).unwrap()
    }
    fn warm_and_select(&mut self, boxes: &[[f32; 4]]) -> String {
        let mut meta = Value::Null;
        for step in 0..5 {
            meta = self.step(step * 33, boxes);
        }
        let key = meta["tracks"][0]["track_key"].as_str().unwrap().to_owned();
        assert!(!association_is_ambiguous(&meta, Some(&key)));
        self.state
            .select_key(&key, self.base + Duration::from_millis(132), true);
        key
    }
}
fn face(x: f32, y: f32) -> [f32; 4] {
    [x, y, x + 0.12, y + 0.18]
}

#[test]
fn replay_one_two_three_people_preserves_explicit_target_identity() {
    for people in 1..=3 {
        let mut replay = Replay::new();
        let boxes: Vec<_> = (0..people)
            .map(|i| face(0.1 + i as f32 * 0.3, 0.2))
            .collect();
        let key = replay.warm_and_select(&boxes);
        let generation = replay.state.selection_generation;
        for step in 5..65 {
            let shift = 0.025 * (step as f32 / 8.0).sin();
            let boxes: Vec<_> = (0..people)
                .map(|i| face(0.1 + i as f32 * 0.3 + shift, 0.2))
                .collect();
            let meta = replay.step(step * 33, &boxes);
            assert_eq!(meta["tracks"].as_array().unwrap().len(), people as usize);
            assert_eq!(meta["selected_track_key"], key);
            assert_eq!(meta["selected_status"], "visible");
            assert_eq!(meta["association_ambiguous"], false);
            assert_eq!(replay.state.selection_generation, generation);
        }
    }
}

#[test]
fn replay_unrelated_crossing_does_not_clear_or_invalidate_selected_person() {
    let mut replay = Replay::new();
    let key = replay.warm_and_select(&[face(0.1, 0.2), face(0.5, 0.2), face(0.8, 0.2)]);
    let target_generation = replay.state.target_generation;
    let selection_generation = replay.state.selection_generation;
    let mut saw_overlap = false;
    for step in 0..61 {
        let delta = step as f32 * 0.005;
        let meta = replay.step(
            (step + 5) * 33,
            &[
                face(0.1, 0.2),
                face(0.5 + delta, 0.2),
                face(0.8 - delta, 0.2),
            ],
        );
        saw_overlap |= meta["scene_association_ambiguous"] == true;
        assert_eq!(meta["selected_track_key"], key);
        assert_eq!(meta["association_ambiguous"], false);
        assert_eq!(replay.state.target_generation, target_generation);
        assert_eq!(replay.state.selection_generation, selection_generation);
    }
    assert!(
        saw_overlap,
        "fixture must exercise actual unrelated overlap"
    );
}

#[test]
fn replay_selected_person_crossing_clears_binding_without_automatic_reselection() {
    let mut replay = Replay::new();
    let key = replay.warm_and_select(&[face(0.2, 0.2), face(0.7, 0.2)]);
    let generation = replay.state.selection_generation;
    let mut selected_overlap = false;
    for step in 0..66 {
        let delta = step as f32 * 0.006;
        let meta = replay.step(
            (step + 5) * 33,
            &[face(0.2 + delta, 0.2), face(0.7 - delta, 0.2)],
        );
        selected_overlap |= meta["ambiguous_track_keys"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some(&key));
        if selected_overlap {
            assert!(meta["selected_track_key"].is_null());
            assert_eq!(replay.state.selection_generation, generation + 1);
        }
    }
    assert!(selected_overlap, "fixture must overlap the selected track");
    assert!(replay.state.selected.is_none());
}

#[test]
fn replay_short_occlusion_recovers_same_uuid_without_extending_long_loss_boundary() {
    let mut replay = Replay::new();
    let key = replay.warm_and_select(&[face(0.2, 0.2), face(0.7, 0.2)]);
    let generation = replay.state.selection_generation;
    for ms in [165, 198, 231] {
        let meta = replay.step(ms, &[face(0.7, 0.2)]);
        assert_eq!(meta["selected_track_key"], key);
        assert_eq!(meta["selected_status"], "temporarily_lost");
        assert_eq!(replay.state.selection_generation, generation);
    }
    let meta = replay.step(264, &[face(0.2, 0.2), face(0.7, 0.2)]);
    assert_eq!(meta["selected_track_key"], key);
    assert_eq!(meta["selected_status"], "visible");
    for ms in [297, 450, 650, 864] {
        let meta = replay.step(ms, &[face(0.7, 0.2)]);
        assert_eq!(meta["selected_track_key"], key);
    }
    let meta = replay.step(865, &[face(0.7, 0.2)]);
    assert!(meta["selected_track_key"].is_null());
    assert_eq!(replay.state.selection_generation, generation + 1);
    let meta = replay.step(898, &[face(0.2, 0.2), face(0.7, 0.2)]);
    assert!(meta["selected_track_key"].is_null());
}

#[test]
fn replay_nonlinear_motion_retains_binding_without_using_predicted_box_as_observation() {
    let mut replay = Replay::new();
    let key = replay.warm_and_select(&[face(0.3, 0.2)]);
    for step in 0..100 {
        let x = 0.3 + 0.06 * (step as f32 / 12.0).sin();
        let y = 0.2 + 0.035 * (step as f32 / 9.0).sin();
        let observed = face(x, y);
        let meta = replay.step((step + 5) * 33, &[observed]);
        assert_eq!(meta["selected_track_key"], key);
        assert_eq!(meta["tracks"][0]["box"], json!(observed));
        assert_eq!(meta["selected_status"], "visible");
    }
}

#[test]
fn replay_scoped_metadata_preserves_stale_and_epoch_rejection() {
    let mut replay = Replay::new();
    let key = replay.warm_and_select(&[face(0.2, 0.2)]);
    let stamp = replay.base + Duration::from_millis(165);
    let meta = replay.step(165, &[face(0.2, 0.2)]);
    assert!(
        !replay
            .state
            .accept_frame(meta.clone(), vec![], stamp + Duration::from_millis(1))
    );
    assert_eq!(replay.state.selected.as_deref(), Some(key.as_str()));
    let mut old = meta;
    old["frame_id"] = json!(1);
    assert!(
        !replay
            .state
            .accept_frame(old, vec![], stamp + Duration::from_millis(2))
    );
    assert_eq!(
        replay
            .state
            .latest_meta(stamp + Duration::from_millis(400))
            .unwrap()["selected_status"],
        "temporarily_lost"
    );
    assert!(
        replay
            .state
            .latest_meta(stamp + Duration::from_millis(601))
            .unwrap()["selected_track_key"]
            .is_null()
    );
    replay.epoch = "fixture-camera-B".into();
    let meta = replay.step(800, &[face(0.2, 0.2)]);
    assert!(meta["selected_track_key"].is_null());
    assert!(
        meta["tracks"][0]["track_key"]
            .as_str()
            .unwrap()
            .starts_with("fixture-camera-B:")
    );
}

#[test]
fn selecting_unrelated_person_is_allowed_but_each_overlapping_person_is_rejected() {
    let service = CameraService::new("unused-fixture-root");
    service.state.lock().unwrap().latest = Some(FrameSnapshot {
        jpeg: Arc::new(vec![]),
        received: Instant::now(),
        meta: json!({"frame_age_ms":0,"association_ambiguous":true,
            "association_ambiguity_scope":TRACK_AMBIGUITY_SCOPE,"ambiguous_track_keys":["B","C"],
            "scene_association_ambiguous":true,"tracks":[{"track_key":"A"},{"track_key":"B"},{"track_key":"C"}]}),
    });
    assert!(service.select("B").is_err());
    assert!(service.select("C").is_err());
    service.select("A").unwrap();
    assert_eq!(service.status()["selected_track_key"], "A");
    assert_eq!(service.status()["association_ambiguous"], false);
    assert_eq!(service.status()["scene_association_ambiguous"], true);
}

#[test]
fn unknown_or_malformed_local_scope_and_legacy_global_overlap_fail_closed() {
    for meta in [
        json!({"association_ambiguous":true}),
        json!({"association_ambiguity_scope":"future_scope","ambiguous_track_keys":[]}),
        json!({"association_ambiguity_scope":TRACK_AMBIGUITY_SCOPE}),
        json!({"association_ambiguity_scope":TRACK_AMBIGUITY_SCOPE,"ambiguous_track_keys":[17]}),
        json!({"association_ambiguity_scope":TRACK_AMBIGUITY_SCOPE,"ambiguous_track_keys":[""]}),
    ] {
        assert!(association_is_ambiguous(&meta, Some("A")));
    }
}

#[test]
fn unconfirmed_or_unassigned_overlapping_detection_still_marks_confirmed_target() {
    let detections = vec![
        FaceDetection {
            bbox: face(0.2, 0.2),
            score: 0.9,
        },
        FaceDetection {
            bbox: face(0.25, 0.2),
            score: 0.2,
        },
    ];
    let flags = ambiguous_detections(&detections);
    let id = Uuid::from_u128(1);
    let target = TrackInfo {
        uuid: id,
        tracked_location: detections[0].bbox,
        count: 5,
        created: 1,
        last_updated: 2,
    };
    for other in [
        None,
        Some(TrackInfo {
            uuid: Uuid::from_u128(2),
            count: 1,
            ..target
        }),
    ] {
        let (tracks, keys) = tracked_faces(
            "epoch",
            &detections,
            &[Some(target), other],
            &flags,
            &mut HashMap::new(),
            &mut 0,
        );
        assert_eq!(tracks.len(), 1);
        assert_eq!(keys, vec![format!("epoch:{id}")]);
        assert_eq!(tracks[0]["association_ambiguous"], true);
    }
}
