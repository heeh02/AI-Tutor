//! Fixed Flash transport. The caller owns provider selection, admission and lifecycle.
use super::*;
use axum::body::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::{
    header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE},
    Client,
};
use windows::Win32::{
    Foundation::{LocalFree, HLOCAL},
    Security::Cryptography::{CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB},
};

#[path = "deepseek_flash_sse.rs"]
mod sse;
use sse::SseDecoder;
use tokio::sync::{mpsc, watch};

pub(super) const ENDPOINT: &str = "https://api.deepseek.com/chat/completions";
pub(super) const MODEL: &str = "deepseek-flash";
pub(super) const MAX_TOKENS: u64 = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StreamContract {
    LegacyV1,
    MetadataV2,
}
impl StreamContract {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::LegacyV1 => "legacy_v1",
            Self::MetadataV2 => "metadata_v2",
        }
    }
    pub(super) fn reply_text_origin(self) -> &'static str {
        match self {
            Self::LegacyV1 => "model_exact_repeat",
            Self::MetadataV2 => "server_segment_concatenation",
        }
    }
    #[cfg(test)]
    pub(super) fn parse(value: &str) -> Result<Self> {
        match value {
            "legacy_v1" => Ok(Self::LegacyV1),
            "metadata_v2" => Ok(Self::MetadataV2),
            _ => bail!("unknown_stream_contract"),
        }
    }
    fn decoder(self, request: &Value, generation: u64) -> Result<SegmentDecoder> {
        match self {
            Self::LegacyV1 => SegmentDecoder::new(request, generation),
            Self::MetadataV2 => SegmentDecoder::new_metadata(request, generation),
        }
    }
}

fn wipe(bytes: &mut [u8]) {
    for byte in bytes {
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}
struct Plaintext(CRYPT_INTEGER_BLOB);
impl Drop for Plaintext {
    fn drop(&mut self) {
        if !self.0.pbData.is_null() {
            unsafe {
                wipe(std::slice::from_raw_parts_mut(
                    self.0.pbData,
                    self.0.cbData as usize,
                ));
                let _ = LocalFree(Some(HLOCAL(self.0.pbData.cast())));
            }
        }
    }
}
fn authorization_header() -> Result<HeaderValue> {
    let home = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| anyhow!("credential_directory_unavailable"))?;
    let path = PathBuf::from(home).join("AI Tutor/secrets/deepseek.dpapi");
    let mut encrypted =
        read_bounded_file(&path, 16 * 1024).map_err(|_| anyhow!("credential_read_failed"))?;
    ensure!(!encrypted.is_empty(), "credential_empty");
    let input = CRYPT_INTEGER_BLOB {
        cbData: encrypted.len() as u32,
        pbData: encrypted.as_mut_ptr(),
    };
    let mut plain = Plaintext(CRYPT_INTEGER_BLOB::default());
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut plain.0,
        )
    }
    .map_err(|_| anyhow!("credential_decrypt_failed"))?;
    ensure!(
        !plain.0.pbData.is_null() && (1..=512).contains(&plain.0.cbData),
        "credential_shape_invalid"
    );
    let key = unsafe { std::slice::from_raw_parts(plain.0.pbData, plain.0.cbData as usize) };
    ensure!(
        key.iter().all(u8::is_ascii_graphic),
        "credential_shape_invalid"
    );
    let mut bytes = b"Bearer ".to_vec();
    bytes.extend_from_slice(key);
    let header = HeaderValue::from_bytes(&bytes).map_err(|_| anyhow!("credential_header_invalid"));
    wipe(&mut bytes);
    let mut header = header?;
    header.set_sensitive(true);
    Ok(header)
}

/// Local credential shape/decryption only. No API authentication/model claim.
pub(super) fn credential_preflight() -> Result<()> {
    drop(authorization_header()?);
    Ok(())
}

