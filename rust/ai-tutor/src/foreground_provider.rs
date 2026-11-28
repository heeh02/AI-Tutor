//! Startup-fixed foreground selection on the existing provider owner and gate.
use super::*;
use serde::Deserialize;
use std::sync::Arc;
#[path = "omni.rs"]
mod omni;

const POLICY_PATH: &str = "runtime/teaching-provider-policy.json";
const UNAVAILABLE: &str =
    "前台教学模型不可用，请检查 runtime/teaching-provider-policy.json 和本地凭证，修正后重启工作台";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Selection {
    Kimi,
    FlashOff,
    FlashLow,
    Omni,
    Invalid,
}
impl Selection {
    fn arm(self) -> Result<&'static str> {
        match self {
            Self::FlashOff => Ok("non_thinking"),
            Self::FlashLow => Ok("thinking_low"),
            _ => bail!("foreground_selection_unavailable"),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    protocol_version: u8,
    provider: String,
    #[serde(default)]
    thinking: Option<String>,
}
fn parse_policy(bytes: &[u8]) -> Result<Selection> {
    let policy: Policy =
        serde_json::from_slice(bytes).map_err(|_| anyhow!("foreground_policy_invalid"))?;
    ensure!(policy.protocol_version == 1, "foreground_policy_version");
    match (policy.provider.as_str(), policy.thinking.as_deref()) {
        ("kimi", None) => Ok(Selection::Kimi),
        ("deepseek_flash", Some("non_thinking")) => Ok(Selection::FlashOff),
        ("deepseek_flash", Some("thinking_low")) => Ok(Selection::FlashLow),
        ("qwen_omni", None) => Ok(Selection::Omni),
        _ => bail!("foreground_policy_selection"),
    }
}
fn selection(root: &Path) -> Result<Selection> {
    let path = root.join(POLICY_PATH);
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Selection::Kimi);
        }
        Err(_) => bail!("foreground_policy_read_failed"),
    };
    use std::io::Read;
    let mut bytes = vec![];
    file.take(2049)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow!("foreground_policy_read_failed"))?;
    ensure!(bytes.len() <= 2048, "foreground_policy_too_large");
    parse_policy(&bytes)
}

pub(crate) struct ForegroundRuntime {
    selected: Selection,
    local_ready: bool,
    error: Option<String>,
    last: Mutex<Value>,
    #[cfg(windows)]
    client: Option<reqwest::Client>,
    omni: Option<Arc<omni::OmniRuntime>>,
}
impl ForegroundRuntime {
    pub(crate) fn kimi() -> Self {
        Self {
            selected: Selection::Kimi,
            local_ready: false,
            error: None,
            last: Mutex::new(Value::Null),
            #[cfg(windows)]
            client: None,
            omni: None,
        }
    }
    fn load(root: &Path) -> Self {
        let mut runtime = Self::kimi();
        runtime.selected = match selection(root) {
            Ok(selected) => selected,
            Err(error) => {
                runtime.selected = Selection::Invalid;
                runtime.error = Some(error.to_string());
                return runtime;
            }
        };
        if runtime.is_kimi() {
            return runtime;
        }
        if runtime.selected == Selection::Omni {
            match omni::OmniRuntime::prepare(root).and_then(|service| {
                service.start(root)?;
                Ok(service)
            }) {
                Ok(service) => {
                    runtime.omni = Some(service);
                    runtime.local_ready = true;
                }
                Err(error) => runtime.error = Some(error.to_string()),
            }
            return runtime;
        }
        #[cfg(windows)]
        {
            let prepared = (|| -> Result<reqwest::Client> {
                let client = deepseek_client::new_client()?;
                deepseek_client::credential_preflight()?;
                Ok(client)
            })();
            match prepared {
                Ok(client) => {
                    runtime.client = Some(client);
                    runtime.local_ready = true;
                }
                Err(error) => runtime.error = Some(error.to_string()),
            }
        }
        #[cfg(not(windows))]
        {
            runtime.error = Some("foreground_flash_requires_windows_dpapi".into());
        }
        runtime
    }
    pub(crate) fn is_kimi(&self) -> bool {
        self.selected == Selection::Kimi
    }
    pub(crate) fn ensure_ready(&self) -> Result<()> {
        if self.selected == Selection::Omni {
            ensure!(
                self.omni.as_ref().is_some_and(|service| service.ready()),
                UNAVAILABLE
            );
            return Ok(());
        }
        ensure!(
            self.local_ready && self.selected != Selection::Invalid,
            UNAVAILABLE
        );
        Ok(())
    }
    pub(crate) async fn stop(&self) {
        if let Some(service) = &self.omni {
            service.stop().await;
        }
    }
    pub(crate) fn record(&self, metrics: &Value, success: bool) {
        *self.last.lock().unwrap() = json!({"succeeded":success,"at":now(),"metrics":metrics});
    }
    #[cfg(test)]
    fn scripted_flash() -> Self {
        let mut runtime = Self::kimi();
        runtime.selected = Selection::FlashOff;
        runtime.local_ready = true;
        runtime
    }
}

