//! Explicit synthetic remote diagnostic for the opt-in production ACP adapter.
//! No database, device, display, audio or raw response output is involved.
use ai_tutor::kimi::{
    KimiProvider,
    acp_stream::SEGMENT_CHANNEL_CAPACITY,
    tutor_segments::{DeliveryBinding, DeliveryLedger, SpeechState, TutorSegment},
};
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

struct Options {
    root: PathBuf,
    language: String,
    cancel_after_ms: Option<u64>,
}

fn arguments(args: impl IntoIterator<Item = String>) -> Result<Option<Options>> {
    let mut root = std::env::current_dir()?;
    let mut language = None;
    let mut real = false;
    let mut cancel_after_ms = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(None),
            "--real-synthetic" => {
                ensure!(!real, "Duplicate real diagnostic flag");
                real = true;
            }
            "--root" => {
                root = PathBuf::from(args.next().ok_or_else(|| anyhow!("Missing root path"))?)
            }
            "--language" => {
                ensure!(language.is_none(), "Duplicate language option");
                language = Some(args.next().ok_or_else(|| anyhow!("Missing language"))?);
            }
            "--cancel-after-ms" => {
                ensure!(cancel_after_ms.is_none(), "Duplicate cancellation option");
                let ms: u64 = args
                    .next()
                    .ok_or_else(|| anyhow!("Missing cancellation interval"))?
                    .parse()?;
                ensure!(
                    (1..=90_000).contains(&ms),
                    "Cancellation interval must be 1..90000 milliseconds"
                );
                cancel_after_ms = Some(ms);
            }
            _ => bail!("Unknown diagnostic argument"),
        }
    }
    ensure!(
        real,
        "Explicit --real-synthetic is required; no model request was made"
    );
    let language = language.ok_or_else(|| {
        anyhow!("Explicit --language zh|en is required; no model request was made")
    })?;
    ensure!(
        ["zh", "en"].contains(&language.as_str()),
        "Language must be zh or en"
    );
    Ok(Some(Options {
        root,
        language,
        cancel_after_ms,
    }))
}

fn accept_segment(
    ledger: &mut DeliveryLedger,
    segment: TutorSegment,
    first: &mut Option<f64>,
    started: Instant,
) -> Result<()> {
    ledger.accept(&segment)?;
    first.get_or_insert(started.elapsed().as_secs_f64() * 1000.0);
    // Deliberately issue no displayed or speech receipt. The body is dropped.
    Ok(())
}

