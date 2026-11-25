use super::run;
use anyhow::{Result, anyhow, ensure};
use serde_json::Value;
use std::{
    io::{BufRead, Read, Write},
    path::PathBuf,
    sync::mpsc,
    time::Duration,
};

/// stdin/stdout use dedicated OS threads: an idle open stdin must not keep
/// Tokio's blocking pool alive after an explicit shutdown command.
pub fn run_stdio(root: PathBuf) -> Result<()> {
    let (input_tx, input_rx) = tokio::sync::mpsc::channel(8);
    let failed_output = input_tx.clone();
    std::thread::Builder::new()
        .name("voice-stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut reader = stdin.lock();
            loop {
                let mut bytes = Vec::new();
                let result = reader
                    .by_ref()
                    .take(1_500_001)
                    .read_until(b'\n', &mut bytes);
                let line = match result {
                    Ok(0) => {
                        let _ = input_tx.blocking_send(Err(anyhow!("voice_input_eof")));
                        return;
                    }
                    Ok(_) if bytes.len() <= 1_500_000 => {
                        String::from_utf8(bytes).map_err(|_| anyhow!("voice_input_encoding"))
                    }
                    _ => Err(anyhow!("voice_input_frame_limit")),
                };
                let failed = line.is_err();
                if input_tx.blocking_send(line).is_err() || failed {
                    return;
                }
            }
        })?;
    let (output_tx, output_rx) = mpsc::sync_channel::<Value>(64);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("voice-stdout".into())
        .spawn(move || {
            let stdout = std::io::stdout();
            let mut writer = stdout.lock();
            let result = (|| -> std::io::Result<()> {
                for value in output_rx {
                    serde_json::to_writer(&mut writer, &value)?;
                    writer.write_all(b"\n")?;
                    writer.flush()?;
                }
                Ok(())
            })();
            if result.is_err() {
                let _ = failed_output.blocking_send(Err(anyhow!("voice_output_unavailable")));
            }
            let _ = done_tx.send(result.is_ok());
        })?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run(root, input_rx, output_tx));
    runtime.shutdown_timeout(Duration::from_secs(2));
    result?;
    ensure!(
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap_or(false),
        "voice_output_unavailable"
    );
    Ok(())
}
