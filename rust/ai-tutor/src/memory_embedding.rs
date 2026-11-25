//! Local fixed-revision E5 encoder. Own one session on an inference worker;
//! do not hold Store locks during encoding. No download or remote model calls.
use anyhow::{Context, Result, anyhow, bail, ensure};
use ort::{
    execution_providers::CPUExecutionProvider,
    session::{RunOptions, Session},
    value::Tensor,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fs, io::Read, path::Path, time::Instant};
use tokenizers::Tokenizer;
mod profile;
pub use profile::EmbeddingProfile;

const REVISION: &str = "614241f622f53c4eeff9890bdc4f31cfecc418b3";
const INT8_MODEL_FILE: &str = "onnx/model_qint8_avx512_vnni.onnx";
const INT8_MODEL_SHA: &str = "dd476dd0c2514e9b9be83aeb3853fac0763e0bdf4a71645407587d77c48a2d88";
const FP32_MODEL_FILE: &str = "onnx/model.onnx";
const FP32_MODEL_SHA: &str = "ca456c06b3a9505ddfd9131408916dd79290368331e7d76bb621f1cba6bc8665";
pub const TOKENIZER_SHA: &str = "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39";
const DLL_SHA: &str = "2bc04b05efbff97049132939640963772cc16225ba1c7f1388993487c8775492";
pub const DIM: usize = 384;

#[derive(Clone, Copy)]
pub enum Precision {
    Int8,
    Fp32,
}
impl Precision {
    fn artifact(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Int8 => ("int8", INT8_MODEL_FILE, INT8_MODEL_SHA),
            Self::Fp32 => ("fp32", FP32_MODEL_FILE, FP32_MODEL_SHA),
        }
    }
}

