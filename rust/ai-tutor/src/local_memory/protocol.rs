use crate::memory_embedding::EmbeddingProfile;
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{io::{AsyncBufRead, AsyncBufReadExt}, sync::mpsc};

pub(super) const MAX_FRAME: usize = 65_536;
pub(super) const MAX_TOKENS: usize = 512;
pub(super) const PROFILE: EmbeddingProfile = EmbeddingProfile::BgeM3NativeFp32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbeddingRole { Query, Document }
impl EmbeddingRole {
    pub fn as_str(self) -> &'static str {
        match self { Self::Query => "query", Self::Document => "document" }
    }
}

#[derive(Clone, Debug)]
pub struct Encoded {
    pub vector: Vec<f32>,
    pub tokens: usize,
    pub timings: Value,
}
#[derive(Clone, Debug)]
pub enum EncodeResult {
    Encoded(Encoded),
    TooLong { tokens: usize, maximum_tokens: usize },
}

#[derive(Clone)]
pub(super) struct Pending {
    pub request_id: String,
    pub role: EmbeddingRole,
    pub input_sha256: String,
}
pub(super) fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 96
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}
pub(super) fn frame(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() < MAX_FRAME, "local_memory_frame_too_large");
    bytes.push(b'\n');
    Ok(bytes)
}
pub(super) fn control(instance: &str, op: &str) -> Value {
    json!({"protocol_version":1,"instance_id":instance,"op":op})
}
pub(super) fn encode_request(instance: &str, request_id: &str, role: EmbeddingRole, text: &str)
    -> Result<(Pending, Vec<u8>)> {
    ensure!(identifier(request_id), "local_memory_request_id_invalid");
    ensure!(!text.trim().is_empty(), "local_memory_input_empty");
    ensure!(text.len() < MAX_FRAME, "local_memory_input_too_large");
    let pending = Pending { request_id: request_id.into(), role,
        input_sha256: format!("{:x}", Sha256::digest(text.as_bytes())) };
    let mut value = control(instance, "encode");
    value["request_id"] = json!(request_id);
    value["role"] = json!(role.as_str());
    value["text"] = json!(text);
    value["input_sha256"] = json!(pending.input_sha256);
    value["profile"] = PROFILE.metadata();
    Ok((pending, frame(&value)?))
}

