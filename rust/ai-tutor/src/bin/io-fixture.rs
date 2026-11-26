//! Development-only native subprocess fixture. Never used by the application.
use std::io::{self, Read, Write};

fn repeated(mut output: impl Write, byte: u8, count: usize) -> io::Result<()> {
    let block = [byte; 4096];
    let mut remaining = count;
    while remaining > 0 {
        let size = remaining.min(block.len());
        output.write_all(&block[..size])?;
        remaining -= size;
    }
    output.flush()
}

fn main() -> io::Result<()> {
    // The test assigns its private Job Object before releasing this gate.
    let mut marker = [0u8; 1];
    io::stdin().read_exact(&mut marker)?;
    if marker != [b'G'] {
        std::process::exit(64);
    }
    match std::env::args().nth(1).as_deref() {
        Some("stress") => {
            let stderr = std::thread::spawn(|| repeated(io::stderr().lock(), b'E', 512 * 1024));
            repeated(io::stdout().lock(), b'O', 32 * 1024)?;
            stderr.join().expect("synthetic stderr writer")?;
            std::process::exit(7);
        }
        Some("fragmented") => {
            let mut output = io::stdout().lock();
            let json = "{\"role\":\"assistant\",\"content\":\"测距😀中文\"}\n";
            for byte in json.as_bytes() {
                output.write_all(&[*byte])?;
                output.flush()?;
            }
        }
        Some("over-stdout") => {
            let stderr = std::thread::spawn(|| repeated(io::stderr().lock(), b'E', 32 * 1024));
            repeated(io::stdout().lock(), b'O', 512 * 1024)?;
            stderr.join().expect("synthetic stderr writer")?;
        }
        Some("over-stderr") => {
            let stderr = std::thread::spawn(|| repeated(io::stderr().lock(), b'E', 512 * 1024));
            repeated(io::stdout().lock(), b'O', 32 * 1024)?;
            stderr.join().expect("synthetic stderr writer")?;
        }
        Some("sleep") => {
            io::stdout().write_all(b"READY\n")?;
            io::stdout().flush()?;
            io::stderr().write_all(b"SYNTHETIC STDERR READY\n")?;
            io::stderr().flush()?;
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
        _ => std::process::exit(64),
    }
    Ok(())
}