fn sha(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("Missing local artifact: {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub struct Encoded {
    pub vector: Vec<f32>,
    pub tokens: usize,
    pub tokenize_ms: f64,
    pub inference_ms: f64,
    pub total_ms: f64,
}
pub struct E5Embedder {
    tokenizer: Tokenizer,
    session: Session,
    metadata: Value,
}
impl E5Embedder {
    pub fn load(root: &Path, threads: usize, precision: Precision) -> Result<Self> {
        ensure!(
            (1..=2).contains(&threads),
            "Embedding CPU threads must be 1 or 2"
        );
        let started = Instant::now();
        let directory = root.join("models/memory/e5-small");
        let dll = root.join("models/native/onnxruntime.dll");
        let profile = EmbeddingProfile::e5_onnx(precision);
        let (precision, model_file, model_sha) = precision.artifact();
        ensure!(
            sha(&directory.join(model_file))? == model_sha,
            "Pinned E5 ONNX hash mismatch"
        );
        ensure!(
            sha(&directory.join("tokenizer.json"))? == TOKENIZER_SHA,
            "Pinned E5 tokenizer hash mismatch"
        );
        ensure!(
            sha(&dll)? == DLL_SHA,
            "Project ONNX Runtime DLL hash mismatch"
        );
        let audit_ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut tokenizer = Tokenizer::from_file(directory.join("tokenizer.json"))
            .map_err(|error| anyhow!("Tokenizer: {error}"))?;
        tokenizer
            .with_truncation(None)
            .map_err(|error| anyhow!("Tokenizer truncation configuration: {error}"))?;
        tokenizer.with_padding(None);
        ort::init_from(dll.display().to_string())
            .with_telemetry(false)
            .commit()?;
        let session = Session::builder()?
            .with_execution_providers([CPUExecutionProvider::default().build()])?
            .with_intra_threads(threads)?
            .with_inter_threads(1)?
            .commit_from_file(directory.join(model_file))?;
        let names: HashSet<_> = session
            .inputs
            .iter()
            .map(|input| input.name.as_str())
            .collect();
        ensure!(
            names.contains("input_ids")
                && names.contains("attention_mask")
                && names
                    .iter()
                    .all(|name| ["input_ids", "attention_mask", "token_type_ids"].contains(name)),
            "Unsupported E5 input contract"
        );
        ensure!(
            session
                .outputs
                .iter()
                .any(|output| output.name == "last_hidden_state"),
            "E5 last_hidden_state output missing"
        );
        let inputs: Vec<_> = session
            .inputs
            .iter()
            .map(|input| json!({"name":input.name,"type":format!("{:?}",input.input_type)}))
            .collect();
        let outputs: Vec<_> = session
            .outputs
            .iter()
            .map(|output| json!({"name":output.name,"type":format!("{:?}",output.output_type)}))
            .collect();
        let model_key = profile.model_key();
        let metadata = json!({"model":"intfloat/multilingual-e5-small","model_key":model_key,"revision":REVISION,"precision":precision,"model_file":model_file,"model_sha256":model_sha,"tokenizer_sha256":TOKENIZER_SHA,"runtime_dll_sha256":DLL_SHA,
            "license":"MIT","dimension":DIM,"max_tokens":512,"query_prefix":"query: ","document_prefix":"passage: ","pooling":"attention-mask mean then L2","execution_provider":"CPU",
            "intra_threads":threads,"inter_threads":1,"ort_binding_api":ort::MINOR_VERSION,"ort_runtime_build":ort::info(),"model_inputs":inputs,"model_outputs":outputs,
            "hash_audit_ms":audit_ms,"load_total_ms":started.elapsed().as_secs_f64()*1000.0,"cpu_features":cpu_features(),
            "precision_parity":"not_decided_by_a_single_precision_run","profile":profile.metadata()});
        Ok(Self {
            tokenizer,
            session,
            metadata,
        })
    }
    pub fn metadata(&self) -> &Value {
        &self.metadata
    }
    pub fn model_key(&self) -> &str {
        self.metadata["model_key"].as_str().unwrap()
    }
    pub fn embed_query(&mut self, text: &str, options: Option<&RunOptions>) -> Result<Encoded> {
        self.encode("query", text, options)
    }
    pub fn embed_document(&mut self, text: &str, options: Option<&RunOptions>) -> Result<Encoded> {
        self.encode("passage", text, options)
    }
    fn encode(&mut self, role: &str, text: &str, options: Option<&RunOptions>) -> Result<Encoded> {
        ensure!(
            ["query", "passage"].contains(&role),
            "Invalid embedding role"
        );
        let start = Instant::now();
        let encoding = self
            .tokenizer
            .encode(format!("{role}: {text}"), true)
            .map_err(|error| anyhow!("Tokenization failed: {error}"))?;
        let tokens = encoding.len();
        ensure!(
            (1..=512).contains(&tokens),
            "Embedding input exceeds 512 tokens; no silent truncation"
        );
        let tokenize_ms = start.elapsed().as_secs_f64() * 1000.0;
        let mut inputs = vec![];
        for input in &self.session.inputs {
            let values: Vec<i64> = match input.name.as_str() {
                "input_ids" => encoding
                    .get_ids()
                    .iter()
                    .map(|value| *value as i64)
                    .collect(),
                "attention_mask" => encoding
                    .get_attention_mask()
                    .iter()
                    .map(|value| *value as i64)
                    .collect(),
                "token_type_ids" => encoding
                    .get_type_ids()
                    .iter()
                    .map(|value| *value as i64)
                    .collect(),
                _ => bail!("Unknown model input"),
            };
            inputs.push((
                input.name.clone(),
                Tensor::from_array(([1, tokens], values))?,
            ));
        }
        let infer = Instant::now();
        let outputs = match options {
            Some(options) => self.session.run_with_options(inputs, options)?,
            None => self.session.run(inputs)?,
        };
        let inference_ms = infer.elapsed().as_secs_f64() * 1000.0;
        let (shape, values) = outputs["last_hidden_state"].try_extract_tensor::<f32>()?;
        ensure!(
            shape.len() == 3
                && shape[0] == 1
                && shape[1] == tokens as i64
                && shape[2] == DIM as i64,
            "Unexpected E5 output shape"
        );
        let vector = pool(values, encoding.get_attention_mask(), DIM)?;
        Ok(Encoded {
            vector,
            tokens,
            tokenize_ms,
            inference_ms,
            total_ms: start.elapsed().as_secs_f64() * 1000.0,
        })
    }
}

fn pool(values: &[f32], mask: &[u32], dimension: usize) -> Result<Vec<f32>> {
    ensure!(
        dimension > 0 && values.len() == mask.len() * dimension,
        "Invalid pooling shape"
    );
    let mut vector = vec![0.0; dimension];
    let mut count = 0usize;
    for (index, token) in values.chunks_exact(dimension).enumerate() {
        if mask[index] == 0 {
            continue;
        }
        ensure!(
            token.iter().all(|value| value.is_finite()),
            "Non-finite embedding tensor"
        );
        count += 1;
        for (sum, value) in vector.iter_mut().zip(token) {
            *sum += value;
        }
    }
    ensure!(count > 0, "All embedding positions are padding");
    for value in &mut vector {
        *value /= count as f32;
    }
    let norm = vector
        .iter()
        .map(|value| (*value as f64).powi(2))
        .sum::<f64>()
        .sqrt();
    ensure!(norm.is_finite() && norm > 0.0, "Invalid embedding norm");
    for value in &mut vector {
        *value = (*value as f64 / norm) as f32;
    }
    Ok(vector)
}
pub fn cosine(left: &[f32], right: &[f32]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(a, b)| *a as f64 * *b as f64)
        .sum()
}

#[cfg(target_arch = "x86_64")]
fn cpu_features() -> Value {
    // CPUID is available on x86_64. The AVX2 test additionally checks OS support.
    let vnni = std::arch::x86_64::__cpuid_count(7, 1).eax & (1 << 4) != 0;
    json!({"architecture":"x86_64","avx2":std::is_x86_feature_detected!("avx2"),"avx_vnni_cpuid":vnni,"avx512f":std::is_x86_feature_detected!("avx512f"),"avx512_vnni":std::is_x86_feature_detected!("avx512vnni")})
}
#[cfg(not(target_arch = "x86_64"))]
fn cpu_features() -> Value {
    json!({"architecture":std::env::consts::ARCH})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attention_mask_pooling_excludes_padding_and_normalizes() -> Result<()> {
        let pooled = pool(&[3.0, 0.0, 999.0, 999.0, 0.0, 4.0], &[1, 0, 1], 2)?;
        ensure!((pooled[0] - 0.6).abs() < 1e-6 && (pooled[1] - 0.8).abs() < 1e-6);
        ensure!(pool(&[1.0, 2.0], &[0], 2).is_err());
        ensure!(pool(&[f32::NAN, 1.0], &[1], 2).is_err());
        Ok(())
    }
}