pub(super) fn common<'a>(value: &'a Value, instance: &str) -> Result<&'a str> {
    ensure!(value.is_object() && value["protocol_version"] == 1 && value["instance_id"] == instance,
        "local_memory_instance_mismatch");
    value["state"].as_str().ok_or_else(|| anyhow!("local_memory_state_missing"))
}
fn profile(value: &Value) -> Result<()> {
    ensure!(value["profile"] == PROFILE.metadata(), "local_memory_profile_mismatch");
    Ok(())
}
fn count(value: &Value, key: &str) -> Result<usize> {
    value[key].as_u64().and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| anyhow!("local_memory_count_invalid"))
}
fn timings(value: &Value, keys: &[&str]) -> Result<Value> {
    let mut result = serde_json::Map::new();
    for key in keys {
        let n = value["timings"][key].as_f64().filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or_else(|| anyhow!("local_memory_timing_invalid"))?;
        result.insert((*key).into(), json!(n));
    }
    Ok(Value::Object(result))
}
pub(super) fn validate_awaiting(value: &Value) -> Result<()> {
    ensure!(value["ready"] == false && value["warmup_completed"] == false
        && value["manager_pid"].as_u64().is_some_and(|v| v > 0), "local_memory_gate_invalid");
    Ok(())
}
pub(super) fn validate_starting(value: &Value) -> Result<()> {
    ensure!(value["ready"] == false && value["warmup_completed"] == false
        && value["pid"].as_u64().is_some_and(|v| v > 0)
        && value["pid"] == value["process_group"], "local_memory_group_invalid");
    Ok(())
}
pub(super) fn validate_loading(value: &Value) -> Result<()> {
    profile(value)?;
    ensure!(value["ready"] == false && value["warmup_completed"] == false, "local_memory_loading_invalid");
    Ok(())
}
pub(super) fn ready(value: &Value) -> Result<Value> {
    profile(value)?;
    ensure!(value["ready"] == true && value["warmup_completed"] == true
        && value["device"] == "cpu" && value["dtype"] == "float32"
        && value["intra_threads"] == 2 && value["inter_threads"] == 1
        && value["cuda_visible_devices"] == "" && value["load_count"] == 1,
        "local_memory_cpu_contract_mismatch");
    let tokens = count(value, "warmup_tokens")?;
    ensure!((1..=MAX_TOKENS).contains(&tokens), "local_memory_warmup_invalid");
    Ok(json!({"profile":PROFILE.metadata(),"device":"cpu","dtype":"float32",
        "intra_threads":2,"inter_threads":1,"cuda_visible_devices":"","load_count":1,
        "warmup_tokens":tokens,"ready":true,"warmup_completed":true,
        "timings":timings(value,&["hash_audit_ms","model_load_ms","warmup_ms","total_ms"])?}))
}
pub(super) fn encoded(value: &Value, instance: &str, pending: &Pending) -> Result<EncodeResult> {
    let state = common(value, instance)?;
    profile(value)?;
    ensure!(value["request_id"] == pending.request_id && value["role"] == pending.role.as_str()
        && value["input_sha256"] == pending.input_sha256 && value["text_sha256"] == pending.input_sha256,
        "local_memory_response_binding_mismatch");
    if state == "encode_error" {
        if value["code"] == "input_too_long" {
            let tokens = count(value, "tokens")?;
            ensure!(tokens > MAX_TOKENS && value.get("vector").is_none(), "local_memory_too_long_invalid");
            return Ok(EncodeResult::TooLong { tokens, maximum_tokens: MAX_TOKENS });
        }
        bail!("local_memory_encode_failed");
    }
    ensure!(state == "encoded", "local_memory_encode_state_invalid");
    let tokens = count(value, "tokens")?;
    ensure!((1..=MAX_TOKENS).contains(&tokens), "local_memory_token_limit_violated");
    let values = value["vector"].as_array().ok_or_else(|| anyhow!("local_memory_vector_missing"))?;
    ensure!(values.len() == PROFILE.dimension(), "local_memory_dimension_mismatch");
    let vector = values.iter().map(|v| v.as_f64().map(|n| n as f32)
        .filter(|n| n.is_finite()).ok_or_else(|| anyhow!("local_memory_vector_nonfinite")))
        .collect::<Result<Vec<_>>>()?;
    ensure!(PROFILE.normalized(&vector), "local_memory_vector_not_normalized");
    Ok(EncodeResult::Encoded(Encoded { vector, tokens,
        timings: timings(value, &["tokenize_ms","inference_ms","total_ms"])? }))
}
pub(super) fn stopped(value: &Value, instance: &str) -> Result<Value> {
    ensure!(common(value, instance)? == "stopped" && value["linux_cleanup_confirmed"] == true
        && value["group_reaped"] == true && value["ready"] == false && value["warmup_completed"] == false,
        "local_memory_linux_cleanup_unconfirmed");
    let started = value["worker_started"].as_bool().ok_or_else(|| anyhow!("local_memory_stop_invalid"))?;
    ensure!(value["forced_group_stop"].is_boolean()
        && if started { value["worker_exit_code"].as_i64().is_some() } else { value["worker_exit_code"].is_null() },
        "local_memory_worker_exit_unconfirmed");
    let reason = value["reason"].as_str().filter(|s| identifier(s))
        .ok_or_else(|| anyhow!("local_memory_stop_reason_invalid"))?;
    Ok(json!({"protocol_version":1,"instance_id":instance,"state":"stopped","reason":reason,
        "worker_started":started,"worker_exit_code":value["worker_exit_code"],
        "forced_group_stop":value["forced_group_stop"],"linux_cleanup_confirmed":true,"group_reaped":true,
        "ready":false,"warmup_completed":false}))
}

/// This runs in an owned reader task, never inside the cancelable encode future.
pub(super) async fn read_frame(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Option<Value>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            ensure!(line.is_empty(), "local_memory_truncated_frame");
            return Ok(None);
        }
        let end = available.iter().position(|b| *b == b'\n');
        let take = end.map_or(available.len(), |n| n + 1);
        ensure!(line.len() + take <= MAX_FRAME, "local_memory_frame_too_large");
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if end.is_some() {
            let value: Value = serde_json::from_slice(&line).map_err(|_| anyhow!("local_memory_invalid_json"))?;
            ensure!(value.is_object(), "local_memory_frame_not_object");
            return Ok(Some(value));
        }
    }
}