async fn run(options: Options) -> Result<Value> {
    let provider = KimiProvider::new(&options.root)?;
    let audit = provider.probe().await?;
    ensure!(
        audit["available"] == true,
        "Existing tool-free provider verification is required; no model request was made"
    );
    let rid = format!("segments-synthetic-{}", uuid::Uuid::new_v4().simple());
    let generation = 1;
    let request = json!({"request_id":rid,"learner_id":"synthetic_segments_probe","context":{
    "language":options.language,"evidence":[],"user_input":if options.language == "zh" {
        "这是合成教学诊断，不是真实学习者或设备观察。请用两句简短的话说明测距模块的作用，再建议一个尚未进行的小实验。"
    } else {
        "This is a synthetic teaching diagnostic, not a real learner or device observation. In two short sentences, explain what a distance sensor does and suggest one experiment that has not been performed."
    }}});
    let mut ledger = DeliveryLedger::new(DeliveryBinding {
        request_id: rid.clone(),
        generation,
    })?;
    let (sender, mut receiver) = mpsc::channel(SEGMENT_CHANNEL_CAPACITY);
    let started = Instant::now();
    let operation = provider.generate_segments(&request, generation, sender);
    tokio::pin!(operation);
    let cancellation_timer = tokio::time::sleep(Duration::from_millis(
        options.cancel_after_ms.unwrap_or(90_000),
    ));
    tokio::pin!(cancellation_timer);
    let mut cancellation_at = None;
    let mut cancellation_source = None;
    let mut first_received_ms = None;
    let mut receiver_open = true;
    let mut consumer_error = None;
    let outcome = loop {
        tokio::select! { biased;
            result=&mut operation=>break result,
            _=&mut cancellation_timer,if options.cancel_after_ms.is_some() && cancellation_at.is_none()=>{
                cancellation_at=Some(Instant::now());
                cancellation_source=Some("synthetic_timer");
                provider.cancel(&rid).await?;
            }
            signal=tokio::signal::ctrl_c(),if cancellation_at.is_none()=>{
                signal?;
                cancellation_at=Some(Instant::now());
                cancellation_source=Some("ctrl_c");
                provider.cancel(&rid).await?;
            }
            segment=receiver.recv(),if receiver_open=>{
                match segment {
                    Some(segment) if consumer_error.is_none()=>{
                        if let Err(error)=accept_segment(&mut ledger,segment,&mut first_received_ms,started) {
                            consumer_error=Some(error.to_string());
                            provider.cancel(&rid).await?;
                        }
                    }
                    Some(_)=>{},
                    None=>receiver_open=false,
                }
            }
        }
    };
    let operation_returned = Instant::now();
    let cancel_to_return_ms =
        cancellation_at.map(|time| operation_returned.duration_since(time).as_secs_f64() * 1000.0);
    let mut queued_discarded = 0;
    // The returned operation has dropped its sender. Drain the at-most-two
    // remaining validated items only on success; failures invalidate delivery.
    while let Ok(segment) = receiver.try_recv() {
        if outcome.is_ok() && consumer_error.is_none() {
            if let Err(error) =
                accept_segment(&mut ledger, segment, &mut first_received_ms, started)
            {
                consumer_error = Some(error.to_string());
            }
        } else {
            queued_discarded += 1;
        }
    }
    let mut reply_characters = None;
    let error = match outcome {
        Ok(reply) if consumer_error.is_none() => {
            if let Err(error) = ledger.finish_validated(&reply) {
                Some(error.to_string())
            } else {
                reply_characters = reply.reply()["reply_text"]
                    .as_str()
                    .map(|text| text.chars().count());
                None
            }
        }
        Ok(_) => consumer_error,
        Err(error) => Some(error.to_string()),
    };
    if error.is_some() {
        ledger.invalidate();
    }
    drop(receiver);
    provider.close().await?;
    let status = provider.status();
    let metrics = status["metrics"]["last_acp_segments"].clone();
    let cleanup_verified = metrics["process_reaped"] == true
        && metrics["owned_directory_removed"] == true
        && metrics["stderr_drained"] == true
        && status["running"] == 0;
    let delivered = ledger
        .segments()
        .iter()
        .any(|s| s.displayed || s.speech != SpeechState::NotStarted);
    Ok(
        json!({"test_type":"synthetic_kimi_acp_validated_segments","version":"2.3.1","language":options.language,
        "success":error.is_none() && cleanup_verified,"error":error,"failure_code":metrics["failure_code"],"checked_at":chrono::Utc::now().to_rfc3339(),
        "binary_sha256":audit["binary_sha256"],"agent_sha256":audit["agent_sha256"],
        "generation":generation,"segment_count":ledger.segments().len(),"reply_characters":reply_characters,
        "first_validated_segment_ms":metrics["first_validated_segment_ms"],"final_validated_ms":metrics["final_validated_ms"],
        "provider_time_origin":"before_session_prompt_write","consumer_first_received_ms":first_received_ms,
        "consumer_time_origin":"before_generate_segments_including_gate_and_handshake",
        "total_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "cancellation":{"requested":cancellation_at.is_some(),"source":cancellation_source,"requested_after_ms":options.cancel_after_ms,
            "timer_origin":"before_generate_segments_including_handshake","cancel_to_return_ms":cancel_to_return_ms,
            "expected_aborted":cancellation_at.is_some() && error.is_some(),"remote_cancel_ack_received":false},
        "cleanup_verified":cleanup_verified,"queued_segments_discarded":queued_discarded,
        "delivery_ledger":ledger,"displayed":false,"played":false,"delivered":delivered,
        "raw_generated_text_retained":false,"user_database_opened":false,"devices_opened":false,
        "provider_metrics":metrics}),
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let Some(options) = arguments(std::env::args().skip(1))? else {
        println!(
            "kimi-segments-probe --real-synthetic --language zh|en [--root PATH] [--cancel-after-ms N]\nOne paid synthetic request through the existing verified provider and foreground gate. Metrics only; no display, speech, database or devices. Cancellation timing starts before generation/handshake; an expected aborted run remains success=false."
        );
        return Ok(());
    };
    let report = run(options).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    ensure!(
        report["success"] == true,
        "Segment diagnostic did not complete; see sanitized report"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Result<Option<Options>> {
        arguments(values.iter().map(|v| (*v).to_owned()))
    }
    #[test]
    fn remote_probe_requires_both_explicit_intent_and_valid_language() -> Result<()> {
        for input in [
            &[][..],
            &["--real-synthetic"][..],
            &["--language", "zh"][..],
            &["--real-synthetic", "--language", "fr"][..],
            &[
                "--real-synthetic",
                "--language",
                "zh",
                "--cancel-after-ms",
                "0",
            ][..],
        ] {
            ensure!(args(input).is_err());
        }
        ensure!(args(&["--help"])?.is_none());
        let parsed = args(&[
            "--real-synthetic",
            "--language",
            "en",
            "--cancel-after-ms",
            "300",
        ])?
        .unwrap();
        ensure!(parsed.language == "en" && parsed.cancel_after_ms == Some(300));
        Ok(())
    }
}
