//! Explicit synthetic remote memory evaluation. Never opens the learner database.
use ai_tutor::{kimi::KimiProvider, memory_worker::run_synthetic_probe};
use anyhow::{Result, bail, ensure};
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
                    "memory-shadow-probe --real-synthetic [--root PATH] [--language zh|en]\nOne synthetic remote consolidation in a temporary simulator database. Shadow only; no learner data or devices."
                );
                return Ok(());
            }
            "--real-synthetic" => real = true,
            "--root" => {
                root = PathBuf::from(args.next().ok_or_else(|| anyhow::anyhow!("Missing root"))?)
            }
            "--language" => {
                language = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("Missing language"))?
            }
            _ => bail!("Unknown argument"),
        }
    }
    ensure!(
        real && ["zh", "en"].contains(&language.as_str()),
        "Explicit synthetic flag and zh|en are required"
    );
    let provider = KimiProvider::new(root)?;
    let audit = provider.probe().await?;
    ensure!(
        audit["available"] == true,
        "Run the existing native --verify-kimi check for this agent configuration first"
    );
    let result = run_synthetic_probe(&provider, &language).await;
    provider.close().await?;
    let report = result?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    ensure!(
        report["result"]["status"] == "shadow_staged",
        "Synthetic consolidation did not reach a validated shadow result"
    );
    Ok(())
}
