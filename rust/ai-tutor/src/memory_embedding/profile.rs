//! Fixed vector identities. This does not load models or enable recall.
use super::{FP32_MODEL_SHA, INT8_MODEL_SHA, Precision, REVISION, TOKENIZER_SHA};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Native profiles identify the already-frozen offline Torch vectors, not an
/// installed Rust inference backend. An export is a distinct weight identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbeddingProfile {
    E5OnnxInt8,
    E5OnnxFp32,
    E5NativeFp32,
    BgeM3NativeFp32,
}

impl EmbeddingProfile {
    pub const ALL: [Self; 4] = [
        Self::E5OnnxInt8,
        Self::E5OnnxFp32,
        Self::E5NativeFp32,
        Self::BgeM3NativeFp32,
    ];

    pub fn e5_onnx(precision: Precision) -> Self {
        match precision {
            Precision::Int8 => Self::E5OnnxInt8,
            Precision::Fp32 => Self::E5OnnxFp32,
        }
    }
    pub fn dimension(self) -> usize {
        match self {
            Self::BgeM3NativeFp32 => 1024,
            _ => 384,
        }
    }
    pub fn model(self) -> &'static str {
        match self {
            Self::BgeM3NativeFp32 => "BAAI/bge-m3",
            _ => "intfloat/multilingual-e5-small",
        }
    }
    pub fn revision(self) -> &'static str {
        match self {
            Self::BgeM3NativeFp32 => "5617a9f61b028005a4858fdac845db406aefb181",
            _ => REVISION,
        }
    }
    pub fn weight_sha256(self) -> &'static str {
        match self {
            Self::E5OnnxInt8 => INT8_MODEL_SHA,
            Self::E5OnnxFp32 => FP32_MODEL_SHA,
            Self::E5NativeFp32 => {
                "1a55775f53449dac10a2bcbc312469fac40b96d53198c407081a831f81c98477"
            }
            Self::BgeM3NativeFp32 => {
                "b5e0ce3470abf5ef3831aa1bd5553b486803e83251590ab7ff35a117cf6aad38"
            }
        }
    }
    pub fn tokenizer_sha256(self) -> &'static str {
        match self {
            Self::BgeM3NativeFp32 => {
                "21106b6d7dab2952c1d496fb21d5dc9db75c28ed361a05f5020bbba27810dd08"
            }
            _ => TOKENIZER_SHA,
        }
    }
    pub fn pooling(self) -> &'static str {
        match self {
            Self::BgeM3NativeFp32 => "cls-l2-v1",
            _ => "mask-mean-l2-v1",
        }
    }
    pub fn query_prefix(self) -> &'static str {
        match self {
            Self::BgeM3NativeFp32 => "",
            _ => "query: ",
        }
    }
    pub fn document_prefix(self) -> &'static str {
        match self {
            Self::BgeM3NativeFp32 => "",
            _ => "passage: ",
        }
    }
    pub fn model_key(self) -> String {
        // Preserve the two deployed E5 keys byte-for-byte. Model identity is
        // fixed by this registry's immutable revision/artifact mapping.
        format!(
            "{:x}",
            Sha256::digest(
                json!([
                    self.revision(),
                    self.weight_sha256(),
                    self.tokenizer_sha256(),
                    self.query_prefix(),
                    self.document_prefix(),
                    self.pooling(),
                    self.dimension()
                ])
                .to_string()
                .as_bytes()
            )
        )
    }
    pub fn from_model_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|profile| profile.model_key() == key)
    }
    pub fn metadata(self) -> Value {
        json!({"model":self.model(),"revision":self.revision(),"model_key":self.model_key(),
            "weight_sha256":self.weight_sha256(),"tokenizer_sha256":self.tokenizer_sha256(),
            "pooling":self.pooling(),"query_prefix":self.query_prefix(),"document_prefix":self.document_prefix(),
            "dimension":self.dimension(),"artifact":match self {
                Self::E5OnnxInt8=>"onnx-int8",Self::E5OnnxFp32=>"onnx-fp32",
                Self::E5NativeFp32=>"safetensors-native-fp32",Self::BgeM3NativeFp32=>"pytorch-bin-native-fp32"},
            "normalization":"L2","maximum_tokens":512})
    }
    pub fn normalized(self, vector: &[f32]) -> bool {
        vector.len() == self.dimension()
            && vector.iter().all(|value| value.is_finite())
            && (vector
                .iter()
                .map(|value| (*value as f64).powi(2))
                .sum::<f64>()
                - 1.0)
                .abs()
                <= 0.001
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_e5_keys_remain_stable_and_all_artifacts_are_isolated() {
        assert_eq!(
            EmbeddingProfile::E5OnnxInt8.model_key(),
            "3804e3b06d6f86abe39d18a439e34e8fe451cdae3d0e9e4ebb33709cfc8d1dda"
        );
        assert_eq!(
            EmbeddingProfile::E5OnnxFp32.model_key(),
            "34257e5ef9d55b2f5395e0f04197a0c212c215e097755a9eeb26f103e7a1d319"
        );
        let mut keys = std::collections::HashSet::new();
        for profile in EmbeddingProfile::ALL {
            assert!(keys.insert(profile.model_key()));
            assert_eq!(
                EmbeddingProfile::from_model_key(&profile.model_key()),
                Some(profile)
            );
        }
        assert_eq!(EmbeddingProfile::from_model_key("invented-artifact"), None);
    }
    #[test]
    fn profile_dimension_is_required_even_when_both_vectors_are_normalized() {
        let mut e5 = vec![0.0; 384];
        e5[0] = 1.0;
        let mut bge = vec![0.0; 1024];
        bge[0] = 1.0;
        assert!(EmbeddingProfile::E5NativeFp32.normalized(&e5));
        assert!(EmbeddingProfile::BgeM3NativeFp32.normalized(&bge));
        assert!(!EmbeddingProfile::E5NativeFp32.normalized(&bge));
        assert!(!EmbeddingProfile::BgeM3NativeFp32.normalized(&e5));
        assert!(!EmbeddingProfile::BgeM3NativeFp32.normalized(&vec![f32::NAN; 1024]));
        assert!(!EmbeddingProfile::BgeM3NativeFp32.normalized(&vec![0.0; 1024]));
    }
}