pub(super) async fn read_events(mut reader: impl AsyncBufRead + Unpin, send: mpsc::Sender<Result<Value,String>>) {
    loop {
        match read_frame(&mut reader).await {
            Ok(Some(value)) => if send.send(Ok(value)).await.is_err() { break; },
            Ok(None) => break,
            Err(_) => { let _ = send.send(Err("local_memory_protocol_read_failed".into())).await; break; }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn response(pending: &Pending) -> Value {
        let mut vector = vec![0.0; 1024]; vector[0] = 1.0;
        json!({"protocol_version":1,"instance_id":"instance","state":"encoded",
            "request_id":pending.request_id,"role":pending.role.as_str(),"input_sha256":pending.input_sha256,
            "text_sha256":pending.input_sha256,"profile":PROFILE.metadata(),"tokens":12,"vector":vector,
            "timings":{"tokenize_ms":1.0,"inference_ms":2.0,"total_ms":3.0}})
    }
    #[test]
    fn exact_text_role_identity_profile_and_normalization_are_required() -> Result<()> {
        let (pending, bytes) = encode_request("instance", "request-1", EmbeddingRole::Query, "中英 mixed\n exact")?;
        let request: Value = serde_json::from_slice(&bytes)?;
        assert_eq!(request["text"], "中英 mixed\n exact");
        assert!(matches!(encoded(&response(&pending), "instance", &pending)?, EncodeResult::Encoded(_)));
        for field in ["instance_id","request_id","role","input_sha256","text_sha256","profile"] {
            let mut wrong = response(&pending); wrong[field] = json!("wrong");
            assert!(encoded(&wrong,"instance",&pending).is_err(), "{field}");
        }
        for vector in [vec![0.0;1024],vec![1.0;1024],vec![0.0;384]] {
            let mut wrong = response(&pending); wrong["vector"] = json!(vector);
            assert!(encoded(&wrong,"instance",&pending).is_err());
        }
        Ok(())
    }
    #[test]
    fn too_long_is_explicit_and_never_returns_a_truncated_vector() -> Result<()> {
        let (pending, _) = encode_request("instance","long",EmbeddingRole::Document,"text")?;
        let mut value = response(&pending);
        value["state"] = json!("encode_error"); value["code"] = json!("input_too_long"); value["tokens"] = json!(513);
        assert!(encoded(&value,"instance",&pending).is_err());
        value.as_object_mut().unwrap().remove("vector");
        assert!(matches!(encoded(&value,"instance",&pending)?, EncodeResult::TooLong { tokens:513, maximum_tokens:512 }));
        assert!(encode_request("instance","bad\nrequest",EmbeddingRole::Query,"text").is_err());
        assert!(encode_request("instance","large",EmbeddingRole::Query,&"\n".repeat(MAX_FRAME/2)).is_err());
        Ok(())
    }
    #[tokio::test]
    async fn bounded_reader_handles_split_frames_and_rejects_partial_or_oversized_lines() -> Result<()> {
        use tokio::io::BufReader;
        let data = b"{\"state\":\"one\"}\n{\"state\":\"two\"}\n";
        let mut reader = BufReader::with_capacity(3, &data[..]);
        assert_eq!(read_frame(&mut reader).await?.unwrap()["state"],"one");
        assert_eq!(read_frame(&mut reader).await?.unwrap()["state"],"two");
        assert!(read_frame(&mut reader).await?.is_none());
        for bytes in [b"{\"state\":\"partial\"}".to_vec(),vec![b' ';MAX_FRAME+1]] {
            assert!(read_frame(&mut BufReader::new(bytes.as_slice())).await.is_err());
        }
        Ok(())
    }
    #[test]
    fn bridge_exit_alone_cannot_substitute_for_linux_group_cleanup() {
        let mut value = json!({"protocol_version":1,"instance_id":"instance","state":"stopped",
            "reason":"owner_stop","worker_started":true,"worker_exit_code":0,"forced_group_stop":false,
            "linux_cleanup_confirmed":true,"group_reaped":true,"ready":false,"warmup_completed":false});
        assert!(stopped(&value,"instance").is_ok());
        value["group_reaped"] = json!(false);
        assert!(stopped(&value,"instance").is_err());
    }
    #[test]
    fn ready_requires_fixed_cpu_precision_threads_and_verified_profile() -> Result<()> {
        let value = json!({"profile":PROFILE.metadata(),"ready":true,"warmup_completed":true,
            "device":"cpu","dtype":"float32","intra_threads":2,"inter_threads":1,
            "cuda_visible_devices":"","load_count":1,"warmup_tokens":5,
            "timings":{"hash_audit_ms":0.0,"model_load_ms":1.0,"warmup_ms":2.0,"total_ms":3.0}});
        assert_eq!(ready(&value)?["profile"], PROFILE.metadata());
        for (field, changed) in [("device",json!("cuda")),("dtype",json!("float16")),
            ("intra_threads",json!(8)),("inter_threads",json!(2)),("load_count",json!(2)),
            ("cuda_visible_devices",json!("0")),("warmup_tokens",json!(513)),("profile",json!({}))] {
            let mut wrong = value.clone(); wrong[field] = changed;
            assert!(ready(&wrong).is_err(), "{field}");
        }
        Ok(())
    }
    #[tokio::test]
    async fn timed_out_wait_does_not_drop_a_partial_response_or_later_stop_receipt() -> Result<()> {
        use tokio::io::{AsyncWriteExt, BufReader};
        use std::time::Duration;
        let (mut writer, reader) = tokio::io::duplex(128);
        let (send, mut receive) = mpsc::channel(8);
        let pump = tokio::spawn(read_events(BufReader::new(reader),send));
        writer.write_all(b"{\"state\":\"enco").await?;
        assert!(tokio::time::timeout(Duration::from_millis(10),receive.recv()).await.is_err());
        writer.write_all(b"ded\"}\n{\"state\":\"stopped\"}\n").await?;
        writer.shutdown().await?;
        assert_eq!(receive.recv().await.unwrap().unwrap()["state"],"encoded");
        assert_eq!(receive.recv().await.unwrap().unwrap()["state"],"stopped");
        assert!(receive.recv().await.is_none());
        pump.await?;
        Ok(())
    }
}