fn bounded_identifier(value: &Value) -> Result<Option<String>> {
    if value.is_null() {
        return Ok(None);
    }
    let text = value
        .as_str()
        .ok_or_else(|| anyhow!("response_identifier_type"))?;
    ensure!(
        !text.is_empty() && text.len() <= 256 && !text.chars().any(char::is_control),
        "response_identifier_shape"
    );
    Ok(Some(text.into()))
}
fn numeric_usage(value: &Value) -> Result<Value> {
    ensure!(value.is_object(), "usage_shape");
    let mut result = json!({});
    for key in [
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "prompt_cache_hit_tokens",
        "prompt_cache_miss_tokens",
    ] {
        if let Some(value) = value.get(key) {
            ensure!(value.as_u64().is_some(), "usage_shape");
            result[key] = value.clone();
        }
    }
    for (group, fields) in [
        ("prompt_tokens_details", &["cached_tokens"][..]),
        ("completion_tokens_details", &["reasoning_tokens"][..]),
    ] {
        if let Some(detail) = value.get(group).filter(|v| !v.is_null()) {
            ensure!(detail.is_object(), "usage_shape");
            for key in fields {
                if let Some(value) = detail.get(*key) {
                    ensure!(value.as_u64().is_some(), "usage_shape");
                    result[group][*key] = value.clone();
                }
            }
        }
    }
    Ok(result)
}
struct ResponseState {
    decoder: SegmentDecoder,
    metrics: Value,
    #[cfg(test)]
    segments: Vec<Value>,
    finish: Option<String>,
    done: bool,
    #[cfg(test)]
    raw_visible: String,
    #[cfg(test)]
    raw_visible_observed: usize,
    #[cfg(test)]
    first_contract_error: Option<&'static str>,
    diagnostic: bool,
    expected_model: &'static str,
}
struct FrameContent {
    content: Option<String>,
    finish: Option<String>,
}
impl ResponseState {
    #[cfg(test)]
    fn new(request: &Value, generation: u64) -> Result<Self> {
        Self::with_mode(request, generation, true, StreamContract::LegacyV1)
    }
    fn production(request: &Value, generation: u64, contract: StreamContract) -> Result<Self> {
        Self::with_mode(request, generation, false, contract)
    }
    fn with_model(
        request: &Value,
        generation: u64,
        contract: StreamContract,
        expected_model: &'static str,
    ) -> Result<Self> {
        if expected_model == MODEL {
            return Self::production(request, generation, contract);
        }
        let mut state = Self::with_mode(request, generation, false, contract)?;
        state.expected_model = expected_model;
        Ok(state)
    }
    fn with_mode(
        request: &Value,
        generation: u64,
        diagnostic: bool,
        contract: StreamContract,
    ) -> Result<Self> {
        let mut metrics = json!({"first_content_ms":null,"first_reasoning_event_ms":null,"first_validated_segment_ms":null,
            "reasoning_chunks_discarded":0,"reasoning_bytes_discarded":0,"reported_model":null,"completion_id":null,"system_fingerprint":null,"usage":null,"finish_reason":null,"done_marker":false});
        if !diagnostic {
            for key in [
                "first_content_ms",
                "first_reasoning_event_ms",
                "reasoning_chunks_discarded",
                "reasoning_bytes_discarded",
            ] {
                metrics.as_object_mut().unwrap().remove(key);
            }
        }
        metrics["stream_contract"] = json!(contract.label());
        metrics["reply_text_origin"] = json!(contract.reply_text_origin());
        Ok(Self {
            decoder: contract.decoder(request, generation)?,
            metrics,
            #[cfg(test)]
            segments: vec![],
            finish: None,
            done: false,
            #[cfg(test)]
            raw_visible: String::new(),
            #[cfg(test)]
            raw_visible_observed: 0,
            #[cfg(test)]
            first_contract_error: None,
            diagnostic,
            expected_model: MODEL,
        })
    }
    fn identity(&mut self, frame: &Value, key: &str, target: &str) -> Result<()> {
        if let Some(value) = bounded_identifier(&frame[key])? {
            ensure!(
                self.metrics[target].is_null() || self.metrics[target] == value,
                "response_identity_changed"
            );
            self.metrics[target] = json!(value);
        }
        Ok(())
    }
    fn prepare_frame(&mut self, bytes: &[u8], elapsed_ms: f64) -> Result<Option<FrameContent>> {
        ensure!(!self.done, "data_after_done");
        if bytes == b"[DONE]" {
            ensure!(self.finish.as_deref() == Some("stop"), "done_without_stop");
            self.done = true;
            self.metrics["done_marker"] = json!(true);
            return Ok(None);
        }
        let frame: Value =
            serde_json::from_slice(bytes).map_err(|_| anyhow!("invalid_sse_json"))?;
        ensure!(
            frame.get("error").is_none_or(Value::is_null),
            "api_error_frame"
        );
        self.identity(&frame, "model", "reported_model")?;
        ensure!(
            self.metrics["reported_model"].is_null()
                || self.metrics["reported_model"] == self.expected_model,
            "response_model_mismatch"
        );
        self.identity(&frame, "id", "completion_id")?;
        self.identity(&frame, "system_fingerprint", "system_fingerprint")?;
        if !frame["usage"].is_null() {
            let mut usage = numeric_usage(&frame["usage"])?;
            if !self.diagnostic {
                usage
                    .as_object_mut()
                    .unwrap()
                    .remove("completion_tokens_details");
            }
            self.metrics["usage"] = usage;
        }
        let choices = frame["choices"]
            .as_array()
            .ok_or_else(|| anyhow!("choices_shape"))?;
        ensure!(choices.len() <= 1, "multiple_choices");
        let Some(choice) = choices.first() else {
            return Ok(None);
        };
        ensure!(choice["index"] == 0, "choice_index");
        let delta = &choice["delta"];
        ensure!(delta.is_object(), "delta_shape");
        ensure!(
            (delta["content"].is_null() || delta["content"].is_string())
                && (delta["reasoning_content"].is_null() || delta["reasoning_content"].is_string())
                && (choice["finish_reason"].is_null() || choice["finish_reason"].is_string()),
            "delta_content_shape"
        );
        ensure!(
            delta["role"].is_null() || delta["role"] == "assistant",
            "role_changed"
        );
        ensure!(
            delta["function_call"].is_null()
                && (delta["tool_calls"].is_null()
                    || delta["tool_calls"].as_array().is_some_and(Vec::is_empty)),
            "tool_output_forbidden"
        );
        if let Some(reasoning) = delta["reasoning_content"]
            .as_str()
            .filter(|v| !v.is_empty())
        {
            ensure!(self.finish.is_none(), "reasoning_after_finish");
            if self.diagnostic {
                if self.metrics["first_reasoning_event_ms"].is_null() {
                    self.metrics["first_reasoning_event_ms"] = json!(elapsed_ms);
                }
                self.metrics["reasoning_chunks_discarded"] =
                    json!(self.metrics["reasoning_chunks_discarded"].as_u64().unwrap() + 1);
                self.metrics["reasoning_bytes_discarded"] = json!(
                    self.metrics["reasoning_bytes_discarded"].as_u64().unwrap()
                        + reasoning.len() as u64
                );
            }
        }
        Ok(Some(FrameContent {
            content: delta["content"]
                .as_str()
                .filter(|v| !v.is_empty())
                .map(str::to_owned),
            finish: choice["finish_reason"].as_str().map(str::to_owned),
        }))
    }
    #[cfg(test)]
    fn consume(&mut self, bytes: &[u8], elapsed_ms: f64) -> Result<()> {
        ensure!(self.diagnostic, "diagnostic_consumer_required");
        let Some(frame) = self.prepare_frame(bytes, elapsed_ms)? else {
            return Ok(());
        };
        if let Some(content) = frame.content.as_deref() {
            let retain_prefix = self.raw_visible.len() == self.raw_visible_observed;
            self.raw_visible_observed += content.len();
            if retain_prefix {
                let mut keep = content
                    .len()
                    .min(MAX_OUTPUT_BYTES.saturating_sub(self.raw_visible.len()));
                while !content.is_char_boundary(keep) {
                    keep -= 1;
                }
                self.raw_visible.push_str(&content[..keep]);
            }
            ensure!(self.finish.is_none(), "content_after_finish");
            if self.metrics["first_content_ms"].is_null() {
                self.metrics["first_content_ms"] = json!(elapsed_ms);
            }
            if self.first_contract_error.is_none() {
                // One completed line per push: a later bad record must not
                // discard earlier successfully returned segments in this delta.
                for piece in content.as_bytes().split_inclusive(|byte| *byte == b'\n') {
                    match self.decoder.push(piece) {
                        Ok(decoded) => {
                            for segment in decoded {
                                self.segments.push(json!({"sequence":segment.sequence(),"text":segment.text(),"used_evidence_ids":segment.used_evidence_ids(),"received_from_invocation_ms":null}));
                            }
                        }
                        Err(error) => {
                            // Only a typed tutor-contract failure permits
                            // draining. Unknown/internal failures still abort.
                            let Some(code) = contract_failure_code(&error) else {
                                return Err(anyhow!("segment_contract_invalid"));
                            };
                            self.first_contract_error = Some(code);
                            self.metrics["contract_failure_code"] = json!(code);
                            self.metrics["contract_failure_phase"] = json!("push");
                            break;
                        }
                    }
                }
            }
        }
        self.finish_frame(frame.finish.as_deref())
    }
    fn finish_frame(&mut self, reason: Option<&str>) -> Result<()> {
        if let Some(reason) = reason {
            ensure!(self.finish.is_none(), "duplicate_finish");
            self.metrics["finish_reason"] = json!(if [
                "stop",
                "length",
                "tool_calls",
                "content_filter",
                "insufficient_system_resource"
            ]
            .contains(&reason)
            {
                reason
            } else {
                "unrecognized"
            });
            self.finish = Some(reason.into());
            ensure!(reason == "stop", "non_stop_finish");
        }
        Ok(())
    }
    #[cfg(test)]
    fn observations(&self, row: &mut Value) {
        row["metrics"] = self.metrics.clone();
        row["validated_segments"] = json!(self.segments);
        row["collected_text"] = json!(self
            .segments
            .iter()
            .map(|v| s(v, "text"))
            .collect::<String>());
        row["contract_diagnostics"] = self.decoder.diagnostics();
        row["contract_failure_code"] = self.metrics["contract_failure_code"].clone();
        row["contract_failure_phase"] = self.metrics["contract_failure_phase"].clone();
        row["raw_visible_assistant_content"] = json!(self.raw_visible);
        row["raw_visible_bytes_observed"] = json!(self.raw_visible_observed);
        row["raw_visible_truncated"] = json!(self.raw_visible.len() < self.raw_visible_observed);
        row["raw_visible_scope"] =
            json!("Unvalidated delta.content only; never reasoning_content or API error bodies");
    }
    #[cfg(test)]
    fn complete(self, row: &mut Value) -> Result<ValidatedTutorReply> {
        self.observations(row);
        ensure!(
            self.done && self.finish.as_deref() == Some("stop"),
            "incomplete_api_stream"
        );
        ensure!(
            self.metrics["reported_model"] == self.expected_model,
            "response_model_mismatch_or_absent"
        );
        if let Some(code) = self.first_contract_error {
            return Err(anyhow!(code));
        }
        self.decoder.finish_clean_end().map_err(|error| {
            let Some(code) = contract_failure_code(&error) else {
                return anyhow!("final_contract_invalid");
            };
            row["contract_failure_code"] = json!(code);
            row["contract_failure_phase"] = json!("finish");
            row["metrics"]["contract_failure_code"] = json!(code);
            row["metrics"]["contract_failure_phase"] = json!("finish");
            anyhow!(code)
        })
    }
}

#[cfg(test)]
fn consume_wire_chunk(
    state: &mut ResponseState,
    framing: &mut SseDecoder,
    chunk: &[u8],
    started: &Instant,
) -> Result<()> {
    // A framing error on a later line must not erase completed API frames
    // already received earlier in this same HTTP chunk.
    for piece in chunk.split_inclusive(|byte| *byte == b'\n') {
        for payload in framing
            .push(piece)
            .map_err(|_| anyhow!("sse_framing_invalid"))?
        {
            let previous = state.segments.len();
            let decoded = state.consume(&payload, started.elapsed().as_secs_f64() * 1000.0);
            let validated_at = started.elapsed().as_secs_f64() * 1000.0;
            if state.segments.len() > previous {
                if state.metrics["first_validated_segment_ms"].is_null() {
                    state.metrics["first_validated_segment_ms"] = json!(validated_at);
                }
                for segment in &mut state.segments[previous..] {
                    segment["received_from_invocation_ms"] = json!(validated_at);
                }
            }
            decoded?;
        }
    }
    Ok(())
}

#[cfg(test)]
fn finish_call(
    state: ResponseState,
    row: &mut Value,
    transport: Result<()>,
) -> Result<ValidatedTutorReply> {
    // EOF alone is insufficient: framing.finish can still fail after it, even
    // when an earlier model error and stop/DONE metadata have been retained.
    row["api_stream_complete"] = json!(
        transport.is_ok()
            && row["http_eof"] == true
            && state.done
            && state.finish.as_deref() == Some("stop")
            && state.metrics["reported_model"] == state.expected_model
    );
    if let Err(error) = transport {
        state.observations(row);
        return Err(error);
    }
    state.complete(row)
}

fn request_body(system: &str, user: &str, arm: &str) -> Result<Value> {
    ensure!(
        ["non_thinking", "thinking_low"].contains(&arm),
        "unknown_arm"
    );
    let mut body = json!({"model":MODEL,"messages":[{"role":"system","content":system},{"role":"user","content":user}],
        "stream":true,"stream_options":{"include_usage":true},"max_tokens":MAX_TOKENS,
        "thinking":{"type":if arm=="non_thinking"{"disabled"}else{"enabled"}}});
    if arm == "thinking_low" {
        body["reasoning_effort"] = json!("low");
    }
    Ok(body)
}
pub(super) fn new_client() -> Result<Client> {
    Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| anyhow!("http_client_failed"))
}

fn http_request(client: &Client, system: &str, user: &str, arm: &str) -> Result<reqwest::Request> {
    client
        .post(ENDPOINT)
        .header(AUTHORIZATION, authorization_header()?)
        .json(&request_body(system, user, arm)?)
        .build()
        .map_err(|_| anyhow!("request_build_failed"))
}

