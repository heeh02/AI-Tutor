//! Opt-in ACP delivery contract. Parsing is independent of subprocesses and UI.
//! Only newline-terminated, validated records can become a TutorSegment.
//! An audited clean ACP end_turn may delimit a final record, never a segment.
use crate::tutor::{
    citation::{self, CitationFailure, CitationSite},
    validate_reply,
};
use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

pub const MAX_SEGMENTS: usize = 12;
pub const MAX_SEGMENT_CHARS: usize = 320;
pub const MAX_REPLY_CHARS: usize = 1600;
pub const MAX_LINE_BYTES: usize = 32 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Debug)]
struct ContractError {
    code: &'static str,
    message: &'static str,
    citation: Option<CitationFailure>,
}
impl std::fmt::Display for ContractError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(self.message)
    }
}
impl std::error::Error for ContractError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.citation
            .as_ref()
            .map(|cause| cause as &(dyn std::error::Error + 'static))
    }
}
fn failure(code: &'static str, message: &'static str) -> anyhow::Error {
    ContractError {
        code,
        message,
        citation: None,
    }
    .into()
}
fn citation_failure(
    code: &'static str,
    message: &'static str,
    error: &anyhow::Error,
) -> anyhow::Error {
    // Retain only our bounded typed cause, never arbitrary error context/text.
    ContractError {
        code,
        message,
        citation: citation::typed_failure(error),
    }
    .into()
}
pub(crate) fn contract_failure_code(error: &anyhow::Error) -> Option<&'static str> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<ContractError>()
            .map(|error| error.code)
    })
}

fn parse_record(bytes: &[u8]) -> Result<Record> {
    std::str::from_utf8(bytes).map_err(|_| failure("invalid_utf8", "教学记录UTF8编码无效"))?;
    serde_json::from_slice(bytes).map_err(|error| {
        let code = match error.classify() {
            serde_json::error::Category::Eof => "incomplete_json",
            serde_json::error::Category::Data => "record_schema_invalid",
            _ => "invalid_json",
        };
        failure(code, "教学记录JSON或字段无效，正文已隐藏")
    })
}

