use ai_tutor::kimi::acp_probe::{ProbeOptions, run};
use anyhow::{Result, anyhow, bail, ensure};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let mut root = std::env::current_dir()?;
    let mut mode = None;
    let mut cancel_after_ms = None;
    let mut thinking_low = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--thinking-low" => {
                ensure!(!thinking_low, "Duplicate thinking option");
                thinking_low = true;
            }
            "--static" | "--real-synthetic" => {
                ensure!(mode.is_none(), "Choose exactly one diagnostic mode");
                mode = Some(arg == "--real-synthetic");
            }
            "--root" => {
                root = PathBuf::from(args.next().ok_or_else(|| anyhow!("Missing root path"))?)
            }
            "--cancel-after-ms" => {
                cancel_after_ms = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("Missing cancel interval"))?
                        .parse()?,
                )
            }
            "--help" | "-h" => {
                println!(
                    "kimi-acp-probe (--static | --real-synthetic) [--root PATH] [--thinking-low] [--cancel-after-ms N]\nStatic uses a synthetic loopback-only provider with no credential link and sends no prompt.\nReal mode makes one paid synthetic request only after this session's effective empty-tool profile is proven. Thinking low requires an advertised option, accepted receipt and matching durable state. Outputs metrics only."
                );
                return Ok(());
            }
            _ => bail!("Unknown diagnostic argument"),
        }
    }
    let real_synthetic =
        mode.ok_or_else(|| anyhow!("Explicit --static or --real-synthetic is required"))?;
    let report = run(
        root,
        ProbeOptions {
            real_synthetic,
            cancel_after_ms,
            thinking_low,
        },
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    ensure!(
        report["success"] == true,
        "ACP diagnostic did not pass; see sanitized report"
    );
    Ok(())
}