fn response_headers(response: &reqwest::Response, metrics: &mut Value) -> Result<()> {
    metrics["http_status"] = json!(response.status().as_u16());
    for name in ["x-request-id", "x-ds-request-id", "request-id"] {
        if let Some(value) = response.headers().get(name).and_then(|h| h.to_str().ok()) {
            metrics["http_request_ids"][name] = json!(bounded_identifier(&json!(value))?);
        }
    }
    ensure!(response.status().is_success(), "http_status_failure");
    ensure!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().starts_with("text/event-stream")),
        "not_event_stream"
    );
    Ok(())
}

fn production_preflight(
    request: &Value,
    generation: u64,
    sender: &mpsc::Sender<TutorSegment>,
    cancel: &watch::Receiver<bool>,
) -> Result<()> {
    ensure!(
        request["generation"].as_u64() == Some(generation),
        "request_generation_mismatch"
    );
    ensure!(
        sender.max_capacity() <= SEGMENT_CHANNEL_CAPACITY,
        "segment_channel_capacity"
    );
    ensure!(!*cancel.borrow(), "foreground_cancelled");
    ensure!(!sender.is_closed(), "consumer_closed");
    Ok(())
}

fn production_metadata(state: &ResponseState, metrics: &mut Value) {
    // Preserve the caller's request/generation/transport/admission fields.
    // No raw-visible fields, first-content/thought timing or reasoning counters.
    for key in [
        "reported_model",
        "completion_id",
        "system_fingerprint",
        "usage",
        "finish_reason",
        "done_marker",
        "stream_contract",
        "reply_text_origin",
    ] {
        metrics[key] = state.metrics[key].clone();
    }
}

fn production_failure(error: &anyhow::Error, metrics: &mut Value) {
    const CODES: &[&str] = &[
        "foreground_cancelled",
        "consumer_closed",
        "request_deadline_exceeded",
        "request_generation_mismatch",
        "segment_channel_capacity",
        "unknown_arm",
        "credential_directory_unavailable",
        "credential_read_failed",
        "credential_empty",
        "credential_decrypt_failed",
        "credential_shape_invalid",
        "credential_header_invalid",
        "request_build_failed",
        "http_transport_failed",
        "http_body_failed",
        "http_status_failure",
        "not_event_stream",
        "sse_framing_invalid",
        "truncated_sse_frame",
        "incomplete_api_stream",
        "response_model_mismatch_or_absent",
        "response_model_mismatch",
        "response_identity_changed",
        "response_identifier_type",
        "response_identifier_shape",
        "api_error_frame",
        "invalid_sse_json",
        "choices_shape",
        "multiple_choices",
        "choice_index",
        "delta_shape",
        "delta_content_shape",
        "role_changed",
        "tool_output_forbidden",
        "reasoning_after_finish",
        "content_after_finish",
        "duplicate_finish",
        "non_stop_finish",
        "data_after_done",
        "done_without_stop",
        "usage_shape",
    ];
    let code = contract_failure_code(error)
        .or_else(|| {
            let message = error.to_string();
            CODES.iter().copied().find(|code| *code == message)
        })
        .unwrap_or("foreground_operation_failed");
    metrics["failure_code"] = json!(code);
}

/// In-memory byte streams exercise the same parsing/publication path as HTTP.
/// This helper never prepares a credential, request or independent admission gate.
#[cfg(test)]
async fn production_chunks<S>(
    chunks: S,
    request: &Value,
    generation: u64,
    sender: &mpsc::Sender<TutorSegment>,
    cancel: &mut watch::Receiver<bool>,
    metrics: &mut Value,
    started: &Instant,
    deadline: tokio::time::Instant,
) -> Result<ValidatedTutorReply>
where
    S: Stream<Item = Result<Bytes>>,
{
    production_chunks_with_contract(
        chunks,
        request,
        generation,
        StreamContract::LegacyV1,
        sender,
        cancel,
        metrics,
        started,
        deadline,
    )
    .await
}

async fn production_chunks_with_contract<S>(
    chunks: S,
    request: &Value,
    generation: u64,
    contract: StreamContract,
    sender: &mpsc::Sender<TutorSegment>,
    cancel: &mut watch::Receiver<bool>,
    metrics: &mut Value,
    started: &Instant,
    deadline: tokio::time::Instant,
) -> Result<ValidatedTutorReply>
where
    S: Stream<Item = Result<Bytes>>,
{
    production_chunks_for_model(
        chunks, request, generation, contract, MODEL, sender, cancel, metrics, started, deadline,
    )
    .await
}

async fn production_chunks_for_model<S>(
    chunks: S,
    request: &Value,
    generation: u64,
    contract: StreamContract,
    expected_model: &'static str,
    sender: &mpsc::Sender<TutorSegment>,
    cancel: &mut watch::Receiver<bool>,
    metrics: &mut Value,
    started: &Instant,
    deadline: tokio::time::Instant,
) -> Result<ValidatedTutorReply>
where
    S: Stream<Item = Result<Bytes>>,
{
    production_preflight(request, generation, sender, cancel)?;
    let mut state = ResponseState::with_model(request, generation, contract, expected_model)?;
    let mut framing = SseDecoder::default();
    metrics["segments_enqueued"] = json!(0);
    metrics["http_eof"] = json!(false);
    metrics["api_stream_complete"] = json!(false);
    metrics["first_validated_segment_ms"] = Value::Null;
    metrics["first_segment_enqueued_ms"] = Value::Null;
    let result = async {
        futures_util::pin_mut!(chunks);
        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.changed() => return Err(anyhow!("foreground_cancelled")),
                _ = sender.closed() => return Err(anyhow!("consumer_closed")),
                _ = tokio::time::sleep_until(deadline) => return Err(anyhow!("request_deadline_exceeded")),
                next = chunks.next() => next,
            };
            let Some(chunk) = next else { break };
            let chunk = chunk?;
            if metrics["first_wire_chunk_ms"].is_null() {
                metrics["first_wire_chunk_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
            }
            for piece in chunk.split_inclusive(|byte| *byte == b'\n') {
                ensure!(!*cancel.borrow(), "foreground_cancelled");
                ensure!(!sender.is_closed(), "consumer_closed");
                ensure!(tokio::time::Instant::now() < deadline, "request_deadline_exceeded");
                for payload in framing.push(piece).map_err(|_| anyhow!("sse_framing_invalid"))? {
                    let Some(frame) = state.prepare_frame(&payload, started.elapsed().as_secs_f64() * 1000.0)? else { continue };
                    if let Some(content) = frame.content.as_deref() {
                        // A later identity check cannot authorize an already
                        // published prefix. Earlier valid metadata may bind it.
                        ensure!(state.metrics["reported_model"] == state.expected_model, "response_model_mismatch_or_absent");
                        ensure!(state.finish.is_none(), "content_after_finish");
                        // Publish each returned, typed segment before parsing a
                        // later record in the same API content delta.
                        for line in content.as_bytes().split_inclusive(|byte| *byte == b'\n') {
                            ensure!(!*cancel.borrow(), "foreground_cancelled");
                            ensure!(tokio::time::Instant::now() < deadline, "request_deadline_exceeded");
                            let decoded = state.decoder.push(line)?;
                            for segment in decoded {
                                let validated = started.elapsed().as_secs_f64() * 1000.0;
                                if metrics["first_validated_segment_ms"].is_null() {
                                    metrics["first_validated_segment_ms"] = json!(validated);
                                }
                                tokio::select! {
                                    biased;
                                    _ = cancel.changed() => return Err(anyhow!("foreground_cancelled")),
                                    _ = sender.closed() => return Err(anyhow!("consumer_closed")),
                                    _ = tokio::time::sleep_until(deadline) => return Err(anyhow!("request_deadline_exceeded")),
                                    sent = sender.send(segment) => sent.map_err(|_| anyhow!("consumer_closed"))?,
                                }
                                metrics["segments_enqueued"] = json!(metrics["segments_enqueued"].as_u64().unwrap() + 1);
                                if metrics["first_segment_enqueued_ms"].is_null() {
                                    metrics["first_segment_enqueued_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
                                }
                            }
                        }
                    }
                    state.finish_frame(frame.finish.as_deref())?;
                }
            }
        }
        metrics["http_eof"] = json!(true);
        framing.finish().map_err(|_| anyhow!("truncated_sse_frame"))?;
        ensure!(state.done && state.finish.as_deref() == Some("stop"), "incomplete_api_stream");
        ensure!(state.metrics["reported_model"] == state.expected_model, "response_model_mismatch_or_absent");
        metrics["api_stream_complete"] = json!(true);
        Ok::<(), anyhow::Error>(())
    }.await;
    production_metadata(&state, metrics);
    result?;
    // Production propagates the original typed ContractError unchanged.
    state.decoder.finish_clean_end()
}

