use std::path::PathBuf;

fn main() -> std::process::ExitCode {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 2 || arguments[0] != "--root" {
        eprintln!("Usage: ai-tutor-voice-bridge --root <voice-root>");
        return std::process::ExitCode::from(2);
    }
    match ai_tutor::voice_bridge::run_stdio(PathBuf::from(&arguments[1])) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("voice_bridge_failed: inspect protocol status; no content logged");
            std::process::ExitCode::from(1)
        }
    }
}