/// The generation is issued by the caller, never accepted from model output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeliveryBinding {
    pub request_id: String,
    pub generation: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct TutorSegment {
    binding: DeliveryBinding,
    sequence: usize,
    text: String,
    used_evidence_ids: Vec<String>,
}
impl TutorSegment {
    pub fn binding(&self) -> &DeliveryBinding {
        &self.binding
    }
    pub fn sequence(&self) -> usize {
        self.sequence
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn used_evidence_ids(&self) -> &[String] {
        &self.used_evidence_ids
    }
}

/// Created only after the complete JSONL contract and TutorReply validate.
/// The ACP adapter returns it only after end_turn, profile proof and cleanup.
#[derive(Debug)]
pub struct ValidatedTutorReply {
    binding: DeliveryBinding,
    segment_count: usize,
    reply: Value,
}
impl ValidatedTutorReply {
    pub fn reply(&self) -> &Value {
        &self.reply
    }
    pub fn into_reply(self) -> Value {
        self.reply
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Record {
    Segment {
        schema_version: String,
        request_id: String,
        sequence: usize,
        text: String,
        used_evidence_ids: Vec<String>,
    },
    Final {
        schema_version: String,
        request_id: String,
        segment_count: usize,
        reply: Value,
    },
    FinalMetadata {
        schema_version: String,
        request_id: String,
        segment_count: usize,
        reply: Value,
    },
}

pub(crate) struct SegmentDecoder {
    request: Value,
    binding: DeliveryBinding,
    evidence: HashSet<String>,
    pending: Vec<u8>,
    bytes: usize,
    segments: Vec<TutorSegment>,
    characters: usize,
    final_reply: Option<Value>,
    failed: bool,
    metadata_final: bool,
}
impl SegmentDecoder {
    pub(crate) fn new(request: &Value, generation: u64) -> Result<Self> {
        Self::with_final_mode(request, generation, false)
    }
    /// Explicit alternate wire contract. Model metadata may never supply text;
    /// the only reply body is the exact already-validated segment concatenation.
    pub(crate) fn new_metadata(request: &Value, generation: u64) -> Result<Self> {
        Self::with_final_mode(request, generation, true)
    }
    fn with_final_mode(request: &Value, generation: u64, metadata_final: bool) -> Result<Self> {
        let request_id = request["request_id"]
            .as_str()
            .ok_or_else(|| anyhow!("流式请求编号缺失"))?;
        ensure!(
            (1..=128).contains(&request_id.chars().count()) && generation > 0,
            "流式交付绑定无效"
        );
        let evidence = request["context"]["evidence"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v["event_id"].as_str().map(str::to_owned))
            .collect();
        Ok(Self {
            request: request.clone(),
            binding: DeliveryBinding {
                request_id: request_id.into(),
                generation,
            },
            evidence,
            pending: vec![],
            bytes: 0,
            segments: vec![],
            characters: 0,
            final_reply: None,
            failed: false,
            metadata_final,
        })
    }
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<TutorSegment>> {
        ensure!(!self.failed, failure("stream_failed", "流式输出已失效"));
        let result = self.push_inner(bytes);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn push_inner(&mut self, bytes: &[u8]) -> Result<Vec<TutorSegment>> {
        ensure!(
            bytes.len() <= MAX_OUTPUT_BYTES.saturating_sub(self.bytes),
            failure("output_limit", "流式输出总量超限")
        );
        self.bytes += bytes.len();
        self.pending.extend_from_slice(bytes);
        let mut emitted = vec![];
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            ensure!(
                end <= MAX_LINE_BYTES,
                failure("line_limit", "流式输出单行超限")
            );
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let record = parse_record(&line)?;
            if let Some(segment) = self.consume_record(record)? {
                emitted.push(segment);
            }
        }
        ensure!(
            self.pending.len() <= MAX_LINE_BYTES,
            failure("line_limit", "流式输出单行超限")
        );
        ensure!(
            self.final_reply.is_none() || self.pending.iter().all(u8::is_ascii_whitespace),
            failure("text_after_final", "final之后仍有输出")
        );
        Ok(emitted)
    }
    /// Newline records and the single clean-EOF final share every validation.
    fn consume_record(&mut self, record: Record) -> Result<Option<TutorSegment>> {
        ensure!(
            self.final_reply.is_none(),
            failure("text_after_final", "final之后仍有输出")
        );
        match record {
            Record::Segment {
                schema_version,
                request_id,
                sequence,
                text,
                used_evidence_ids,
            } => {
                self.validate_binding(&schema_version, &request_id)?;
                ensure!(
                    sequence == self.segments.len() && sequence < MAX_SEGMENTS,
                    failure("segment_sequence", "教学片段编号或段数无效")
                );
                let chars = text.chars().count();
                ensure!(
                    (1..=MAX_SEGMENT_CHARS).contains(&chars) && !text.trim().is_empty(),
                    failure("segment_length", "教学片段长度无效")
                );
                ensure!(
                    self.characters + chars <= MAX_REPLY_CHARS,
                    failure("reply_length", "教学全文超出字数上限")
                );
                citation::check_citations(
                    &json!(used_evidence_ids),
                    &self.evidence,
                    &self.request,
                    CitationSite::Segment(sequence),
                    0,
                    20,
                )
                .map_err(|error| {
                    citation_failure("segment_evidence", "教学片段来源不属于当前请求", &error)
                })?;
                ensure!(
                    used_evidence_ids.iter().collect::<HashSet<_>>().len()
                        == used_evidence_ids.len(),
                    failure("segment_evidence", "教学片段来源重复")
                );
                let segment = TutorSegment {
                    binding: self.binding.clone(),
                    sequence,
                    text,
                    used_evidence_ids,
                };
                self.characters += chars;
                self.segments.push(segment.clone());
                Ok(Some(segment))
            }
            Record::Final {
                schema_version,
                request_id,
                segment_count,
                reply,
            } => {
                ensure!(
                    !self.metadata_final,
                    failure("final_protocol_mismatch", "终稿类型与显式协议不匹配")
                );
                self.accept_final(&schema_version, &request_id, segment_count, reply, false)
            }
            Record::FinalMetadata {
                schema_version,
                request_id,
                segment_count,
                reply,
            } => {
                // Preserve legacy rejection of this previously unknown tag.
                ensure!(
                    self.metadata_final,
                    failure(
                        "record_schema_invalid",
                        "教学记录JSON或字段无效，正文已隐藏"
                    )
                );
                self.accept_final(&schema_version, &request_id, segment_count, reply, true)
            }
        }
    }
    fn accept_final(
        &mut self,
        schema_version: &str,
        request_id: &str,
        segment_count: usize,
        mut reply: Value,
        derive_text: bool,
    ) -> Result<Option<TutorSegment>> {
        self.validate_binding(schema_version, request_id)?;
        ensure!(
            segment_count > 0 && segment_count == self.segments.len(),
            failure("final_count", "final段数不匹配")
        );
        let exact: String = self.segments.iter().map(|s| s.text.as_str()).collect();
        if derive_text {
            let metadata = reply
                .as_object_mut()
                .ok_or_else(|| failure("final_schema_invalid", "最终TutorReply未通过完整校验"))?;
            ensure!(
                !metadata.contains_key("reply_text"),
                failure("metadata_reply_text_forbidden", "元数据终稿不得提供正文")
            );
            metadata.insert("reply_text".into(), json!(exact));
        }
        validate_reply(&reply, &self.request).map_err(|error| {
            citation_failure(
                "final_schema_invalid",
                "最终TutorReply未通过完整校验",
                &error,
            )
        })?;
        ensure!(
            reply["reply_text"].as_str() == Some(exact.as_str()),
            failure("final_text_mismatch", "final正文与已校验片段不一致")
        );
        let final_ids: HashSet<_> = reply["used_evidence_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        ensure!(
            self.segments
                .iter()
                .flat_map(|s| s.used_evidence_ids.iter())
                .all(|id| final_ids.contains(id.as_str())),
            failure("final_evidence_mismatch", "final未覆盖教学片段来源")
        );
        self.final_reply = Some(reply);
        Ok(None)
    }
    fn validate_binding(&self, version: &str, request_id: &str) -> Result<()> {
        ensure!(
            version == "1.0" && request_id == self.binding.request_id,
            failure("request_binding", "教学片段请求绑定不一致")
        );
        Ok(())
    }
    /// Strict line-delimited completion, without assuming a clean remote end.
    pub(crate) fn finish(self) -> Result<ValidatedTutorReply> {
        ensure!(!self.failed, failure("stream_failed", "流式输出已失效"));
        ensure!(
            self.pending.iter().all(u8::is_ascii_whitespace),
            failure("unterminated_record", "流式输出末行尚未结束")
        );
        let reply = self
            .final_reply
            .ok_or_else(|| failure("missing_final", "流式输出缺少完整final"))?;
        Ok(ValidatedTutorReply {
            binding: self.binding,
            segment_count: self.segments.len(),
            reply,
        })
    }
    /// The caller must first confirm ACP end_turn and the final profile audit.
    /// EOF can delimit only a complete final. It never emits a tail segment.
    pub(crate) fn finish_clean_end(mut self) -> Result<ValidatedTutorReply> {
        ensure!(!self.failed, failure("stream_failed", "流式输出已失效"));
        if !self.pending.iter().all(u8::is_ascii_whitespace) {
            let record = parse_record(&self.pending)?;
            ensure!(
                matches!(record, Record::Final { .. } | Record::FinalMetadata { .. }),
                failure("tail_not_final", "干净结束时末行必须是完整final")
            );
            self.consume_record(record)?;
            self.pending.clear();
        }
        self.finish()
    }
    /// Counts and fixed shape labels only; never includes generated text/IDs.
    pub(crate) fn diagnostics(&self) -> Value {
        let (shape, kind) = if self.pending.iter().all(u8::is_ascii_whitespace) {
            ("empty", "none")
        } else if std::str::from_utf8(&self.pending).is_err() {
            ("invalid_utf8", "unknown")
        } else {
            match serde_json::from_slice::<Value>(&self.pending) {
                Ok(value) => (
                    "complete_single_json",
                    match value["type"].as_str() {
                        Some("final") => "final",
                        Some("final_metadata") if self.metadata_final => "final_metadata",
                        Some("segment") => "segment",
                        _ => "other",
                    },
                ),
                Err(error) if error.is_eof() => ("incomplete_json", "unknown"),
                Err(_) => ("invalid_json", "unknown"),
            }
        };
        json!({"received_bytes":self.bytes,"validated_segments":self.segments.len(),
            "newline_final_validated":self.final_reply.is_some(),"pending_tail_bytes":self.pending.len(),
            "pending_tail_shape":shape,"pending_tail_record_kind":kind,"failed":self.failed})
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptKind {
    Displayed,
    SpeechStarted,
    SpeechCompleted,
    SpeechInterrupted,
}

#[derive(Clone, Debug)]
pub struct DeliveryReceipt {
    pub binding: DeliveryBinding,
    pub sequence: usize,
    pub kind: ReceiptKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeechState {
    NotStarted,
    Reserved,
    Started,
    Completed,
    Interrupted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeechGate {
    Disabled,
    Open,
    Closed,
}

#[derive(Clone, Debug, Serialize)]
pub struct SegmentDelivery {
    pub sequence: usize,
    pub validated: bool,
    pub displayed: bool,
    pub speech: SpeechState,
}

/// A consumer owns this ledger. Merely parsing/enqueuing a segment is never a
/// display or speech receipt. It must be invalidated when its session changes.
#[derive(Debug, Serialize)]
pub struct DeliveryLedger {
    binding: DeliveryBinding,
    segments: Vec<SegmentDelivery>,
    final_validated: bool,
    invalidated: bool,
    speech_gate: SpeechGate,
}
impl DeliveryLedger {
    pub fn new(binding: DeliveryBinding) -> Result<Self> {
        ensure!(
            !binding.request_id.is_empty() && binding.generation > 0,
            "交付账本绑定无效"
        );
        Ok(Self {
            binding,
            segments: vec![],
            final_validated: false,
            invalidated: false,
            speech_gate: SpeechGate::Open,
        })
    }
    /// Reconstruct the small ledger from durable metadata, never from a second
    /// text/audio queue. Invalid prefixes must not become resumable work.
    pub(crate) fn restore(
        binding: DeliveryBinding,
        segments: Vec<SegmentDelivery>,
        final_validated: bool,
        speech_gate: SpeechGate,
    ) -> Result<Self> {
        let mut ledger = Self::new(binding)?;
        ensure!(segments.len() <= MAX_SEGMENTS, "交付账本片段超限");
        let mut undisplayed = false;
        for (sequence, segment) in segments.iter().enumerate() {
            ensure!(
                segment.sequence == sequence && segment.validated,
                "持久交付片段编号或校验状态无效"
            );
            ensure!(!undisplayed || !segment.displayed, "持久显示前缀不连续");
            undisplayed |= !segment.displayed;
            if segment.speech != SpeechState::NotStarted {
                ensure!(
                    segment.displayed
                        && segments[..sequence]
                            .iter()
                            .all(|previous| previous.speech == SpeechState::Completed),
                    "持久播报前缀不连续或尚未显示"
                );
            }
            ensure!(
                speech_gate == SpeechGate::Open
                    || !matches!(segment.speech, SpeechState::Reserved | SpeechState::Started),
                "已关闭播报仍有活动片段"
            );
        }
        ensure!(!final_validated || !segments.is_empty(), "完整回复缺少片段");
        ledger.segments = segments;
        ledger.final_validated = final_validated;
        ledger.speech_gate = speech_gate;
        Ok(ledger)
    }

    pub(crate) fn reserve_speech(&mut self, sequence: usize) -> Result<()> {
        ensure!(
            !self.invalidated && self.speech_gate == SpeechGate::Open,
            "自动播报已关闭"
        );
        ensure!(sequence < self.segments.len(), "播报没有已校验片段");
        let segment = &self.segments[sequence];
        ensure!(
            segment.displayed
                && segment.speech == SpeechState::NotStarted
                && self.segments[..sequence]
                    .iter()
                    .all(|previous| previous.speech == SpeechState::Completed),
            "播报预约不满足已显示完整前缀"
        );
        self.segments[sequence].speech = SpeechState::Reserved;
        Ok(())
    }
    pub fn accept(&mut self, segment: &TutorSegment) -> Result<()> {
        ensure!(
            !self.invalidated && !self.final_validated && segment.binding == self.binding,
            "教学片段已过期"
        );
        ensure!(
            segment.sequence == self.segments.len() && self.segments.len() < MAX_SEGMENTS,
            "交付片段乱序或重复"
        );
        self.segments.push(SegmentDelivery {
            sequence: segment.sequence,
            validated: true,
            displayed: false,
            speech: SpeechState::NotStarted,
        });
        Ok(())
    }
    pub fn receipt(&mut self, receipt: DeliveryReceipt) -> Result<()> {
        ensure!(
            !self.invalidated && receipt.binding == self.binding,
            "交付回执已过期"
        );
        ensure!(
            receipt.sequence < self.segments.len(),
            "交付回执没有已校验片段"
        );
        let previous = &self.segments[..receipt.sequence];
        let current = &self.segments[receipt.sequence];
        match receipt.kind {
            ReceiptKind::Displayed => ensure!(
                !current.displayed && previous.iter().all(|s| s.displayed),
                "显示回执乱序或重复"
            ),
            ReceiptKind::SpeechStarted => ensure!(
                self.speech_gate == SpeechGate::Open
                    && current.displayed
                    && matches!(
                        current.speech,
                        SpeechState::NotStarted | SpeechState::Reserved
                    )
                    && previous.iter().all(|s| s.speech == SpeechState::Completed),
                "播报开始回执乱序或重复"
            ),
            ReceiptKind::SpeechCompleted | ReceiptKind::SpeechInterrupted => ensure!(
                self.speech_gate == SpeechGate::Open && current.speech == SpeechState::Started,
                "播报完成回执缺少开始或已结束"
            ),
        }
        let current = &mut self.segments[receipt.sequence];
        match receipt.kind {
            ReceiptKind::Displayed => current.displayed = true,
            ReceiptKind::SpeechStarted => current.speech = SpeechState::Started,
            ReceiptKind::SpeechCompleted => current.speech = SpeechState::Completed,
            ReceiptKind::SpeechInterrupted => current.speech = SpeechState::Interrupted,
        }
        Ok(())
    }
    /// The caller invokes this only on a successful generate_segments result.
    pub fn finish_validated(&mut self, reply: &ValidatedTutorReply) -> Result<()> {
        ensure!(
            !self.invalidated
                && !self.final_validated
                && !self.segments.is_empty()
                && reply.binding == self.binding
                && reply.segment_count == self.segments.len(),
            "交付完成绑定无效"
        );
        self.final_validated = true;
        Ok(())
    }
    /// Invalidation retains what was actually delivered and does not mark any
    /// interrupted utterance as completely heard. Late receipts are rejected.
    pub fn invalidate(&mut self) {
        self.invalidated = true;
        self.close_speech();
    }
    /// Closing audio does not invalidate text validation or later DOM receipts.
    pub fn close_speech(&mut self) {
        self.speech_gate = SpeechGate::Closed;
        for segment in &mut self.segments {
            if matches!(segment.speech, SpeechState::Reserved | SpeechState::Started) {
                segment.speech = SpeechState::Interrupted;
            }
        }
    }
    pub(crate) fn speech_complete(&self) -> bool {
        self.final_validated
            && !self.segments.is_empty()
            && self
                .segments
                .iter()
                .all(|segment| segment.speech == SpeechState::Completed)
    }
    pub fn segments(&self) -> &[SegmentDelivery] {
        &self.segments
    }
}

#[cfg(test)]
#[path = "tutor_segments_tests.rs"]
mod tests;
