//! Explicit local synthetic PCM diagnostic. Never opens an audio device.
use ai_tutor::audio::streaming_tts::{QUEUE_CAPACITY, SynthesisOutcome, TtsClient};
use ai_tutor::local_speech::LocalSpeech;
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::json;
use std::{
    io::Write,
    path::Path,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// Only files created with create_new and empty directories created by this
/// probe are removed. No recursive delete or production descriptor is used.
struct ProbeFixture {
    root: PathBuf,
    descriptor: PathBuf,
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
}
impl ProbeFixture {
    fn create(root: &Path, descriptor: &serde_json::Value, prepare: bool) -> Result<Self> {
        let directory = root
            .join("runtime")
            .join(format!("tts-probe-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&directory)?;
        let mut fixture = Self {
            root: directory.clone(),
            descriptor: directory.join("runtime/tts-service.json"),
            files: vec![],
            directories: vec![directory],
        };
        let runtime = fixture.root.join("runtime");
        std::fs::create_dir(&runtime)?;
        fixture.directories.push(runtime);
        fixture.write_new(fixture.descriptor.clone(), descriptor)?;
        if prepare {
            fixture.write_new(
                fixture.root.join("runtime/local-speech-policy.json"),
                &json!({
                "protocol_version":1,"mode":"external","warmup":"synthetic_bilingual"}),
            )?;
        }
        Ok(fixture)
    }
    fn write_new(&mut self, path: PathBuf, value: &serde_json::Value) -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        self.files.push(path);
        file.write_all(&serde_json::to_vec(value)?)?;
        Ok(())
    }
    fn cleanup(&mut self) -> Result<()> {
        // Retain any failed path for the Drop retry, rather than forgetting a
        // private credential file after a transient filesystem error.
        while let Some(path) = self.files.last() {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            self.files.pop();
        }
        while let Some(path) = self.directories.last() {
            match std::fs::remove_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            self.directories.pop();
        }
        Ok(())
    }
}
impl Drop for ProbeFixture {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

async fn prepare_bilingual(
    mut fixture: ProbeFixture,
    engine: &str,
    cancel_ms: Option<u64>,
) -> Result<()> {
    let local = match LocalSpeech::new(&fixture.root) {
        Ok(local) => Arc::new(local),
        Err(error) => {
            fixture.cleanup()?;
            return Err(error);
        }
    };
    let cancelled_at = Arc::new(Mutex::new(None));
    let timer = cancel_ms.map(|ms| {
        let local = local.clone();
        let cancelled_at = cancelled_at.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            *cancelled_at.lock().unwrap() = Some(Instant::now());
            local.cancel_preparation();
        })
    });
    let started = Instant::now();
    let result = local.prepare_external_tts().await;
    let cancel_to_return_ms = cancelled_at
        .lock()
        .unwrap()
        .map(|when: Instant| when.elapsed().as_secs_f64() * 1000.0);
    if let Some(timer) = timer {
        timer.abort();
        let _ = timer.await;
    }
    let status = local.status();
    let (outcome, receipt, error) = match result {
        Ok(prepared) => ("completed", Some(prepared.receipt().clone()), None),
        Err(error) => (
            if status["phase"] == "cancelled" {
                "cancelled"
            } else {
                "failed"
            },
            None,
            Some(error.to_string()),
        ),
    };
    fixture.cleanup()?;
    let success = if cancel_ms.is_some() {
        outcome == "cancelled"
    } else {
        outcome == "completed" && status["phase"] == "warm_ready_pending_install"
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
        "test_type":"rust_local_tts_prepare_bilingual_synthetic","engine":engine,"languages":["zh","en"],
        "success":success,"outcome":outcome,"error":error,"receipt":receipt,"status":status,
        "total_ms":started.elapsed().as_secs_f64()*1000.0,"cancel_requested_after_ms":cancel_ms,
        "cancel_to_return_ms":cancel_to_return_ms,"client_installed":false,
        "speaker_output":false,"microphone_opened":false,"played":false,"remote_cancel_ack_received":false,
        "production_descriptor_created":false,"production_policy_created":false,"private_fixture_cleaned":true}))?
    );
    ensure!(
        success,
        "Local bilingual preparation did not meet its explicit outcome; see report"
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let (mut root, mut language, mut explicit, mut cancel_ms) =
        (std::env::current_dir()?, None, false, None);
    let mut engine = "qwen".to_owned();
    let mut prepare = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "tts-stream-probe --real-local-synthetic --root PATH --language zh|en [--engine qwen|cosy] [--cancel-after-ms N]\ntts-stream-probe --real-local-synthetic --root PATH --prepare-bilingual [--engine qwen|cosy] [--cancel-after-ms N]\nReads local lab PCM only; no speaker, microphone or learner database. Preparation does not install a voice."
                );
                return Ok(());
            }
            "--real-local-synthetic" => explicit = true,
            "--prepare-bilingual" => prepare = true,
            "--root" => root = PathBuf::from(args.next().ok_or_else(|| anyhow!("Missing root"))?),
            "--engine" => engine = args.next().ok_or_else(|| anyhow!("Missing engine"))?,
            "--language" => {
                language = Some(args.next().ok_or_else(|| anyhow!("Missing language"))?)
            }
            "--cancel-after-ms" => {
                cancel_ms = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("Missing interval"))?
                        .parse::<u64>()?,
                )
            }
            _ => bail!("Unknown diagnostic argument"),
        }
    }
    ensure!(explicit, "Explicit --real-local-synthetic is required");
    ensure!(
        language
            .as_deref()
            .is_none_or(|language| ["zh", "en"].contains(&language)),
        "Expected zh or en"
    );
    ensure!(
        prepare || language.is_some(),
        "Explicit --language zh|en is required"
    );
    let max_cancel_ms = if prepare { 120_000 } else { 90_000 };
    ensure!(
        cancel_ms.is_none_or(|ms| (1..=max_cancel_ms).contains(&ms)),
        "Cancellation interval is outside this probe mode's deadline"
    );
    root = root.canonicalize()?;
    ensure!(
        ["qwen", "cosy"].contains(&engine.as_str()),
        "Expected qwen or cosy"
    );
    let cosy = engine == "cosy";
    let token = std::fs::read_to_string(root.join(if cosy {
        "runtime/speech-cosy/api-key"
    } else {
        "runtime/speech-gpu/omni-api-key"
    }))?;
    let mut service = json!({"protocol_version":1,"endpoint":format!("http://127.0.0.1:{}/v1/audio/speech", if cosy {18869} else {18868}),
        "bearer_token":token.trim(),"model":if cosy {"tutor-cosy-tts"} else {"tutor-qwen-tts"}});
    if cosy {
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join(
            "runtime/goal-baselines/20261104/cosyvoice3-preparation/reference-audio.json",
        ))?)?;
        let mut references = serde_json::Map::new();
        for language in ["zh", "en"] {
            let id = format!("{language}_concept");
            let reference = manifest["samples"]
                .as_array()
                .ok_or_else(|| anyhow!("Reference manifest has no samples"))?
                .iter()
                .find(|row| row["id"] == id)
                .ok_or_else(|| anyhow!("Missing synthetic reference"))?;
            references.insert(language.into(), json!({"wav_path":reference["path"],"sha256":reference["sha256"],"text":reference["text"]}));
        }
        service["references"] = references.into();
    }
    // The disposable diagnostic descriptor does not enable the production voice.
    let mut fixture = ProbeFixture::create(&root, &service, prepare)?;
    if prepare {
        return prepare_bilingual(fixture, &engine, cancel_ms).await;
    }
    let language = language.unwrap();
    let loaded = TtsClient::from_descriptor(&fixture.descriptor);
    fixture.cleanup()?;
    let client = loaded?;
    let epoch = Arc::new(AtomicU64::new(1));
    let cancelled_at = Arc::new(Mutex::new(None));
    let timer = cancel_ms.map(|ms| {
        let epoch = epoch.clone();
        let cancelled_at = cancelled_at.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            *cancelled_at.lock().unwrap() = Some(Instant::now());
            epoch.store(2, Ordering::SeqCst);
        })
    });
    let (send, mut receive) = tokio::sync::mpsc::channel(QUEUE_CAPACITY);
    let task_epoch = epoch.clone();
    let task_language = language.clone();
    let start = Instant::now();
    let job = tokio::spawn(async move {
        let text = if task_language == "zh" {
            "先比较两次结果，再决定下一步。"
        } else {
            "Compare the two results before choosing the next step."
        };
        client
            .synthesize(text, &task_language, "normal", task_epoch, 1, send)
            .await
    });
    let (mut samples, mut nonzero, mut discarded, mut first) = (0usize, 0usize, 0usize, None);
    while let Some(packet) = receive.recv().await {
        if epoch.load(Ordering::SeqCst) != 1 {
            discarded += packet.len();
            continue;
        }
        first.get_or_insert(start.elapsed().as_secs_f64() * 1000.0);
        samples += packet.len();
        nonzero += packet.iter().filter(|sample| **sample != 0).count();
    }
    let result = job.await?;
    if let Some(timer) = timer {
        timer.abort();
    }
    let (outcome, metrics, error) = match result {
        Ok(SynthesisOutcome::Completed(metrics)) => ("completed", Some(metrics), None),
        Ok(SynthesisOutcome::Cancelled) => ("cancelled", None, None),
        Err(error) => ("failed", None, Some(error.to_string())),
    };
    let success = if cancel_ms.is_some() {
        outcome == "cancelled"
    } else {
        outcome == "completed" && samples > 0 && nonzero > 0
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"test_type":"rust_local_tts_pcm_synthetic",
        "language":language,"engine":engine,"success":success,"outcome":outcome,"error":error,"metrics":metrics,
        "first_packet_received_ms":first,"samples_received":samples,"nonzero_samples":nonzero,
        "discarded_stale_samples":discarded,"cancel_requested_after_ms":cancel_ms,
        "cancel_to_return_ms":cancelled_at.lock().unwrap().map(|when: Instant| when.elapsed().as_secs_f64()*1000.0),
        "speaker_output":false,"microphone_opened":false,"played":false,
        "remote_cancel_ack_received":false,"production_descriptor_created":false}))?
    );
    ensure!(
        success,
        "Local PCM diagnostic did not meet its explicit outcome; see report"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn probe_cleanup_removes_only_its_own_files_and_empty_directories() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("runtime"))?;
        let production = root.path().join("runtime/local-speech-policy.json");
        std::fs::write(&production, "production-fixture")?;
        let mut fixture = ProbeFixture::create(root.path(), &json!({"fixture":true}), true)?;
        let directory = fixture.root.clone();
        let foreign = directory.join("not-owned-by-the-probe.txt");
        std::fs::write(&foreign, "must remain")?;
        assert!(
            fixture.cleanup().is_err(),
            "nonempty directory must not be deleted recursively"
        );
        assert!(!directory.join("runtime/tts-service.json").exists());
        assert!(!directory.join("runtime/local-speech-policy.json").exists());
        assert_eq!(std::fs::read_to_string(&foreign)?, "must remain");
        assert_eq!(std::fs::read_to_string(&production)?, "production-fixture");
        // This test owns the sentinel; the probe itself deliberately left it.
        std::fs::remove_file(foreign)?;
        fixture.cleanup()?;
        assert!(!directory.exists());
        Ok(())
    }
}
