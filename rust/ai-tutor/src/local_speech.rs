//! Explicit preparation of external or owned managed local voices. The runtime
//! coordinates startup and installation; this module never owns audio devices.
pub(crate) mod managed;
pub(crate) mod managed_prepare;
use crate::audio::{
    AudioService, TtsConfigurationReceipt, TtsPreparationToken,
    streaming_tts::{SynthesisOutcome, TtsClient, WARM_FIXTURE_VERSION, wait_cancelled},
};
use anyhow::{Result, anyhow, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

pub const PREPARATION_DEADLINE: Duration = Duration::from_secs(120);
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalSpeechMode {
    #[default]
    Disabled,
    External,
    Managed,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedSpeechConfig {
    pub engine: String,
    pub distribution: String,
    pub references_file: Option<PathBuf>,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WarmupPolicy {
    #[default]
    Disabled,
    SyntheticBilingual,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalSpeechPolicy {
    protocol_version: u8,
    pub mode: LocalSpeechMode,
    #[serde(default)]
    pub warmup: WarmupPolicy,
    #[serde(default)]
    pub managed: Option<ManagedSpeechConfig>,
}
impl LocalSpeechPolicy {
    pub fn load(root: &Path) -> Result<Self> {
        let file = match File::open(root.join("runtime/local-speech-policy.json")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    protocol_version: 1,
                    mode: LocalSpeechMode::Disabled,
                    warmup: WarmupPolicy::Disabled,
                    managed: None,
                });
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(4097).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 4096, "Local speech policy exceeds its bound");
        let policy: Self = serde_json::from_slice(&bytes)?;
        ensure!(
            policy.protocol_version == 1,
            "Unsupported local speech policy version"
        );
        ensure!(
            policy.mode != LocalSpeechMode::Disabled || policy.warmup == WarmupPolicy::Disabled,
            "Disabled local speech cannot request warmup"
        );
        ensure!(
            (policy.mode == LocalSpeechMode::Managed) == policy.managed.is_some(),
            "Managed voice configuration must match the selected mode"
        );
        if let Some(config) = &policy.managed {
            ensure!(
                ["qwen", "cosy"].contains(&config.engine.as_str()),
                "Unsupported managed voice"
            );
            ensure!(
                !config.distribution.is_empty()
                    && config.distribution.len() <= 80
                    && config
                        .distribution
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
                "Expected an explicit WSL distribution name"
            );
            ensure!(
                (config.engine == "cosy") == config.references_file.is_some(),
                "Only CosyVoice requires an explicit bilingual reference file"
            );
        }
        if policy.mode != LocalSpeechMode::Disabled
            && crate::gpu_resource_policy::load(root)?
                == crate::gpu_resource_policy::GpuResourceMode::QwenPair12gbV1
        {
            ensure!(
                policy.mode == LocalSpeechMode::Managed
                    && policy.warmup == WarmupPolicy::SyntheticBilingual
                    && policy
                        .managed
                        .as_ref()
                        .is_some_and(|config| config.engine == "qwen"),
                "The shared Qwen profile requires a managed Qwen voice and bilingual warmup"
            );
        }
        Ok(policy)
    }
}

pub struct PreparedTts {
    client: Arc<TtsClient>,
    preparation_epoch: u64,
    receipt: Value,
}
impl PreparedTts {
    pub fn receipt(&self) -> &Value {
        &self.receipt
    }
    pub fn client(&self) -> Arc<TtsClient> {
        self.client.clone()
    }
}
pub struct LocalSpeech {
    root: PathBuf,
    policy: LocalSpeechPolicy,
    status: Mutex<Value>,
    epoch: Arc<AtomicU64>,
    installation_commit: Arc<Mutex<Option<u64>>>,
    busy: AtomicBool,
}
struct Busy<'a>(&'a AtomicBool);
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl LocalSpeech {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().canonicalize()?;
        let policy = LocalSpeechPolicy::load(&root)?;
        let phase = if policy.mode == LocalSpeechMode::Disabled {
            "disabled"
        } else {
            "configured"
        };
        Ok(Self {
            root,
            policy,
            status: Mutex::new(
                json!({"phase":phase,"ready":false,"warmup_completed":false,"active":false,
            "external_instance_verified":false,"model_revision_verified":false,"speaker_output_measured":false}),
            ),
            epoch: Arc::new(AtomicU64::new(0)),
            installation_commit: Arc::new(Mutex::new(None)),
            busy: AtomicBool::new(false),
        })
    }
    pub fn status(&self) -> Value {
        self.status.lock().unwrap().clone()
    }
    pub fn policy(&self) -> &LocalSpeechPolicy {
        &self.policy
    }
    pub(crate) fn update_lifecycle(&self, fields: Value) {
        let mut status = self.status.lock().unwrap();
        for (key, value) in fields.as_object().into_iter().flatten() {
            status[key] = value.clone();
        }
    }
    pub fn cancel_preparation(&self) {
        let committed = self.installation_commit.lock().unwrap();
        let cancelled_epoch = self.epoch.fetch_add(1, Ordering::SeqCst);
        let installation_won = *committed == Some(cancelled_epoch);
        drop(committed);
        let mut status = self.status.lock().unwrap();
        if self.epoch.load(Ordering::SeqCst) != cancelled_epoch.wrapping_add(1) {
            return;
        }
        if installation_won {
            status["phase"] = json!("active");
            status["cancellation_after_install"] = json!(true);
        } else if !["active", "disabled"].contains(&status["phase"].as_str().unwrap_or("")) {
            status["phase"] = json!("cancelled");
        }
    }
    fn enter(&self) -> Result<Busy<'_>> {
        ensure!(
            self.busy
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok(),
            "Local speech preparation is already active"
        );
        Ok(Busy(&self.busy))
    }
    fn phase(&self, epoch: u64, phase: &str) {
        let mut status = self.status.lock().unwrap();
        if self.epoch.load(Ordering::SeqCst) == epoch {
            status["phase"] = json!(phase);
        }
    }

    /// Both language probes and readiness share one 120-second deadline. The
    /// foreground synthesize path keeps its independent 15-second I/O bounds.
    pub async fn prepare_external_tts(&self) -> Result<PreparedTts> {
        self.prepare_with_deadline(PREPARATION_DEADLINE).await
    }
    async fn prepare_with_deadline(&self, deadline: Duration) -> Result<PreparedTts> {
        self.prepare_client_with_deadline(deadline, None).await
    }
    pub(crate) async fn prepare_managed_tts(&self, client: Arc<TtsClient>) -> Result<PreparedTts> {
        self.prepare_client_with_deadline(PREPARATION_DEADLINE, Some(client))
            .await
    }
    async fn prepare_client_with_deadline(
        &self,
        deadline: Duration,
        client: Option<Arc<TtsClient>>,
    ) -> Result<PreparedTts> {
        ensure!(
            ((self.policy.mode == LocalSpeechMode::External && client.is_none())
                || (self.policy.mode == LocalSpeechMode::Managed && client.is_some()))
                && self.policy.warmup == WarmupPolicy::SyntheticBilingual,
            "Synthetic bilingual preparation requires explicit external speech opt-in"
        );
        let _busy = self.enter()?;
        let epoch = {
            let mut committed = self.installation_commit.lock().unwrap();
            *committed = None;
            self.epoch.fetch_add(1, Ordering::SeqCst).wrapping_add(1)
        };
        *self.status.lock().unwrap() = json!({"phase":"checking","ready":false,"warmup_completed":false,"active":false,
            "external_instance_verified":false,"model_revision_verified":false,"speaker_output_measured":false,
            "preparation_epoch":epoch});
        let started = Instant::now();
        let operation = async {
            let client = match client {
                Some(client) => client,
                None => Arc::new(TtsClient::from_descriptor(
                    &self.root.join("runtime/tts-service.json"),
                )?),
            };
            client.check_ready().await?;
            ensure!(
                self.epoch.load(Ordering::SeqCst) == epoch,
                "Local speech preparation cancelled"
            );
            {
                let mut status = self.status.lock().unwrap();
                ensure!(
                    self.epoch.load(Ordering::SeqCst) == epoch,
                    "Local speech preparation cancelled"
                );
                status["ready"] = json!(true);
                status["phase"] = json!("ready_cold");
                status["model"] = json!(client.model_name());
                status["engine_kind"] = json!(client.engine_kind());
            }
            let mut languages = Vec::with_capacity(2);
            for language in ["zh", "en"] {
                self.phase(epoch, "warming");
                let (send, mut receive) = mpsc::channel(2);
                let (result, samples) = tokio::join!(
                    client.warm_synthetic(language, self.epoch.clone(), epoch, send),
                    async {
                        let mut samples = 0usize;
                        while let Some(packet) = receive.recv().await {
                            samples += packet.len();
                        }
                        samples
                    }
                );
                let metrics = match result? {
                    SynthesisOutcome::Completed(metrics) => metrics,
                    SynthesisOutcome::Cancelled => {
                        return Err(anyhow!("Local speech preparation cancelled"));
                    }
                };
                ensure!(
                    samples > 0 && metrics["samples"].as_u64() == Some(samples as u64),
                    "Warmup PCM drain did not match the validated transport"
                );
                languages.push(json!({"language":language,"samples":samples,"transport":metrics}));
            }
            ensure!(
                self.epoch.load(Ordering::SeqCst) == epoch,
                "Local speech preparation cancelled"
            );
            let receipt = json!({"fixture_version":WARM_FIXTURE_VERSION,"model":client.model_name(),"engine_kind":client.engine_kind(),
                "configuration_sha256":client.configuration_sha256(),"languages":languages,
                "preparation_ms":started.elapsed().as_secs_f64()*1000.0,"total_deadline_ms":deadline.as_millis(),
                "external_instance_verified":false,"model_revision_verified":false,"speaker_output_measured":false});
            Ok(PreparedTts {
                client,
                preparation_epoch: epoch,
                receipt,
            })
        };
        let result = tokio::select! { biased;
            _=wait_cancelled(&self.epoch,epoch)=>Err(anyhow!("Local speech preparation cancelled")),
            result=tokio::time::timeout(deadline,operation)=>match result { Ok(result)=>result, Err(_)=>Err(anyhow!("Local speech preparation exceeded its total deadline")) },
        };
        {
            let mut status = self.status.lock().unwrap();
            if self.epoch.load(Ordering::SeqCst) == epoch {
                match &result {
                    Ok(prepared) => {
                        status["phase"] = json!("warm_ready_pending_install");
                        status["warmup_completed"] = json!(true);
                        status["receipt"] = prepared.receipt.clone();
                    }
                    Err(error) => {
                        status["phase"] = json!("degraded");
                        status["error"] =
                            json!(error.to_string().chars().take(240).collect::<String>());
                    }
                }
            }
        }
        result
    }

    /// Installing is explicit and only changes the client's future use. Busy or
    /// stale audio epochs stay pending; this method never queues text or retries.
    pub async fn install_prepared(
        &self,
        audio: &AudioService,
        prepared: &PreparedTts,
        expected_audio_epoch: u64,
    ) -> Result<TtsConfigurationReceipt> {
        let _busy = self.enter()?;
        ensure!(
            self.epoch.load(Ordering::SeqCst) == prepared.preparation_epoch,
            "Prepared voice has been superseded or cancelled"
        );
        // Once enqueued, the owner decides the commit order. Waiting for its
        // existing bounded confirmation avoids reporting cancellation when it
        // already installed the client but this task has not polled the receipt.
        let result = audio
            .configure_prepared_tts(
                prepared.client.clone(),
                expected_audio_epoch,
                TtsPreparationToken {
                    epoch: self.epoch.clone(),
                    expected: prepared.preparation_epoch,
                    committed: self.installation_commit.clone(),
                },
            )
            .await;
        let committed =
            *self.installation_commit.lock().unwrap() == Some(prepared.preparation_epoch);
        let mut status = self.status.lock().unwrap();
        let cancelled = self.epoch.load(Ordering::SeqCst) != prepared.preparation_epoch;
        let applied = result.as_ref().is_ok_and(|receipt| receipt.applied) || committed;
        status["phase"] = json!(if applied {
            "active"
        } else if cancelled {
            "cancelled"
        } else if result.is_ok() {
            "warm_ready_pending_install"
        } else {
            "degraded"
        });
        if applied && cancelled {
            status["cancellation_after_install"] = json!(true);
        }
        match &result {
            Ok(receipt) => status["installation"] = json!(receipt),
            Err(error) => {
                status["installation_error"] =
                    json!(error.to_string().chars().take(240).collect::<String>())
            }
        }
        result
    }
}

#[cfg(test)]
#[path = "local_speech_tests.rs"]
mod tests;
