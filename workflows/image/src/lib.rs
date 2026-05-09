//! Image workflow protocol.
//!
//! TS bindings live in `packages/image-protocol`; variant indices and
//! field order here are the wire schema. Adding/reordering arms is a
//! breaking change — bump both sides together.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateParams {
    pub prompt: String,
    /// `None` means the engine picks a fresh u64.
    pub seed: Option<u64>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ImageRequest {
    Generate(GenerateParams),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedImage {
    pub mime: String,
    /// Base64-encoded image bytes — the iframe renders via `data:` URL.
    pub bytes_b64: String,
    /// Seed actually used (echoed so the UI can show / re-seed).
    pub seed: u64,
    /// Wall-clock ms for this generation.
    pub ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ImageOk {
    Image(GeneratedImage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ImageError {
    /// Could not even reach the ComfyUI HTTP endpoint.
    ComfyUnreachable(String),
    /// ComfyUI accepted the prompt but execution failed (missing
    /// checkpoint, OOM, sampler error, …). Message is whatever the
    /// `/history` status reports.
    Comfy(String),
    Internal(String),
}

pub type ImageResponse = Result<ImageOk, ImageError>;

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_allocvec(value)
}

pub fn decode<'de, T: Deserialize<'de>>(bytes: &'de [u8]) -> Result<T, postcard::Error> {
    postcard::from_bytes(bytes)
}
