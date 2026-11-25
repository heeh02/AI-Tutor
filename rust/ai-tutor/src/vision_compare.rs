//! Offline comparison adapter. Reuses production association metadata and State.
use super::*;
use anyhow::{Result, anyhow, ensure};
use std::io::BufRead;

fn rows(path: &Path) -> Result<Vec<Value>> {
    ensure!(
        fs::metadata(path)?.len() <= 32 * 1024 * 1024,
        "Fixture too large"
    );
    std::io::BufReader::new(fs::File::open(path)?)
        .lines()
        .map(|line| Ok(serde_json::from_str(&line?)?))
        .collect()
}
pub fn run(fixture: &Path, output: &Path, candidate: Option<&Path>) -> Result<()> {
    let input = rows(fixture)?;
    let fixture_sha = format!("{:x}", Sha256::digest(fs::read(fixture)?));
    let external = candidate.map(rows).transpose()?;
    ensure!(
        external
            .as_ref()
            .is_none_or(|rows| rows.len() == input.len()),
        "Candidate frame count mismatch"
    );
    let mut output_rows = Vec::new();
    let mut timings = Vec::new();
    for repeat in 0..if candidate.is_some() { 1 } else { 20 } {
        let mut case = String::new();
        let mut tracker = ByteTrackBuilder::new()
            .track_high_conf(0.4)
            .track_iou(0.25)
            .track_update(0.25)
            .track_extra_lifespan(600_000_000)
            .build::<FaceDetection>();
        let mut state = State::new();
        let mut labels = HashMap::new();
        let mut serial = 0;
        let mut raw_ids = HashMap::new();
        let mut raw_serial = 0u32;
        let base = Instant::now();
        for (index, row) in input.iter().enumerate() {
            let next_case = row["case_id"]
                .as_str()
                .ok_or_else(|| anyhow!("Missing case"))?;
            if case != next_case {
                tracker = ByteTrackBuilder::new()
                    .track_high_conf(0.4)
                    .track_iou(0.25)
                    .track_update(0.25)
                    .track_extra_lifespan(600_000_000)
                    .build::<FaceDetection>();
                state = State::new();
                labels.clear();
                serial = 0;
                raw_ids.clear();
                raw_serial = 0;
                case = next_case.into();
            }
            let width = row["width"]
                .as_f64()
                .ok_or_else(|| anyhow!("Missing width"))? as f32;
            let height = row["height"]
                .as_f64()
                .ok_or_else(|| anyhow!("Missing height"))? as f32;
            ensure!(width > 0.0 && height > 0.0, "Invalid dimensions");
            let source = row["detections"]
                .as_array()
                .ok_or_else(|| anyhow!("Missing detections"))?;
            let mut detections = Vec::new();
            for item in source {
                let values = item["xyxy"]
                    .as_array()
                    .ok_or_else(|| anyhow!("Missing box"))?;
                ensure!(values.len() == 4, "Invalid box");
                let mut bbox = [0.0; 4];
                for coordinate in 0..4 {
                    bbox[coordinate] = values[coordinate]
                        .as_f64()
                        .ok_or_else(|| anyhow!("Invalid coordinate"))?
                        as f32
                        / if coordinate % 2 == 0 { width } else { height };
                }
                detections.push(FaceDetection {
                    bbox,
                    score: item["score"]
                        .as_f64()
                        .ok_or_else(|| anyhow!("Missing score"))? as f32,
                });
            }
            let timestamp = row["timestamp_ms"]
                .as_u64()
                .ok_or_else(|| anyhow!("Missing timestamp"))?;
            let started = Instant::now();
            let infos = if let Some(external) = &external {
                let trace = &external[index];
                ensure!(
                    trace["fixture_sha256"] == fixture_sha
                        && trace["case_id"] == row["case_id"]
                        && trace["frame_id"] == row["frame_id"],
                    "Candidate fixture identity mismatch"
                );
                let assignments = trace["assignments"]
                    .as_array()
                    .ok_or_else(|| anyhow!("Missing assignments"))?;
                ensure!(
                    assignments.len() == detections.len(),
                    "Candidate detection count mismatch"
                );
                assignments
                    .iter()
                    .zip(&detections)
                    .map(|(assignment, detection)| {
                        if assignment.is_null() {
                            return Ok(None);
                        }
                        let id = assignment["track_id"]
                            .as_u64()
                            .ok_or_else(|| anyhow!("Missing track id"))?;
                        let count = assignment["count"]
                            .as_i64()
                            .ok_or_else(|| anyhow!("Missing count"))?;
                        ensure!(
                            id > 0 && count > 0 && count <= i32::MAX as i64,
                            "Invalid candidate assignment"
                        );
                        Ok(Some(TrackInfo {
                            uuid: Uuid::from_u128(id as u128),
                            tracked_location: detection.bbox,
                            count: count as i32,
                            created: 0,
                            last_updated: timestamp * 1_000_000,
                        }))
                    })
                    .collect::<Result<Vec<_>>>()?
            } else {
                tracker.update(&detections, (timestamp + 1) * 1_000_000)
            };
            let update_ms = started.elapsed().as_secs_f64() * 1000.0;
            if external.is_none() {
                timings.push(update_ms);
            }
            if repeat != 0 {
                continue;
            }
            let flags = ambiguous_detections(&detections);
            let epoch = row["epoch"]
                .as_str()
                .ok_or_else(|| anyhow!("Missing epoch"))?;
            let (tracks, ambiguous) =
                tracked_faces(epoch, &detections, &infos, &flags, &mut labels, &mut serial);
            let mut assignments = Vec::new();
            for (detection_index, info) in infos.iter().enumerate() {
                assignments.push(info.map(|info| {
                    let id = *raw_ids.entry(info.uuid).or_insert_with(|| { raw_serial += 1; raw_serial });
                    json!({"track_id":id,"count":info.count,"truth_id":source[detection_index]["truth_id"]})
                }).unwrap_or(Value::Null));
            }
            let at = base + Duration::from_millis(timestamp);
            let metadata = json!({"frame_id":row["frame_id"],"camera_session_id":epoch,"frame_age_ms":row["frame_age_ms"],"tracks":tracks,"association_ambiguity_scope":TRACK_AMBIGUITY_SCOPE,"ambiguous_track_keys":ambiguous,"scene_association_ambiguous":flags.iter().any(|v|*v),"association_ambiguous":flags.iter().any(|v|*v)});
            ensure!(
                state.accept_frame(metadata, vec![], at),
                "Replay frame rejected"
            );
            if let Some(select) = row["select_truth"].as_str() {
                let choice = source
                    .iter()
                    .position(|det| det["truth_id"].as_str() == Some(select))
                    .and_then(|det| infos[det])
                    .ok_or_else(|| anyhow!("Selection fixture has no track"))?;
                let key = format!("{epoch}:{}", choice.uuid);
                ensure!(
                    choice.count >= 3
                        && !association_is_ambiguous(&state.latest_meta(at).unwrap(), Some(&key)),
                    "Selection fixture is unconfirmed/ambiguous"
                );
                state.select_key(&key, at, true);
            }
            let meta = state.latest_meta(at).unwrap();
            let selected_truth = state
                .selected
                .as_ref()
                .and_then(|selected| {
                    infos.iter().position(|info| {
                        info.is_some_and(|info| format!("{epoch}:{}", info.uuid) == *selected)
                    })
                })
                .map(|position| source[position]["truth_id"].clone());
            output_rows.push(json!({"fixture_sha256":fixture_sha,"case_id":row["case_id"],"frame_id":row["frame_id"],"timestamp_ms":timestamp,"assignments":assignments,"selected_truth":selected_truth,"selected_status":meta["selected_status"],"selected_key":meta["selected_track_key"],"selection_generation":meta["selection_generation"],"update_ms":external.as_ref().map_or(json!(update_ms),|rows|rows[index]["update_ms"].clone())}));
        }
    }
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::File::create(output)?;
    for row in &output_rows {
        writeln!(file, "{row}")?;
    }
    timings.sort_by(f64::total_cmp);
    let percentile = |fraction: f64| {
        timings
            .get(((timings.len().saturating_sub(1)) as f64 * fraction) as usize)
            .copied()
    };
    println!(
        "{}",
        json!({"fixture_sha256":fixture_sha,"frames":input.len(),"algorithm":if candidate.is_some(){"official OC-SORT trace through production selection policy"}else{"edgefirst-tracker ByteTrack 0.38.2"},"policy_replays":1,"kernel_timing_repeats":if candidate.is_some(){Value::Null}else{json!(20)},"kernel_timing_source":if candidate.is_some(){"external Python oracle trace and adjacent .timings.json; no Rust kernel timing"}else{"20 Rust ByteTrack update passes"},"update_p50_ms":percentile(0.5),"update_p95_ms":percentile(0.95),"update_p99_ms":percentile(0.99),"devices_opened":false})
    );
    Ok(())
}
