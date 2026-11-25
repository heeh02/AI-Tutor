//! Local voice helper for desktop hosts. JSONL over owned stdin/stdout;
//! no learner database, teacher model, microphone capture or network listener.
mod backend;
mod jobs;
mod media;
mod stdio;

pub use stdio::run_stdio;

use anyhow::{Result, anyhow, ensure};
use backend::VoiceBackend;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};
use tokio::sync::{mpsc, watch};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Scope {
    owner: String,
    generation: u64,
}

#[derive(Debug, Deserialize)]
struct Request {
    version: u32,
    sequence: u64,
    id: String,
    #[serde(flatten)]
    command: Command,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Command {
    Status,
    Bind {
        scope: Scope,
    },
    Prepare,
    Speak {
        scope: Scope,
        text: String,
        language: String,
    },
    Transcribe {
        scope: Scope,
        wav_base64: String,
    },
    Cancel {
        operation_id: String,
        operation_sequence: u64,
    },
    Shutdown,
}

type Work = Pin<Box<dyn Future<Output = Result<Value>> + Send>>;

struct Job {
    request: Request,
    scope: Option<Scope>,
    cancel: watch::Sender<bool>,
    work: Work,
}

struct Session {
    sequence: u64,
    scope: Option<Scope>,
}

impl Session {
    fn accept(&mut self, request: &Request) -> Result<()> {
        ensure!(request.version == 1, "voice_protocol_version");
        ensure!(
            !request.id.is_empty()
                && request.id.len() <= 80
                && request
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b)),
            "voice_request_id"
        );
        ensure!(request.sequence > self.sequence, "voice_request_sequence");
        self.sequence = request.sequence;
        Ok(())
    }

    fn check_scope(&self, scope: &Scope) -> Result<()> {
        ensure!(self.scope.as_ref() == Some(scope), "voice_scope_stale");
        Ok(())
    }

    fn bind(&mut self, scope: Scope) -> Result<bool> {
        ensure!(
            !scope.owner.trim().is_empty() && scope.owner.len() <= 200 && scope.generation > 0,
            "voice_scope_invalid"
        );
        if self.scope.as_ref() == Some(&scope) {
            return Ok(false);
        }
        // Generation is monotonically increasing for the whole helper session,
        // including a switch to another companion/conversation.
        ensure!(
            self.scope
                .as_ref()
                .is_none_or(|old| scope.generation > old.generation),
            "voice_scope_generation"
        );
        self.scope = Some(scope);
        Ok(true)
    }
}

fn envelope(request: &Request, event: &str, data: Value) -> Value {
    json!({"version":1,"id":request.id,"sequence":request.sequence,"event":event,"data":data})
}

fn emit(output: &std::sync::mpsc::SyncSender<Value>, value: Value) -> Result<()> {
    output
        .try_send(value)
        .map_err(|_| anyhow!("voice_output_unavailable"))
}

fn error_code(error: &anyhow::Error) -> String {
    let text = error.to_string();
    if text.len() <= 100
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit())
    {
        text
    } else {
        "voice_operation_failed".into()
    }
}

async fn run(
    root: PathBuf,
    mut input: mpsc::Receiver<Result<String>>,
    output: std::sync::mpsc::SyncSender<Value>,
) -> Result<()> {
    let backend = VoiceBackend::new(root)?;
    let mut capabilities = backend.capability_info();
    capabilities["tts"]["max_text_utf16_units"] = json!(2000);
    capabilities["tts"]["segmented_playback"] = json!(true);
    capabilities["transport"] = json!("stdio_jsonl_v1");
    let result = serve(backend.clone(), &mut input, &output, capabilities).await;
    // All paths (EOF, broken stdout, malformed framing, shutdown) release owned models.
    let cleanup = backend.shutdown().await;
    result?;
    ensure!(
        cleanup["cleanup_confirmed"] == true,
        "voice_owned_cleanup_unconfirmed"
    );
    Ok(())
}

