//! Bounded, scheduled read-only OSC numeric sampling; never registers, writes,
//! moves, stops, changes AI, removes latches, or overwrites existing evidence.
use ai_tutor::obsbot_osc::{GimbalProfile, OscArg, OscReadClient, ReadQuery, ReadState};
use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn unix_ms() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut options = BTreeMap::new();
    let mut tiny3 = false;
    while let Some(arg) = args.next() {
        if arg == "--help" {
            println!(
                "osc-read-sample --server-port P --reply-port P --device-index I --count N --interval-ms MS --start-unix-ms MS --output NEW_JSON --tiny3-center-2015\nOnly GetGimbalPosInfo and GetAiTrackingInfo are sent to 127.0.0.1. No handshake, device writes, stop or latch changes."
            );
            return Ok(());
        }
        if arg == "--tiny3-center-2015" {
            ensure!(!tiny3, "duplicate profile");
            tiny3 = true;
            continue;
        }
        ensure!(
            matches!(
                arg.as_str(),
                "--server-port"
                    | "--reply-port"
                    | "--device-index"
                    | "--count"
                    | "--interval-ms"
                    | "--start-unix-ms"
                    | "--output"
            ),
            "unsupported sample option"
        );
        let value = args.next().context("sample option requires value")?;
        ensure!(
            options.insert(arg, value).is_none(),
            "duplicate sample option"
        );
    }
    let required = |key: &str| -> Result<&str> {
        options
            .get(key)
            .map(String::as_str)
            .with_context(|| format!("explicit {key} required"))
    };
    let server_port: u16 = required("--server-port")?.parse()?;
    let reply_port: u16 = required("--reply-port")?.parse()?;
    let index: i32 = required("--device-index")?.parse()?;
    let count: usize = required("--count")?.parse()?;
    let interval_ms: u64 = required("--interval-ms")?.parse()?;
    let start_ms: u64 = required("--start-unix-ms")?.parse()?;
    let output = PathBuf::from(required("--output")?);
    ensure!(tiny3, "explicit --tiny3-center-2015 required");
    ensure!((1..=40).contains(&count), "sample count must be 1..40");
    ensure!(
        (50..=1000).contains(&interval_ms),
        "interval must be 50..1000ms"
    );
    let now = unix_ms()?;
    ensure!(
        start_ms >= now && start_ms - now <= 30_000,
        "start must be within next 30 seconds"
    );
    ReadQuery::GimbalPosition.request(index)?;
    let profile = GimbalProfile::Tiny3Center2015PanPitch;
    let mut client = OscReadClient::bind_with_profile(
        server_port,
        reply_port,
        Duration::from_millis(600),
        profile,
    )
    .await?;
    let local = client.local_addr()?;
    // Reserve new evidence before any query; fail instead of rewriting a run.
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&output)
        .context("sample evidence must be a new file")?;
    let remaining = start_ms.saturating_sub(unix_ms()?);
    tokio::time::sleep(Duration::from_millis(remaining)).await;
    let cadence = tokio::time::Instant::now();
    let deadline = cadence + Duration::from_secs(30);
    let mut rows = Vec::with_capacity(count);
    let mut raw_counts = BTreeMap::<String, usize>::new();
    let mut yaw_counts = BTreeMap::<i32, usize>::new();
    let mut pitch_counts = BTreeMap::<i32, usize>::new();
    let mut ai_counts = BTreeMap::<u8, usize>::new();
    let mut changes = Vec::new();
    let mut previous: Option<(usize, i32, i32)> = None;
    let mut pose_ok = 0;
    let mut ai_ok = 0;
    for sample in 0..count {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep_until(
            (cadence + Duration::from_millis(interval_ms * sample as u64)).min(deadline),
        )
        .await;
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let requested_at = chrono::Local::now().to_rfc3339();
        let requested_unix_ms = unix_ms()?;
        let pose = match client.query(ReadQuery::GimbalPosition, index).await {
            Ok(receipt) => {
                pose_ok += 1;
                let raw = receipt
                    .message
                    .arguments
                    .iter()
                    .map(|arg| match arg {
                        OscArg::Int(value) => Some(*value),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>();
                *raw_counts.entry(serde_json::to_string(&raw)?).or_default() += 1;
                if let ReadState::GimbalPosition {
                    yaw_degrees,
                    pitch_degrees,
                    ..
                } = receipt.reply.state
                {
                    *yaw_counts.entry(yaw_degrees).or_default() += 1;
                    *pitch_counts.entry(pitch_degrees).or_default() += 1;
                    if let Some((prior, pyaw, ppitch)) = previous {
                        if pyaw != yaw_degrees || ppitch != pitch_degrees {
                            changes.push(json!({"before_sample":prior,"after_sample":sample,"before_yaw":pyaw,"before_pitch":ppitch,"after_yaw":yaw_degrees,"after_pitch":pitch_degrees}));
                        }
                    }
                    previous = Some((sample, yaw_degrees, pitch_degrees));
                }
                json!({"status":"PASS","receipt":receipt,"transport_diagnostics":client.diagnostics()})
            }
            Err(error) => {
                previous = None;
                json!({"status":"FAIL","error":error.to_string(),"transport_diagnostics":client.diagnostics()})
            }
        };
        let ai = match client.query(ReadQuery::AiTrackingInfo, index).await {
            Ok(receipt) => {
                ai_ok += 1;
                if let ReadState::AiTrackingInfo { locked } = receipt.reply.state {
                    *ai_counts.entry(u8::from(locked)).or_default() += 1;
                }
                json!({"status":"PASS","receipt":receipt,"transport_diagnostics":client.diagnostics()})
            }
            Err(error) => {
                json!({"status":"FAIL","error":error.to_string(),"transport_diagnostics":client.diagnostics()})
            }
        };
        rows.push(json!({"sample_index":sample,"requested_at":requested_at,"requested_unix_ms":requested_unix_ms,"pose":pose,"ai":ai}));
    }
    let complete = rows.len() == count && pose_ok == count && ai_ok == count;
    let summary = json!({"result":if complete{"PASS_READ_ONLY_SAMPLES"}else{"INCOMPLETE_READ_ONLY_SAMPLES"},"requested_samples":count,"completed_rounds":rows.len(),"pose_successes":pose_ok,"ai_successes":ai_ok,"raw_pose_counts":raw_counts,"yaw_counts":yaw_counts,"pitch_counts":pitch_counts,"ai_locked_counts":ai_counts,"adjacent_pose_changes":changes});
    let record = json!({"implementation":"Rust / read-only OSC pose+AI samples","at":chrono::Local::now().to_rfc3339(),"scheduled_start_unix_ms":start_ms,"interval_ms":interval_ms,"local_reply_address":local,"remote_address":format!("127.0.0.1:{server_port}"),"device_index":index,"gimbal_profile":profile,"mapping_source":profile.source_note(),"nonzero_commands_sent":0,"zero_stop_commands_sent":0,"software_registration_sent":false,"latches_changed":false,"summary":summary,"samples":rows,"limitation":"Later read-only samples cannot reconstruct unrecorded baseline/current values from an earlier failure or establish physical motion/stop. Rounded protocol output does not prove a particular rounding implementation."});
    serde_json::to_writer_pretty(&mut file, &record)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"output":output,"summary":record["summary"]}))?
    );
    if !complete {
        bail!("read-only sample run incomplete; evidence retained");
    }
    Ok(())
}
