//! Explicit native-device probe. Saves metrics only; never audio or ambient text.
use ai_tutor::audio::AudioService;
use anyhow::Result;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
#[path = "audio_probe/synthetic_asr.rs"]
mod synthetic_asr;

#[tokio::main]
async fn main() -> Result<()> {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let audio = Arc::new(AudioService::new(root.clone()));
    let mut report = json!({"test_type":"native_ambient_audio_probe","at":chrono::Utc::now().with_timezone(&chrono::FixedOffset::east_opt(28800).unwrap()).to_rfc3339(),"implementation":"Rust / CPAL WASAPI / whisper-rs / SAPI COM","natural_speech_verified":false,"audible_output_confirmed":false,"raw_audio_saved":false});
    let ready = Instant::now();
    while audio.status()["voices"]
        .as_array()
        .is_none_or(|v| v.is_empty())
        && ready.elapsed() < Duration::from_secs(5)
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    report["devices"] = match audio.devices() {
        Ok(v) => v,
        Err(e) => json!({"error":e.to_string()}),
    };
    report["initial_status"] = audio.status();
    let capture_started = Instant::now();
    match audio.start_recording(None).await {
        Ok(device) => {
            report["capture_start"] = json!({"status":"PASS","device":device,"elapsed_ms":capture_started.elapsed().as_millis()});
            let refusal = audio
                .speak(
                    "This must not play while recording.".into(),
                    "en".into(),
                    audio.speech_epoch(),
                )
                .await;
            report["half_duplex"] = json!({"status":if matches!(refusal,Ok(false)){"PASS"}else{"FAIL"},"speech_rejected":matches!(refusal,Ok(false))});
            tokio::time::sleep(Duration::from_secs(2)).await;
            let started = Instant::now();
            report["ambient_capture_stop"] = match audio.stop_recording("zh").await {
                Ok(value) => {
                    json!({"status":if value["sample_count"].as_u64().unwrap_or(0)>0{"PASS"}else{"FAIL"},"elapsed_ms":started.elapsed().as_millis(),"rms":value["rms"],"sample_count":value["sample_count"],"sample_rate":value["sample_rate"],"recorded_ms":value["recorded_ms"],"text_characters":value["text"].as_str().map(|v|v.chars().count()).unwrap_or(0),"reliable_speech":false,"message":value["error"]})
                }
                Err(e) => {
                    json!({"status":"FAIL","error":e.to_string(),"elapsed_ms":started.elapsed().as_millis()})
                }
            };
        }
        Err(e) => {
            report["capture_start"] = json!({"status":"FAIL","error":e.to_string(),"elapsed_ms":capture_started.elapsed().as_millis()})
        }
    }
    audio.discard_recording();
    let english_started = Instant::now();
    report["tts_english_completion"] = match tokio::time::timeout(
        Duration::from_secs(15),
        audio.speak(
            "This is a local AI Tutor speech test.".into(),
            "en".into(),
            audio.speech_epoch(),
        ),
    )
    .await
    {
        Ok(Ok(true)) => {
            json!({"status":"PASS","sapi_reported_complete":true,"elapsed_ms":english_started.elapsed().as_millis(),"heard_by_adult":Value::Null})
        }
        Ok(Ok(false)) => json!({"status":"FAIL","error":"Speech was rejected or interrupted"}),
        Ok(Err(e)) => json!({"status":"FAIL","error":e.to_string()}),
        Err(_) => json!({"status":"FAIL","error":"Speech completion deadline exceeded"}),
    };
    let expected = audio.speech_epoch();
    let speaking = audio.clone();
    let voice = tokio::spawn(async move {
        speaking
            .speak(
                "这是本机语音测试，接下来验证停止播报。".repeat(6),
                "zh".into(),
                expected,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(450)).await;
    let stopped = Instant::now();
    let stop = audio.stop_speech().await;
    let outcome = tokio::time::timeout(Duration::from_secs(3), voice).await;
    let interrupted = matches!(outcome, Ok(Ok(Ok(false))));
    report["tts_chinese_stop"] = json!({"status":if stop.is_ok()&&interrupted{"PASS"}else{"FAIL"},"stop_response_ms":stopped.elapsed().as_millis(),"interrupted":interrupted,"error":stop.err().map(|e|e.to_string()),"heard_by_adult":Value::Null});
    let stale = audio
        .speak(
            "This stale request must never play.".into(),
            "en".into(),
            expected,
        )
        .await;
    report["stale_speech"] = json!({"status":if matches!(stale,Ok(false)){"PASS"}else{"FAIL"}});
    audio.close().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    report["final_status"] = audio.status();
    let model_root = root.clone();
    report["synthetic_bilingual_asr"] =
        match tokio::task::spawn_blocking(move || synthetic_asr::probe(&model_root)).await {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => json!({"status":"FAIL","error":error.to_string()}),
            Err(error) => json!({"status":"FAIL","error":error.to_string()}),
        };
    let path = root.join("runtime/rust_audio_probe.json");
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