async fn serve(
    backend: Arc<VoiceBackend>,
    input: &mut mpsc::Receiver<Result<String>>,
    output: &std::sync::mpsc::SyncSender<Value>,
    capabilities: Value,
) -> Result<()> {
    let mut session = Session {
        sequence: 0,
        scope: None,
    };
    let mut active: Option<Job> = None;
    let (started_tx, mut started_rx) = mpsc::channel::<(u64, Value)>(8);
    emit(
        output,
        json!({"version":1,"event":"hello","data":capabilities}),
    )?;
    let mut shutdown_request = None;
    let outcome = loop {
        tokio::select! {
            // Fair polling keeps cancellation cleanup moving during status polling.
            line = input.recv() => {
                let Some(line) = line else { break Ok(()); };
                let line = match line {
                    Ok(line) => line,
                    Err(error) if error.to_string() == "voice_input_eof" => break Ok(()),
                    Err(error) => break Err(error),
                };
                let request: Request = match serde_json::from_str(&line) {
                    Ok(request) => request,
                    Err(_) => {
                        if let Err(error) = emit(output, json!({"version":1,"event":"rejected","data":{"error":"voice_invalid_request"}})) { break Err(error); }
                        continue;
                    }
                };
                let handled = (|| -> Result<()> {
                    session.accept(&request)?;
                    match &request.command {
                        Command::Status => emit(output, envelope(&request, "status", json!({
                            "backend":backend.status(),"scope":session.scope,
                            "audio_processing":backend.audio_processing_status(),
                            "active_operation":active.as_ref().map(|job| &job.request.id)
                        }))),
                        Command::Bind { scope } => {
                            if session.bind(scope.clone())? {
                                if let Some(job) = &active {
                                    if job.scope.is_some() {
                                        let _ = job.cancel.send(true);
                                        backend.request_stop_speech();
                                    }
                                }
                            }
                            emit(output, envelope(&request, "bound", json!({"scope":session.scope})))
                        }
                        Command::Cancel { operation_id, operation_sequence } => {
                            let job = active.as_ref().filter(|job| &job.request.id == operation_id && job.request.sequence == *operation_sequence)
                                .ok_or_else(|| anyhow!("voice_operation_not_active"))?;
                            let _ = job.cancel.send(true);
                            if matches!(job.request.command, Command::Prepare) { backend.request_stop_preparation(); }
                            else { backend.request_stop_speech(); }
                            emit(output, envelope(&request, "cancel_requested", json!({"operation_id":operation_id})))
                        }
                        Command::Shutdown => { shutdown_request = Some(()); Ok(()) }
                        command => {
                            ensure!(active.is_none(), "voice_busy");
                            let scope = match command {
                                Command::Speak {scope, ..} | Command::Transcribe {scope, ..} => {
                                    session.check_scope(scope)?;
                                    Some(scope.clone())
                                }
                                _ => None,
                            };
                            let (cancel, cancelled) = watch::channel(false);
                            let work = jobs::create(&backend, &request, cancelled, started_tx.clone())?;
                            emit(output, envelope(&request, "accepted", json!({"scope":scope})))?;
                            active = Some(Job { request: Request {
                                version: request.version, sequence: request.sequence,
                                id:request.id.clone(), command: match command {
                                    Command::Prepare => Command::Prepare,
                                    _ => Command::Status, // retain only operation identity, not transcript/audio/text
                                },
                            }, scope, cancel, work });
                            Ok(())
                        }
                    }
                })();
                if let Err(error) = handled {
                    if let Err(error) = emit(output, envelope(&request, "rejected", json!({"error":error_code(&error)}))) { break Err(error); }
                }
                if shutdown_request.take().is_some() { break Ok(()); }
            }
            Some((sequence, data)) = started_rx.recv() => {
                if let Some(job) = &active {
                    if job.request.sequence == sequence && !*job.cancel.borrow()
                        && job.scope == session.scope {
                        if let Err(error) = emit(output, envelope(&job.request, "speech_started", data)) { break Err(error); }
                    }
                }
            }
            result = async { active.as_mut().unwrap().work.as_mut().await }, if active.is_some() => {
                let job = active.take().unwrap();
                let canceled = *job.cancel.borrow() || (job.scope.is_some() && job.scope != session.scope);
                // A short clip can finish in the poll that queues its start receipt.
                // Deliver those receipts before the terminal event, never after it.
                let mut output_error = None;
                while let Ok((sequence, data)) = started_rx.try_recv() {
                    if sequence == job.request.sequence && !canceled {
                        if let Err(error) = emit(output, envelope(&job.request, "speech_started", data)) {
                            output_error = Some(error);
                            break;
                        }
                    }
                }
                if let Some(error) = output_error { break Err(error); }
                let (event, data) = if canceled {
                    let error = result.err().map(|error| error_code(&error))
                        .filter(|error| error != "voice_operation_canceled" && error != "voice_preparation_canceled");
                    ("canceled", json!({"scope":job.scope,"delivery_revoked":true,"stop_error":error}))
                } else {
                    match result {
                        Ok(mut data) => { data["scope"] = json!(job.scope); ("completed", data) },
                        Err(error) => ("failed", json!({"error":error_code(&error)})),
                    }
                };
                if let Err(error) = emit(output, envelope(&job.request, event, data)) { break Err(error); }
            }
        }
    };
    backend.request_shutdown();
    if let Some(mut job) = active {
        let _ = job.cancel.send(true);
        // Preparation must finish its ownership cleanup; do not abort it mid-launch.
        let _ = job.work.as_mut().await;
    }
    let cleanup = backend.shutdown().await;
    if outcome.is_ok() {
        emit(
            output,
            json!({"version":1,"event":"shutdown","data":cleanup}),
        )?;
    }
    outcome
}

#[cfg(test)]
mod tests;