impl KimiProvider {
    /// Only Workstation selects this policy. Explicit Kimi diagnostics keep new().
    pub(crate) fn new_for_workstation(root: PathBuf) -> Result<Self> {
        let foreground = ForegroundRuntime::load(&root);
        let mut provider = Self::new(root)?;
        provider.foreground = foreground;
        Ok(provider)
    }
    pub fn foreground_status(&self) -> Value {
        let running = self
            .calls
            .lock()
            .unwrap()
            .values()
            .filter(|call| !call.background)
            .count();
        if self.foreground.is_kimi() {
            let kimi = self.status();
            return json!({"provider":"kimi","available":kimi["available"],
                "readiness_basis":"existing_kimi_profile_verification","verified":kimi["verified"],
                "error":kimi["error"],"running":running,"tools":[],"startup_fixed":true});
        }
        if self.foreground.selected == Selection::Omni {
            let service = self
                .foreground
                .omni
                .as_ref()
                .map(|service| service.status());
            return json!({"provider":"qwen_omni","available":self.foreground.omni.as_ref().is_some_and(|service| service.ready()),
                "model":OMNI_MODEL,"local":true,"running":running,"tools":[],"startup_fixed":true,
                "readiness_basis":"local_model_health_and_text_speech_warmup",
                "stream_contract":"metadata_v2_complete_local_json","reply_text_origin":"local_complete_json_validated_server_sentence_split",
                "input_modalities":["text","image","audio"],"output_modalities":["text","audio"],
                "service":service,"error":self.foreground.error});
        }
        let invalid = self.foreground.selected == Selection::Invalid;
        json!({"provider":if invalid {"invalid"} else {"deepseek_flash"},
            "available":self.foreground.local_ready,"model":if invalid {Value::Null} else {json!("deepseek-flash")},
            "thinking":self.foreground.selected.arm().ok(),"running":running,"tools":[],"startup_fixed":true,
            "readiness_basis":"local_client_and_credential_preflight_only",
            "stream_contract":if invalid {Value::Null}else{json!("metadata_v2")},
            "reply_text_origin":if invalid {Value::Null}else{json!("server_segment_concatenation")},
            "remote_verified_by_status_poll":false,"last_request":self.foreground.last.lock().unwrap().clone(),
            "error_code":self.foreground.error,
            "error":if self.foreground.local_ready {Value::Null} else {json!(UNAVAILABLE)}})
    }
    pub async fn infer_omni(&self, payload: Value) -> Result<Value> {
        ensure!(
            self.foreground.selected == Selection::Omni,
            "omni_not_selected"
        );
        self.foreground.ensure_ready()?;
        let request_id = s(&payload, "request_id");
        let learner_id = s(&payload, "learner_id");
        ensure!(
            !request_id.is_empty() && !learner_id.is_empty(),
            "omni_request_identity_required"
        );
        let service = self
            .foreground
            .omni
            .as_ref()
            .ok_or_else(|| anyhow!("omni_not_ready"))?
            .clone();
        let (_call, _, mut cancellation) = self.register_call(&request_id, &learner_id, false)?;
        let _permit = tokio::select! {
            biased;
            _ = cancellation.changed() => bail!("foreground_cancelled"),
            permit = self.gate.acquire() => permit?,
        };
        tokio::select! {
            biased;
            _ = cancellation.changed() => bail!("foreground_cancelled"),
            result = service.infer(payload) => result,
        }
    }
    pub(crate) async fn generate_flash_whole(&self, request: &Value) -> Result<Value> {
        let generation = request["generation"]
            .as_u64()
            .ok_or_else(|| anyhow!("教学请求缺少代次"))?;
        let (sender, mut receiver) = mpsc::channel(SEGMENT_CHANNEL_CAPACITY);
        let operation = self.generate_segments(request, generation, sender);
        let drain = async { while receiver.recv().await.is_some() {} };
        let (result, ()) = tokio::join!(operation, drain);
        Ok(result?.into_reply())
    }
    pub(super) async fn run_flash_stream(
        &self,
        request: &Value,
        generation: u64,
        sender: mpsc::Sender<TutorSegment>,
        mut cancellation: watch::Receiver<bool>,
        metrics: &mut Value,
    ) -> Result<ValidatedTutorReply> {
        if self.foreground.selected == Selection::Omni {
            let policy = String::from_utf8(read_bounded_file(
                &self.root.join("prompts/tutor_core.md"),
                32 * 1024,
            )?)
            .map_err(|_| anyhow!("teaching_policy_utf8_invalid"))?;
            let schema = serde_json::from_slice(&read_bounded_file(
                &self.root.join("implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json"),
                128 * 1024,
            )?)?;
            let prompt = complete_prompt(&policy, schema, request)?;
            let agent = String::from_utf8(read_bounded_file(
                &self.root.join("prompts/tutor-agent.md"),
                32 * 1024,
            )?)
            .map_err(|_| anyhow!("teaching_agent_utf8_invalid"))?
            .replace("\r\n", "\n");
            ensure!(agent.starts_with("---\n"), "teaching_agent_header_invalid");
            let system = agent[4..]
                .split_once("\n---\n")
                .ok_or_else(|| anyhow!("teaching_agent_body_missing"))?
                .1
                .trim();
            self.foreground.ensure_ready()?;
            ensure!(!*cancellation.borrow(), "foreground_cancelled");
            let service = self
                .foreground
                .omni
                .as_ref()
                .ok_or_else(|| anyhow!("omni_not_ready"))?
                .clone();
            self.increment("requests");
            self.increment("omni_requests");
            metrics["transport"] = json!("local_omni_http_complete");
            metrics["reply_text_origin"] =
                json!("local_complete_json_validated_server_sentence_split");
            metrics["omni_output_contract"] = json!("single_json_object");
            let payload = json!({"messages":[{"role":"system","content":system},{"role":"user","content":prompt}],
                "modalities":["text"],"max_tokens":2048});
            let response = tokio::select! {
                biased;
                _ = cancellation.changed() => bail!("foreground_cancelled"),
                response = service.infer(payload) => response?,
            };
            let content = response["choices"][0]["message"]["content"]
                .as_str()
                .ok_or_else(|| anyhow!("omni_empty_response"))?;
            let result = deepseek_client::complete_omni_reply(
                content,
                request,
                generation,
                &sender,
                &mut cancellation,
                metrics,
            )
            .await;
            self.increment(if result.is_ok() {
                "completed"
            } else if *cancellation.borrow() {
                "cancelled"
            } else {
                "errors"
            });
            return result;
        }
        #[cfg(windows)]
        {
            let contract = deepseek_client::StreamContract::MetadataV2;
            metrics["stream_contract"] = json!(contract.label());
            metrics["reply_text_origin"] = json!(contract.reply_text_origin());
            self.foreground.ensure_ready()?;
            let client = self
                .foreground
                .client
                .as_ref()
                .ok_or_else(|| anyhow!("foreground_http_client_unavailable"))?;
            let policy = String::from_utf8(read_bounded_file(
                &self.root.join("prompts/tutor_core.md"),
                32 * 1024,
            )?)
            .map_err(|_| anyhow!("teaching_policy_utf8_invalid"))?;
            let schema = serde_json::from_slice(&read_bounded_file(
                &self.root.join("implementation-package/AI_Tutor_Codex_Implementation_Package/schemas/tutor_reply.schema.json"),
                128 * 1024,
            )?)?;
            let prompt = stream_prompt_metadata(&policy, schema, request)?;
            let agent = String::from_utf8(read_bounded_file(
                &self.root.join("prompts/tutor-agent.md"),
                32 * 1024,
            )?)
            .map_err(|_| anyhow!("teaching_agent_utf8_invalid"))?
            .replace("\r\n", "\n");
            ensure!(agent.starts_with("---\n"), "teaching_agent_header_invalid");
            let system = agent[4..]
                .split_once("\n---\n")
                .ok_or_else(|| anyhow!("teaching_agent_body_missing"))?
                .1
                .trim();
            ensure!(!system.is_empty(), "teaching_agent_body_empty");
            ensure!(!*cancellation.borrow(), "请求已取消");
            self.increment("requests");
            self.increment("deepseek_segment_requests");
            let result = deepseek_client::stream(
                client,
                request,
                generation,
                system,
                &prompt,
                self.foreground.selected.arm()?,
                contract,
                sender,
                cancellation.clone(),
                metrics,
            )
            .await;
            self.increment(if result.is_ok() {
                "completed"
            } else if *cancellation.borrow() {
                "cancelled"
            } else {
                "errors"
            });
            result
        }
        #[cfg(not(windows))]
        {
            let _ = (request, generation, sender, cancellation, metrics);
            bail!("foreground_flash_requires_windows_dpapi")
        }
    }
    #[cfg(test)]
    pub(crate) fn select_scripted_flash_for_test(&mut self) {
        self.foreground = ForegroundRuntime::scripted_flash();
        let mut state = self.state.lock().unwrap();
        state.ready = false;
        state.audit["available"] = json!(false);
    }
}

#[cfg(test)]
#[path = "foreground_provider_tests.rs"]
mod tests;