fn split_complete_omni_text(text: &str) -> Result<Vec<String>> {
    ensure!(
        !text.trim().is_empty() && text.chars().count() <= 1600,
        "omni_reply_text_invalid"
    );
    let mut remaining = text;
    let mut segments = Vec::new();
    while !remaining.is_empty() {
        if remaining.chars().count() <= MAX_SEGMENT_CHARS {
            segments.push(remaining.to_owned());
            break;
        }
        let cutoff = remaining
            .char_indices()
            .nth(MAX_SEGMENT_CHARS)
            .map(|(index, _)| index)
            .unwrap_or(remaining.len());
        let candidate = &remaining[..cutoff];
        let boundary = candidate
            .char_indices()
            .filter(|(_, character)| {
                matches!(
                    character,
                    '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n' | '.'
                )
            })
            .map(|(index, character)| index + character.len_utf8())
            .last()
            .or_else(|| {
                candidate
                    .char_indices()
                    .filter(|(_, character)| character.is_whitespace())
                    .map(|(index, character)| index + character.len_utf8())
                    .last()
            })
            .filter(|boundary| *boundary >= cutoff / 2)
            .unwrap_or(cutoff);
        segments.push(remaining[..boundary].to_owned());
        remaining = &remaining[boundary..];
        ensure!(
            segments.len() < MAX_SEGMENTS || remaining.is_empty(),
            "omni_reply_segments_exceeded"
        );
    }
    ensure!(
        !segments.is_empty()
            && segments.len() <= MAX_SEGMENTS
            && segments.iter().all(|segment| {
                !segment.trim().is_empty() && segment.chars().count() <= MAX_SEGMENT_CHARS
            })
            && segments.concat() == text,
        "omni_reply_segmentation_invalid"
    );
    Ok(segments)
}

fn complete_omni_records(content: &str, request: &Value) -> Result<String> {
    let reply: Value =
        serde_json::from_str(content).map_err(|_| anyhow!("omni_tutor_json_invalid"))?;
    validate_reply(&reply, request)?;
    let text = reply["reply_text"]
        .as_str()
        .ok_or_else(|| anyhow!("omni_reply_text_missing"))?;
    let segments = split_complete_omni_text(text)?;
    let mut metadata = reply.clone();
    metadata.as_object_mut().unwrap().remove("reply_text");
    let request_id = request["request_id"]
        .as_str()
        .ok_or_else(|| anyhow!("omni_request_id_missing"))?;
    let mut records = String::new();
    for (sequence, text) in segments.iter().enumerate() {
        records.push_str(
            &json!({"schema_version":"1.0","type":"segment","request_id":request_id,
            "sequence":sequence,"text":text,"used_evidence_ids":[]})
            .to_string(),
        );
        records.push('\n');
    }
    records.push_str(
        &json!({"schema_version":"1.0","type":"final_metadata","request_id":request_id,
        "segment_count":segments.len(),"reply":metadata})
        .to_string(),
    );
    records.push('\n');
    Ok(records)
}

pub(super) async fn complete_omni_reply(
    content: &str,
    request: &Value,
    generation: u64,
    sender: &mpsc::Sender<TutorSegment>,
    cancel: &mut watch::Receiver<bool>,
    metrics: &mut Value,
) -> Result<ValidatedTutorReply> {
    let records = complete_omni_records(content, request)?;
    complete_omni(&records, request, generation, sender, cancel, metrics).await
}

pub(super) async fn complete_omni(
    content: &str,
    request: &Value,
    generation: u64,
    sender: &mpsc::Sender<TutorSegment>,
    cancel: &mut watch::Receiver<bool>,
    metrics: &mut Value,
) -> Result<ValidatedTutorReply> {
    let frame = serde_json::to_vec(&json!({"id":"local_complete","model":OMNI_MODEL,
        "choices":[{"index":0,"delta":{"content":content},"finish_reason":"stop"}]}))?;
    let mut wire = b"data: ".to_vec();
    wire.extend(frame);
    wire.extend(b"\n\ndata: [DONE]\n\n");
    let chunks = futures_util::stream::once(async { Ok(Bytes::from(wire)) });
    let started = Instant::now();
    production_chunks_for_model(
        chunks,
        request,
        generation,
        StreamContract::MetadataV2,
        OMNI_MODEL,
        sender,
        cancel,
        metrics,
        &started,
        tokio::time::Instant::from_std(started + Duration::from_secs(300)),
    )
    .await
}

