//! Owned, opt-in CPU embedding transport. Scheduling and Store access live elsewhere.
mod owned;
mod policy;
mod protocol;

pub use owned::OwnedEmbedding;
pub use policy::LocalMemoryPolicy;
pub use protocol::{EncodeResult, Encoded, EmbeddingRole};
