//! Offline detection replay only. Never creates a CameraService or PTZ owner.
use anyhow::{Result, anyhow};
use std::path::PathBuf;
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut fixture = None;
    let mut output = None;
    let mut candidate = None;
    while let Some(arg) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| anyhow!("Missing value for {arg}"))?;
        match arg.as_str() {
            "--fixture" => fixture = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--candidate-trace" => candidate = Some(PathBuf::from(value)),
            _ => return Err(anyhow!("Unknown argument: {arg}")),
        }
    }
    ai_tutor::vision::comparison::run(
        &fixture.ok_or_else(|| anyhow!("--fixture is required"))?,
        &output.ok_or_else(|| anyhow!("--output is required"))?,
        candidate.as_deref(),
    )
}