/// Production streaming only; provider ownership and the single shared gate
/// remain in the caller. Cancellation drops this local HTTP operation, not a
/// claim that remote server computation was stopped.
pub(super) async fn stream(
    client: &Client,
    request: &Value,
    generation: u64,
    system: &str,
    user: &str,
    arm: &str,
    contract: StreamContract,
    sender: mpsc::Sender<TutorSegment>,
    mut cancel: watch::Receiver<bool>,
    metrics: &mut Value,
) -> Result<ValidatedTutorReply> {
    let started = Instant::now();
    let deadline = tokio::time::Instant::from_std(started + Duration::from_secs(90));
    metrics["time_origin"] = json!("call_invocation_before_request_preparation");
    metrics["stream_contract"] = json!(contract.label());
    metrics["reply_text_origin"] = json!(contract.reply_text_origin());
    metrics["request_attempted"] = json!(false);
    metrics["http_eof"] = json!(false);
    metrics["api_stream_complete"] = json!(false);
    metrics["http_request_ids"] = json!({});
    let result = async {
        production_preflight(request, generation, &sender, &cancel)?;
        ensure!(["non_thinking", "thinking_low"].contains(&arm), "unknown_arm");
        contract.decoder(request, generation)?;
        metrics["phase"] = json!("prepare");
        let http = http_request(client, system, user, arm)?;
        metrics["request_prepared_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
        let response = {
            let send = async {
                metrics["request_attempted"] = json!(true);
                metrics["http_send_offset_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
                client.execute(http).await.map_err(|_| anyhow!("http_transport_failed"))
            };
            tokio::select! {
                biased;
                _ = cancel.changed() => return Err(anyhow!("foreground_cancelled")),
                _ = sender.closed() => return Err(anyhow!("consumer_closed")),
                _ = tokio::time::sleep_until(deadline) => return Err(anyhow!("request_deadline_exceeded")),
                response = send => response?,
            }
        };
        response_headers(&response, metrics)?;
        metrics["headers_received_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
        metrics["phase"] = json!("stream");
        production_chunks_with_contract(
            response.bytes_stream().map(|v| v.map_err(|_| anyhow!("http_body_failed"))),
            request, generation, contract, &sender, &mut cancel, metrics, &started, deadline,
        ).await
    }.await;
    metrics["total_duration_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    if let Err(error) = &result {
        production_failure(error, metrics);
    }
    result
}

#[cfg(test)]
pub(super) async fn call(
    client: &Client,
    request: &Value,
    system: &str,
    user: &str,
    arm: &str,
    row: &mut Value,
) -> Result<ValidatedTutorReply> {
    call_with_contract(
        client,
        request,
        system,
        user,
        arm,
        StreamContract::LegacyV1,
        row,
    )
    .await
}

#[cfg(test)]
pub(super) async fn call_with_contract(
    client: &Client,
    request: &Value,
    system: &str,
    user: &str,
    arm: &str,
    contract: StreamContract,
    row: &mut Value,
) -> Result<ValidatedTutorReply> {
    let started = Instant::now();
    row["stream_contract"] = json!(contract.label());
    row["reply_text_origin"] = json!(contract.reply_text_origin());
    row["http_eof"] = json!(false);
    row["api_stream_complete"] = json!(false);
    row["time_origin"] = json!("call_invocation_before_request_preparation");
    let generation = request["generation"]
        .as_u64()
        .ok_or_else(|| anyhow!("request_generation_missing"))?;
    let mut state = ResponseState::with_mode(request, generation, true, contract)?;
    let request = http_request(client, system, user, arm)?;
    row["request_prepared_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    let transport = async {
        row["request_attempted"] = json!(true);
        row["http_send_offset_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
        let mut response = client
            .execute(request)
            .await
            .map_err(|_| anyhow!("http_transport_failed"))?;
        response_headers(&response, row)?;
        row["headers_received_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
        let mut framing = SseDecoder::default();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow!("http_body_failed"))?
        {
            if row["first_wire_chunk_ms"].is_null() {
                row["first_wire_chunk_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
            }
            consume_wire_chunk(&mut state, &mut framing, &chunk, &started)?;
        }
        row["http_eof"] = json!(true);
        framing
            .finish()
            .map_err(|_| anyhow!("truncated_sse_frame"))?;
        ensure!(
            state.done && state.finish.as_deref() == Some("stop"),
            "missing_terminal_marker"
        );
        Ok::<(), anyhow::Error>(())
    };
    let remaining = Duration::from_secs(90)
        .checked_sub(started.elapsed())
        .ok_or_else(|| anyhow!("request_deadline_exceeded"))?;
    let result = tokio::time::timeout(remaining, transport)
        .await
        .map_err(|_| anyhow!("request_deadline_exceeded"))
        .and_then(|v| v);
    row["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    finish_call(state, row, result)
}
#[test]
fn request_arms_are_fixed_no_tools_and_no_default_high_reasoning() -> Result<()> {
    let off = request_body("same system", "same user", "non_thinking")?;
    let low = request_body("same system", "same user", "thinking_low")?;
    assert_eq!(off["messages"], low["messages"]);
    assert_eq!(off["thinking"]["type"], "disabled");
    assert!(off.get("reasoning_effort").is_none());
    assert_eq!(low["thinking"]["type"], "enabled");
    assert_eq!(low["reasoning_effort"], "low");
    assert!(off.get("tools").is_none() && low.get("tools").is_none());
    Ok(())
}
#[test]
fn reasoning_is_never_fed_to_tutor_decoder_and_stop_is_not_done() -> Result<()> {
    let request = json!({"request_id":"fixture","context":{"evidence":[]}});
    let final_reply = json!({"schema_version":"1.0","request_id":"fixture","reply_text":"Check one variable.","teaching_move":"give_hint","task_update_candidates":[],"memory_candidates":[],"used_evidence_ids":[],"needs_clarification":false});
    let content = format!(
        "{}\n{}\n",
        json!({"schema_version":"1.0","type":"segment","request_id":"fixture","sequence":0,"text":"Check one variable.","used_evidence_ids":[]}),
        json!({"schema_version":"1.0","type":"final","request_id":"fixture","segment_count":1,"reply":final_reply})
    );
    for done in [false, true] {
        let mut state = ResponseState::new(&request, 1)?;
        state.consume(&serde_json::to_vec(&json!({"model":MODEL,"choices":[{"index":0,"delta":{"reasoning_content":"PRIVATE_REASONING_CANARY_NOT_JSON"},"finish_reason":null}]}))?,1.0)?;
        assert!(state.segments.is_empty());
        assert!(!state.metrics.to_string().contains("PRIVATE_REASONING"));
        state.consume(&serde_json::to_vec(&json!({"model":MODEL,"choices":[{"index":0,"delta":{"content":content},"finish_reason":"stop"}]}))?,2.0)?;
        assert!(!state.done);
        state.consume(&serde_json::to_vec(&json!({"model":MODEL,"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7}}))?,3.0)?;
        assert_eq!(state.metrics["usage"]["total_tokens"], 7);
        if done {
            state.consume(b"[DONE]", 4.0)?;
        }
        assert_eq!(state.complete(&mut json!({})).is_ok(), done);
    }
    Ok(())
}

#[test]
fn wrong_model_or_premature_done_cannot_pass_a_flash_call() -> Result<()> {
    let request = json!({"request_id":"fixture","context":{"evidence":[]}});
    let mut wrong = ResponseState::new(&request, 1)?;
    assert!(wrong
        .consume(
            &serde_json::to_vec(&json!({"model":"deepseek-pro","choices":[]}))?,
            1.0
        )
        .is_err());
    assert_eq!(wrong.metrics["reported_model"], "deepseek-pro");
    assert!(wrong.complete(&mut json!({})).is_err());
    let mut early = ResponseState::new(&request, 1)?;
    assert!(early.consume(b"[DONE]", 1.0).is_err());
    Ok(())
}

#[test]
fn malformed_optional_fields_fail_instead_of_being_silently_ignored() -> Result<()> {
    let request = json!({"request_id":"fixture","context":{"evidence":[]}});
    for (delta, finish, usage) in [
        (json!({"content":false}), Value::Null, Value::Null),
        (
            json!({"reasoning_content":{"text":"PRIVATE_CANARY"}}),
            Value::Null,
            Value::Null,
        ),
        (json!({}), json!(7), Value::Null),
        (json!({}), Value::Null, json!(false)),
    ] {
        let mut state = ResponseState::new(&request, 1)?;
        assert!(state.consume(&serde_json::to_vec(&json!({"model":MODEL,"choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":usage}))?,1.0).is_err());
        assert!(state.segments.is_empty());
        assert!(!state.metrics.to_string().contains("PRIVATE_CANARY"));
    }
    Ok(())
}

#[test]
fn complete_invalid_final_retains_visible_record_and_typed_push_or_finish_cause() -> Result<()> {
    let request = json!({"request_id":"fixture","context":{"evidence":[]}});
    let segment = format!(
        "{}\n",
        json!({"schema_version":"1.0","type":"segment","request_id":"fixture","sequence":0,"text":"Check one variable.","used_evidence_ids":[]})
    );
    let final_record=json!({"schema_version":"1.0","type":"final","request_id":"fixture","segment_count":1,
        "reply":{"schema_version":"1.0","request_id":"fixture","reply_text":"A different sentence.","teaching_move":"give_hint","task_update_candidates":[],"memory_candidates":[],"used_evidence_ids":[],"needs_clarification":false}}).to_string();
    for newline in [false, true] {
        let mut state = ResponseState::new(&request, 1)?;
        state.consume(&serde_json::to_vec(&json!({"model":MODEL,"choices":[{"index":0,"delta":{"reasoning_content":"PRIVATE_REASONING_CANARY"},"finish_reason":null}]}))?,1.0)?;
        state.consume(&serde_json::to_vec(&json!({"model":MODEL,"choices":[{"index":0,"delta":{"content":segment},"finish_reason":null}]}))?,2.0)?;
        let tail = format!("{final_record}{}", if newline { "\n" } else { "" });
        state.consume(&serde_json::to_vec(&json!({"model":MODEL,"choices":[{"index":0,"delta":{"content":tail},"finish_reason":"stop"}]}))?,3.0)?;
        let mut row = json!({});
        if !newline {
            assert_eq!(
                state.decoder.diagnostics()["pending_tail_shape"],
                "complete_single_json"
            );
        }
        state.consume(b"[DONE]", 4.0)?;
        assert!(state.complete(&mut row).is_err());
        assert_eq!(
            row["raw_visible_assistant_content"],
            format!("{segment}{tail}")
        );
        assert_eq!(row["raw_visible_truncated"], false);
        assert_eq!(row["contract_failure_code"], "final_text_mismatch");
        assert_eq!(
            row["contract_failure_phase"],
            if newline { "push" } else { "finish" }
        );
        assert!(!row.to_string().contains("PRIVATE_REASONING_CANARY"));
        assert_eq!(row["validated_segments"].as_array().unwrap().len(), 1);
    }
    Ok(())
}

#[cfg(test)]
fn drain_fixture() -> (Value, String, String) {
    let request = json!({"request_id":"drain-fixture","context":{"evidence":[]}});
    let segment = format!(
        "{}\n",
        json!({"schema_version":"1.0","type":"segment","request_id":"drain-fixture","sequence":0,"text":"Check one variable.","used_evidence_ids":[]})
    );
    let final_record = format!(
        "{}\n",
        json!({"schema_version":"1.0","type":"final","request_id":"drain-fixture","segment_count":1,
        "reply":{"schema_version":"1.0","request_id":"drain-fixture","reply_text":"A different sentence.","teaching_move":"give_hint","task_update_candidates":[],"memory_candidates":[],"used_evidence_ids":[],"needs_clarification":false}})
    );
    (request, segment, final_record)
}

#[cfg(test)]
fn drain_frame(delta: Value, finish: Value) -> Vec<u8> {
    format!(
        "data: {}\n\n",
        json!({"model":MODEL,"choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
    )
    .into_bytes()
}

#[test]
fn same_content_chunk_preserves_prefix_and_drains_usage_done_without_losing_first_error(
) -> Result<()> {
    let (request, segment, final_record) = drain_fixture();
    let bad_batch = format!("{segment}{final_record}");
    let after = "not another tutor record after the first failure\n";
    let mut wire = drain_frame(json!({"content":bad_batch}), Value::Null);
    wire.extend(drain_frame(
        json!({"content":after,"reasoning_content":"PRIVATE_DRAIN_REASONING"}),
        Value::Null,
    ));
    wire.extend(drain_frame(json!({}), json!("stop")));
    wire.extend(format!("data: {}\n\n", json!({"model":MODEL,"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7}})).as_bytes());
    wire.extend(b"data: [DONE]\n\n");
    let mut state = ResponseState::new(&request, 1)?;
    let mut framing = SseDecoder::default();
    consume_wire_chunk(&mut state, &mut framing, &wire, &Instant::now())?;
    framing.finish()?;
    let mut row = json!({"http_eof":true});
    let error = finish_call(state, &mut row, Ok(())).unwrap_err();
    assert_eq!(error.to_string(), "final_text_mismatch");
    assert_eq!(row["contract_failure_code"], "final_text_mismatch");
    assert_eq!(row["contract_failure_phase"], "push");
    assert_eq!(row["api_stream_complete"], true);
    assert_eq!(row["metrics"]["done_marker"], true);
    assert_eq!(row["metrics"]["usage"]["total_tokens"], 7);
    assert_eq!(row["validated_segments"].as_array().unwrap().len(), 1);
    assert!(row["metrics"]["first_validated_segment_ms"].is_number());
    assert_eq!(
        row["raw_visible_assistant_content"],
        format!("{bad_batch}{after}")
    );
    assert_eq!(row["metrics"]["reasoning_chunks_discarded"], 1);
    assert!(!row.to_string().contains("PRIVATE_DRAIN_REASONING"));
    Ok(())
}

#[test]
fn infrastructure_after_model_error_never_claims_a_clean_api_stream() -> Result<()> {
    for scenario in ["malformed_api", "timeout", "truncated_after_done"] {
        let (request, segment, final_record) = drain_fixture();
        let mut state = ResponseState::new(&request, 1)?;
        let mut framing = SseDecoder::default();
        let started = Instant::now();
        consume_wire_chunk(
            &mut state,
            &mut framing,
            &drain_frame(
                json!({"content":format!("{segment}{final_record}")}),
                json!("stop"),
            ),
            &started,
        )?;
        let mut row = json!({"http_eof":false});
        let transport = match scenario {
            "malformed_api" => consume_wire_chunk(
                &mut state,
                &mut framing,
                &drain_frame(json!({"content":17}), Value::Null),
                &started,
            ),
            // Inject exactly the transport timeout result consumed by the same
            // finish_call helper used by call(); no network/timer mock framework.
            "timeout" => Err(anyhow!("request_deadline_exceeded")),
            _ => {
                consume_wire_chunk(&mut state, &mut framing, b"data: [DONE]\n\ndata:", &started)?;
                row["http_eof"] = json!(true);
                framing.finish().map_err(|_| anyhow!("truncated_sse_frame"))
            }
        };
        let expected = transport.as_ref().unwrap_err().to_string();
        let error = finish_call(state, &mut row, transport).unwrap_err();
        assert_eq!(error.to_string(), expected);
        assert_ne!(expected, "final_text_mismatch");
        assert_eq!(row["contract_failure_code"], "final_text_mismatch");
        assert_eq!(row["api_stream_complete"], false);
        assert_eq!(row["validated_segments"].as_array().unwrap().len(), 1);
        if scenario == "truncated_after_done" {
            assert_eq!(row["http_eof"], true);
            assert_eq!(row["metrics"]["done_marker"], true);
            assert_eq!(row["metrics"]["finish_reason"], "stop");
            assert_eq!(row["metrics"]["reported_model"], MODEL);
        }
    }
    Ok(())
}

#[test]
fn same_http_chunk_retains_complete_frames_before_a_later_framing_limit_failure() -> Result<()> {
    let (request, segment, _) = drain_fixture();
    let mut wire = drain_frame(json!({"content":segment}), Value::Null);
    wire.extend(vec![b'x'; 128 * 1024 + 1]);
    let mut state = ResponseState::new(&request, 1)?;
    let mut framing = SseDecoder::default();
    let transport = consume_wire_chunk(&mut state, &mut framing, &wire, &Instant::now());
    assert!(transport.is_err());
    let mut row = json!({"http_eof":false});
    let error = finish_call(state, &mut row, transport).unwrap_err();
    assert_eq!(error.to_string(), "sse_framing_invalid");
    assert_eq!(row["api_stream_complete"], false);
    assert_eq!(row["validated_segments"].as_array().unwrap().len(), 1);
    assert_eq!(row["raw_visible_assistant_content"], segment);
    assert!(row["contract_failure_code"].is_null());
    Ok(())
}

#[test]
fn draining_retains_a_strict_utf8_prefix_after_the_visible_cap_is_reached() -> Result<()> {
    let (request, segment, final_record) = drain_fixture();
    let bad_batch = format!("{segment}{final_record}");
    let mut state = ResponseState::new(&request, 1)?;
    let mut framing = SseDecoder::default();
    let started = Instant::now();
    consume_wire_chunk(
        &mut state,
        &mut framing,
        &drain_frame(json!({"content":bad_batch}), Value::Null),
        &started,
    )?;
    let remaining = MAX_OUTPUT_BYTES - bad_batch.len();
    let large = format!(
        "{}{}",
        if remaining % 3 == 0 { "x" } else { "" },
        "中".repeat(remaining / 3 + 2)
    );
    consume_wire_chunk(
        &mut state,
        &mut framing,
        &drain_frame(json!({"content":large}), Value::Null),
        &started,
    )?;
    consume_wire_chunk(
        &mut state,
        &mut framing,
        &drain_frame(json!({"content":"TAIL"}), json!("stop")),
        &started,
    )?;
    consume_wire_chunk(&mut state, &mut framing, b"data: [DONE]\n\n", &started)?;
    framing.finish()?;
    let all_visible = format!("{bad_batch}{large}TAIL");
    let mut boundary = MAX_OUTPUT_BYTES;
    while !all_visible.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let mut row = json!({"http_eof":true});
    assert_eq!(
        finish_call(state, &mut row, Ok(()))
            .unwrap_err()
            .to_string(),
        "final_text_mismatch"
    );
    assert_eq!(
        row["raw_visible_assistant_content"],
        &all_visible[..boundary]
    );
    assert_eq!(row["raw_visible_bytes_observed"], all_visible.len());
    assert_eq!(row["raw_visible_truncated"], true);
    assert_eq!(row["api_stream_complete"], true);
    Ok(())
}

#[cfg(test)]
fn production_test_wire(texts: &[&str], bad_final: bool) -> (Value, Vec<u8>, String) {
    let request =
        json!({"request_id":"production-fixture","generation":7,"context":{"evidence":[]}});
    let joined = texts.concat();
    let mut content = String::new();
    for (sequence, text) in texts.iter().enumerate() {
        content.push_str(&format!("{}\n",json!({"schema_version":"1.0","type":"segment","request_id":"production-fixture","sequence":sequence,"text":text,"used_evidence_ids":[]})));
    }
    let final_text = if bad_final {
        "Different final."
    } else {
        &joined
    };
    content.push_str(&json!({"schema_version":"1.0","type":"final","request_id":"production-fixture","segment_count":texts.len(),
        "reply":{"schema_version":"1.0","request_id":"production-fixture","reply_text":final_text,"teaching_move":"give_hint","task_update_candidates":[],"memory_candidates":[],"used_evidence_ids":[],"needs_clarification":false}}).to_string());
    if bad_final {
        content.push('\n');
    }
    let mut wire = drain_frame(
        json!({"reasoning_content":"PRIVATE_PRODUCTION_REASONING"}),
        Value::Null,
    );
    wire.extend(drain_frame(json!({"content":content}), json!("stop")));
    wire.extend(format!("data: {}\n\n",json!({"model":MODEL,"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7,"completion_tokens_details":{"reasoning_tokens":1}}})).as_bytes());
    wire.extend(b"data: [DONE]\n\n");
    (request, wire, joined)
}

#[cfg(test)]
fn production_test_metrics() -> Value {
    json!({"transport":"fixture_transport","request_id":"production-fixture","generation":7,"queue_wait_ms":23})
}

#[cfg(test)]
fn metadata_records() -> (Value, Value, String, String, String) {
    let request =
        json!({"request_id":"production-fixture","generation":7,"context":{"evidence":[]}});
    let reply = json!({"schema_version":"1.0","request_id":"production-fixture",
        "teaching_move":"give_hint","task_update_candidates":[],"memory_candidates":[],"used_evidence_ids":[],"needs_clarification":false});
    let texts = ["Keep reply_text literal.", "  Record it."];
    let first = format!(
        "{}\n",
        json!({"schema_version":"1.0","type":"segment","request_id":"production-fixture","sequence":0,"text":texts[0],"used_evidence_ids":[]})
    );
    let second = format!(
        "{}\n",
        json!({"schema_version":"1.0","type":"segment","request_id":"production-fixture","sequence":1,"text":texts[1],"used_evidence_ids":[]})
    );
    (request, reply, first, second, texts.concat())
}

#[tokio::test]
async fn metadata_stream_publishes_before_final_and_derives_the_standard_reply_without_repeating_text(
) -> Result<()> {
    for newline in [false, true] {
        let (request, metadata, first, second, joined) = metadata_records();
        let final_record = json!({"schema_version":"1.0","type":"final_metadata","request_id":"production-fixture","segment_count":2,"reply":metadata});
        assert!(final_record["reply"].get("reply_text").is_none());
        let first_wire = drain_frame(json!({"content":first}), Value::Null);
        let mut tail = drain_frame(
            json!({"content":format!("{second}{final_record}{}",if newline {"\n"}else{""})}),
            json!("stop"),
        );
        tail.extend(b"data: [DONE]\n\n");
        let (release, released) = tokio::sync::oneshot::channel();
        let chunks = futures_util::stream::iter(vec![Ok(Bytes::from(first_wire))]).chain(
            futures_util::stream::once(async move {
                released
                    .await
                    .map_err(|_| anyhow!("fixture_final_not_released"))?;
                Ok(Bytes::from(tail))
            }),
        );
        let (sender, mut receiver) = mpsc::channel(2);
        let (_cancel_owner, mut cancel) = watch::channel(false);
        let mut metrics = production_test_metrics();
        let started = Instant::now();
        let (result, received) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(
                production_chunks_with_contract(
                    chunks,
                    &request,
                    7,
                    StreamContract::MetadataV2,
                    &sender,
                    &mut cancel,
                    &mut metrics,
                    &started,
                    tokio::time::Instant::now() + Duration::from_secs(5)
                ),
                async {
                    let first = receiver
                        .recv()
                        .await
                        .ok_or_else(|| anyhow!("first_segment_absent"))?;
                    release
                        .send(())
                        .map_err(|_| anyhow!("fixture_receiver_closed"))?;
                    let second = receiver
                        .recv()
                        .await
                        .ok_or_else(|| anyhow!("second_segment_absent"))?;
                    Ok::<_, anyhow::Error>(vec![first, second])
                }
            )
        })
        .await?;
        let reply = result?;
        let mut expected = metadata;
        expected["reply_text"] = json!(joined);
        assert_eq!(reply.reply(), &expected);
        let received = received?;
        assert_eq!(
            received.iter().map(|v| v.text()).collect::<String>(),
            joined
        );
        assert!(
            received
                .iter()
                .all(|v| v.binding().generation == 7
                    && v.binding().request_id == "production-fixture")
        );
        assert_eq!(metrics["stream_contract"], "metadata_v2");
        assert_eq!(metrics["reply_text_origin"], "server_segment_concatenation");
        assert_eq!(metrics["api_stream_complete"], true);
        assert_private_production_metrics(&metrics);
    }
    Ok(())
}

#[test]
fn diagnostic_contract_is_explicit_and_legacy_never_accepts_metadata_final() -> Result<()> {
    let (request, metadata, first, second, joined) = metadata_records();
    let content = format!(
        "{first}{second}{}\n",
        json!({"schema_version":"1.0","type":"final_metadata","request_id":"production-fixture","segment_count":2,"reply":metadata})
    );
    for contract in [StreamContract::LegacyV1, StreamContract::MetadataV2] {
        let mut state = ResponseState::with_mode(&request, 7, true, contract)?;
        state.consume(&serde_json::to_vec(&json!({"model":MODEL,"choices":[{"index":0,"delta":{"content":content},"finish_reason":"stop"}]}))?,1.0)?;
        state.consume(b"[DONE]", 2.0)?;
        let mut row = json!({});
        let result = state.complete(&mut row);
        assert_eq!(result.is_ok(), contract == StreamContract::MetadataV2);
        if let Ok(reply) = result {
            assert_eq!(reply.reply()["reply_text"], joined);
        }
        assert_eq!(row["metrics"]["stream_contract"], contract.label());
    }
    assert!(StreamContract::parse("automatic").is_err());
    Ok(())
}

#[tokio::test]
async fn metadata_stream_rejects_legacy_final_or_any_repeated_body_without_losing_the_prefix(
) -> Result<()> {
    for legacy in [false, true] {
        let (request, mut metadata, first, _, _) = metadata_records();
        metadata["reply_text"] = if legacy {
            json!("Keep reply_text literal.")
        } else {
            Value::Null
        };
        let final_record = json!({"schema_version":"1.0","type":if legacy {"final"}else{"final_metadata"},"request_id":"production-fixture","segment_count":1,"reply":metadata});
        let mut wire = drain_frame(
            json!({"content":format!("{first}{final_record}\n")}),
            json!("stop"),
        );
        wire.extend(b"data: [DONE]\n\n");
        let (sender, mut receiver) = mpsc::channel(2);
        let (_cancel_owner, mut cancel) = watch::channel(false);
        let mut metrics = production_test_metrics();
        let error = production_chunks_with_contract(
            futures_util::stream::iter(vec![Ok(Bytes::from(wire))]),
            &request,
            7,
            StreamContract::MetadataV2,
            &sender,
            &mut cancel,
            &mut metrics,
            &Instant::now(),
            tokio::time::Instant::now() + Duration::from_secs(5),
        )
        .await
        .unwrap_err();
        assert!(contract_failure_code(&error).is_some());
        assert_eq!(receiver.try_recv()?.text(), "Keep reply_text literal.");
        assert!(receiver.try_recv().is_err());
        assert_eq!(metrics["api_stream_complete"], false);
        assert_private_production_metrics(&metrics);
    }
    Ok(())
}

#[tokio::test]
async fn omni_complete_reply_uses_the_same_strict_contract_and_its_own_model_identity() -> Result<()>
{
    let (request, metadata, first, second, joined) = metadata_records();
    let content = format!(
        "{first}{second}{}\n",
        json!({"schema_version":"1.0","type":"final_metadata","request_id":"production-fixture","segment_count":2,"reply":metadata})
    );
    let (sender, mut receiver) = mpsc::channel(2);
    let (_cancel_owner, mut cancel) = watch::channel(false);
    let mut metrics = production_test_metrics();
    let reply = complete_omni(&content, &request, 7, &sender, &mut cancel, &mut metrics).await?;
    assert_eq!(reply.reply()["reply_text"], joined);
    assert_eq!(
        [
            receiver.try_recv()?.text().to_owned(),
            receiver.try_recv()?.text().to_owned()
        ]
        .concat(),
        joined
    );
    assert!(receiver.try_recv().is_err());
    assert_eq!(metrics["reported_model"], OMNI_MODEL);
    assert_eq!(metrics["api_stream_complete"], true);
    assert_private_production_metrics(&metrics);
    let (sender, _receiver) = mpsc::channel(2);
    let error = complete_omni(
        "not a tutor record",
        &request,
        7,
        &sender,
        &mut cancel,
        &mut production_test_metrics(),
    )
    .await;
    assert!(error.is_err());
    Ok(())
}

#[cfg(test)]
fn assert_private_production_metrics(metrics: &Value) {
    let text = metrics.to_string();
    for forbidden in [
        "PRIVATE_PRODUCTION_REASONING",
        "reasoning",
        "raw_visible",
        "collected_text",
        "validated_segments",
    ] {
        assert!(
            !text.contains(forbidden),
            "production metric retained forbidden field"
        );
    }
    assert_eq!(metrics["transport"], "fixture_transport");
    assert_eq!(metrics["request_id"], "production-fixture");
    assert_eq!(metrics["generation"], 7);
    assert_eq!(metrics["queue_wait_ms"], 23);
}

#[tokio::test]
async fn production_publishes_typed_segments_and_validates_clean_eof_without_raw_metrics(
) -> Result<()> {
    let (request, wire, joined) =
        production_test_wire(&["Check one variable.", " Record the observation."], false);
    let content = std::str::from_utf8(&wire)?
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|frame| {
            frame["choices"][0]["delta"]["content"]
                .as_str()
                .map(str::to_owned)
        })
        .unwrap();
    let cut = content.find('\n').unwrap() + 1;
    let mut first = drain_frame(
        json!({"reasoning_content":"PRIVATE_PRODUCTION_REASONING"}),
        Value::Null,
    );
    first.extend(drain_frame(json!({"content":&content[..cut]}), Value::Null));
    let mut tail = drain_frame(json!({"content":&content[cut..]}), json!("stop"));
    tail.extend(format!("data: {}\n\n",json!({"model":MODEL,"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7,"completion_tokens_details":{"reasoning_tokens":1}}})).as_bytes());
    tail.extend(b"data: [DONE]\n\n");
    let (release, released) = tokio::sync::oneshot::channel();
    let chunks = futures_util::stream::iter(vec![Ok(Bytes::from(first))]).chain(
        futures_util::stream::once(async move {
            released
                .await
                .map_err(|_| anyhow!("fixture_final_not_released"))?;
            Ok(Bytes::from(tail))
        }),
    );
    let (sender, mut receiver) = mpsc::channel(2);
    let (_cancel_owner, mut cancel) = watch::channel(false);
    let mut metrics = production_test_metrics();
    let started = Instant::now();
    let (result, received) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(
            production_chunks(
                chunks,
                &request,
                7,
                &sender,
                &mut cancel,
                &mut metrics,
                &started,
                tokio::time::Instant::now() + Duration::from_secs(5)
            ),
            async {
                let first = receiver
                    .recv()
                    .await
                    .ok_or_else(|| anyhow!("missing_first_segment"))?;
                assert_eq!(first.sequence(), 0);
                // The final/API terminal chunk cannot arrive until the first
                // typed segment has actually reached this bounded consumer.
                release
                    .send(())
                    .map_err(|_| anyhow!("fixture_receiver_closed"))?;
                let second = receiver
                    .recv()
                    .await
                    .ok_or_else(|| anyhow!("missing_second_segment"))?;
                Ok::<_, anyhow::Error>(vec![first, second])
            }
        )
    })
    .await?;
    let reply = result?;
    assert_eq!(reply.reply()["reply_text"], joined);
    for (sequence, segment) in received?.into_iter().enumerate() {
        assert_eq!(segment.sequence(), sequence);
        assert_eq!(segment.binding().request_id, "production-fixture");
        assert_eq!(segment.binding().generation, 7);
    }
    assert!(receiver.try_recv().is_err());
    assert_eq!(metrics["segments_enqueued"], 2);
    assert_eq!(metrics["api_stream_complete"], true);
    assert_eq!(metrics["usage"]["total_tokens"], 7);
    assert!(metrics["usage"].get("completion_tokens_details").is_none());
    assert_private_production_metrics(&metrics);
    Ok(())
}

#[tokio::test]
async fn production_same_delta_prefix_is_published_before_original_contract_error_returns(
) -> Result<()> {
    let (request, wire, _) = production_test_wire(&["Check one variable."], true);
    let (sender, mut receiver) = mpsc::channel(2);
    let (_cancel_owner, mut cancel) = watch::channel(false);
    let mut metrics = production_test_metrics();
    let error = production_chunks(
        futures_util::stream::iter(vec![Ok(Bytes::from(wire))]),
        &request,
        7,
        &sender,
        &mut cancel,
        &mut metrics,
        &Instant::now(),
        tokio::time::Instant::now() + Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert_eq!(contract_failure_code(&error), Some("final_text_mismatch"));
    production_failure(&error, &mut metrics);
    assert_eq!(metrics["failure_code"], "final_text_mismatch");
    assert_eq!(receiver.try_recv()?.text(), "Check one variable.");
    assert!(receiver.try_recv().is_err());
    assert_eq!(metrics["segments_enqueued"], 1);
    assert_eq!(metrics["http_eof"], false);
    assert_eq!(metrics["api_stream_complete"], false);
    assert_private_production_metrics(&metrics);
    Ok(())
}

#[tokio::test]
async fn production_cancel_while_queue_is_full_never_enqueues_a_third_segment() -> Result<()> {
    let (request, wire, _) = production_test_wire(&["First.", " Second.", " Third."], false);
    let (sender, mut receiver) = mpsc::channel(2);
    let probe = sender.clone();
    let (cancel_owner, mut cancel) = watch::channel(false);
    let mut metrics = production_test_metrics();
    let started = Instant::now();
    let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(biased;
            production_chunks(futures_util::stream::iter(vec![Ok(Bytes::from(wire))]), &request, 7, &sender,
                &mut cancel, &mut metrics, &started, tokio::time::Instant::now() + Duration::from_secs(5)),
            async { assert_eq!(probe.capacity(),0); cancel_owner.send(true).unwrap(); }
        )
    }).await?;
    let error = result.unwrap_err();
    assert_eq!(error.to_string(), "foreground_cancelled");
    production_failure(&error, &mut metrics);
    assert_eq!(metrics["failure_code"], "foreground_cancelled");
    assert_eq!(metrics["segments_enqueued"], 2);
    assert_eq!(receiver.try_recv()?.sequence(), 0);
    assert_eq!(receiver.try_recv()?.sequence(), 1);
    assert!(receiver.try_recv().is_err());
    assert_eq!(metrics["api_stream_complete"], false);
    assert_private_production_metrics(&metrics);
    Ok(())
}

#[tokio::test]
async fn production_consumer_close_while_queue_is_full_stops_publication() -> Result<()> {
    let (request, wire, _) = production_test_wire(&["First.", " Second.", " Third."], false);
    let (sender, receiver) = mpsc::channel(2);
    let probe = sender.clone();
    let (_cancel_owner, mut cancel) = watch::channel(false);
    let mut metrics = production_test_metrics();
    let started = Instant::now();
    let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(biased;
            production_chunks(futures_util::stream::iter(vec![Ok(Bytes::from(wire))]), &request, 7, &sender,
                &mut cancel, &mut metrics, &started, tokio::time::Instant::now() + Duration::from_secs(5)),
            async { assert_eq!(probe.capacity(),0); drop(receiver); }
        )
    }).await?;
    let error = result.unwrap_err();
    assert_eq!(error.to_string(), "consumer_closed");
    production_failure(&error, &mut metrics);
    assert_eq!(metrics["failure_code"], "consumer_closed");
    assert_eq!(metrics["segments_enqueued"], 2);
    assert_eq!(metrics["api_stream_complete"], false);
    assert_private_production_metrics(&metrics);
    Ok(())
}

#[tokio::test]
async fn production_deadline_bounds_full_queue_and_pending_read_is_cancellable() -> Result<()> {
    let (request, wire, _) = production_test_wire(&["First.", " Second.", " Third."], false);
    let (sender, mut receiver) = mpsc::channel(2);
    let (_cancel_owner, mut cancel) = watch::channel(false);
    let mut metrics = production_test_metrics();
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        production_chunks(
            futures_util::stream::iter(vec![Ok(Bytes::from(wire))]),
            &request,
            7,
            &sender,
            &mut cancel,
            &mut metrics,
            &Instant::now(),
            tokio::time::Instant::now() + Duration::from_millis(500),
        ),
    )
    .await?
    .unwrap_err();
    assert_eq!(error.to_string(), "request_deadline_exceeded");
    production_failure(&error, &mut metrics);
    assert_eq!(metrics["failure_code"], "request_deadline_exceeded");
    assert_eq!(metrics["segments_enqueued"], 2);
    assert_eq!(receiver.try_recv()?.sequence(), 0);
    assert_eq!(receiver.try_recv()?.sequence(), 1);
    assert!(receiver.try_recv().is_err());
    assert_private_production_metrics(&metrics);

    let (cancel_owner, mut cancel) = watch::channel(false);
    let mut metrics = production_test_metrics();
    let started = Instant::now();
    let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(biased;
            production_chunks(futures_util::stream::pending::<Result<Bytes>>(), &request, 7, &sender,
                &mut cancel, &mut metrics, &started, tokio::time::Instant::now() + Duration::from_secs(5)),
            async { cancel_owner.send(true).unwrap(); }
        )
    }).await?;
    assert_eq!(result.unwrap_err().to_string(), "foreground_cancelled");
    assert_eq!(metrics["segments_enqueued"], 0);
    assert_eq!(metrics["http_eof"], false);
    assert_private_production_metrics(&metrics);
    Ok(())
}

#[tokio::test]
async fn production_preflight_and_fixed_failure_projection_do_not_poll_or_expose_context(
) -> Result<()> {
    let (request, _, _) = production_test_wire(&["Safe."], false);
    for (generation, capacity, expected) in [
        (8, 2, "request_generation_mismatch"),
        (7, 3, "segment_channel_capacity"),
    ] {
        let (sender, _receiver) = mpsc::channel(capacity);
        let (_cancel_owner, mut cancel) = watch::channel(false);
        let mut metrics = production_test_metrics();
        let mut polls = 0;
        let chunks = futures_util::stream::poll_fn(|_| {
            polls += 1;
            std::task::Poll::Ready(None)
        });
        let error = production_chunks(
            chunks,
            &request,
            generation,
            &sender,
            &mut cancel,
            &mut metrics,
            &Instant::now(),
            tokio::time::Instant::now() + Duration::from_secs(5),
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), expected);
        assert_eq!(polls, 0);
        production_failure(&error, &mut metrics);
        assert_eq!(metrics["failure_code"], expected);
        assert_private_production_metrics(&metrics);
    }
    let line = format!(
        "{}\n",
        json!({"schema_version":"1.0","type":"segment","request_id":"production-fixture","sequence":0,"text":"Safe.","used_evidence_ids":[]})
    );
    let missing_identity = format!(
        "data: {}\n\n",
        json!({"choices":[{"index":0,"delta":{"content":line},"finish_reason":null}]})
    );
    let (sender, mut receiver) = mpsc::channel(2);
    let (_cancel_owner, mut cancel) = watch::channel(false);
    let mut metrics = production_test_metrics();
    let error = production_chunks(
        futures_util::stream::iter(vec![Ok(Bytes::from(missing_identity))]),
        &request,
        7,
        &sender,
        &mut cancel,
        &mut metrics,
        &Instant::now(),
        tokio::time::Instant::now() + Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "response_model_mismatch_or_absent");
    assert!(receiver.try_recv().is_err());
    assert_eq!(metrics["segments_enqueued"], 0);
    assert_eq!(metrics["api_stream_complete"], false);
    production_failure(&error, &mut metrics);
    assert_private_production_metrics(&metrics);
    let mut metrics = production_test_metrics();
    production_failure(
        &anyhow!("PRIVATE_PRODUCTION_REASONING arbitrary failure body"),
        &mut metrics,
    );
    assert_eq!(metrics["failure_code"], "foreground_operation_failed");
    metrics["http_status"] = json!(401);
    production_failure(&anyhow!("http_status_failure"), &mut metrics);
    assert_eq!(metrics["failure_code"], "http_status_failure");
    assert_eq!(metrics["http_status"], 401);
    assert_private_production_metrics(&metrics);
    Ok(())
}

#[tokio::test]
async fn omni_single_json_is_validated_then_split_without_changing_reply_text() -> Result<()> {
    let request =
        json!({"request_id":"production-fixture","generation":7,"context":{"evidence":[]}});
    let reply = json!({"schema_version":"1.0","request_id":"production-fixture",
        "reply_text":"先观察阳光穿过水珠。 Then notice how the colours separate.",
        "teaching_move":"answer","task_update_candidates":[],"memory_candidates":[],
        "used_evidence_ids":[],"needs_clarification":false});
    let content = serde_json::to_string(&reply)?;
    let records = complete_omni_records(&content, &request)?;
    assert!(records.contains("\"type\":\"final_metadata\""));
    assert!(!records.contains("```"));
    let (sender, mut receiver) = mpsc::channel(2);
    let (_cancel_owner, mut cancel) = watch::channel(false);
    let mut metrics = production_test_metrics();
    let validated =
        complete_omni_reply(&content, &request, 7, &sender, &mut cancel, &mut metrics).await?;
    assert_eq!(validated.into_reply(), reply);
    let mut displayed = String::new();
    while let Ok(segment) = receiver.try_recv() {
        displayed.push_str(segment.text());
        assert!(segment.used_evidence_ids().is_empty());
    }
    assert_eq!(displayed, reply["reply_text"]);
    for invalid in [
        format!("```json\n{content}\n```"),
        content.trim_end_matches('}').to_owned() + ",\"unexpected\":true}",
    ] {
        assert!(complete_omni_records(&invalid, &request).is_err());
    }
    Ok(())
}

#[test]
fn omni_long_text_split_is_bounded_lossless_and_never_creates_empty_segments() -> Result<()> {
    for text in [
        "界".repeat(700),
        "First sentence. Second sentence. Third sentence.".repeat(30),
        "第一句！第二句？第三句；第四句。".repeat(40),
    ] {
        let segments = split_complete_omni_text(&text)?;
        assert!(segments.len() <= MAX_SEGMENTS);
        assert!(segments.iter().all(|segment| {
            !segment.trim().is_empty() && segment.chars().count() <= MAX_SEGMENT_CHARS
        }));
        assert_eq!(segments.concat(), text);
    }
    Ok(())
}
