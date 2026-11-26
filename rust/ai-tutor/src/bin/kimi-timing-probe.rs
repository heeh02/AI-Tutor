//! Explicit paid-model diagnostic. Runs only after --real-synthetic is given.
//! It cannot deliver a reply, persist a learner turn, or operate devices.
use ai_tutor::kimi::KimiProvider;
use anyhow::{Result, bail, ensure};
use serde_json::json;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let mut root = std::env::current_dir()?;
    let mut language = "zh".to_owned();
    let mut real = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "kimi-timing-probe --real-synthetic [--root PATH] [--language zh|en]\nOne real remote synthetic request using the already verified tool-free provider; emits metrics only."
                );
                return Ok(());
            }
            "--real-synthetic" => real = true,
            "--root" => {
                root = PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--root requires a path"))?,
                )
            }
            "--language" => {
                language = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--language requires zh or en"))?
            }
            _ => bail!("Unknown diagnostic argument"),
        }
    }
    ensure!(
        real,
        "Explicit --real-synthetic is required; no model request was made"
    );
    ensure!(
        ["zh", "en"].contains(&language.as_str()),
        "Language must be zh or en"
    );
    let provider = KimiProvider::new(root)?;
    let status = provider.probe().await?;
    ensure!(
        status["available"] == true,
        "Existing tool-free provider verification is required"
    );
    let result = provider.diagnose_timing(&language).await;
    provider.close().await?;
    let report = match result {
        Ok(mut report) => {
            report["binary_sha256"] = status["binary_sha256"].clone();
            report["agent_sha256"] = status["agent_sha256"].clone();
            report["checked_at"] = json!(chrono::Utc::now().to_rfc3339());
            report
        }
        Err(error) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "test_type":"synthetic_kimi_stream_json_timing", "success":false,
                    "error":error.to_string(), "metrics":provider.status()["metrics"],
                    "binary_sha256":status["binary_sha256"], "delivered":false
                }))?
            );
            return Err(error);
        }
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
