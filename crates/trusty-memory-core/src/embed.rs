//! Embedding abstraction.
//!
//! Why: We want to swap embedding backends (fastembed local, OpenAI remote, mock
//! for tests) without changing the retrieval code; a trait is the natural seam.
//! What: `Embedder` async trait + `FastEmbedder` stub implementation.
//! Test: `FastEmbedder::new().await?.dimension() == 384`.

use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
    fn dimension(&self) -> usize;
}

/// Local CPU embedder backed by fastembed-rs (ONNX runtime, all-MiniLM-L6-v2).
///
/// Why: Default to local-only embeddings so the service has zero external deps.
/// What: Currently a stub — real implementation will load fastembed model.
/// Test: Once implemented, embed("hello") should return a 384-d vector with
/// non-trivial L2 norm.
pub struct FastEmbedder;

impl FastEmbedder {
    pub async fn new() -> Result<Self> {
        // TODO(impl): initialize fastembed::TextEmbedding with all-MiniLM-L6-v2.
        Ok(Self)
    }
}

#[async_trait]
impl Embedder for FastEmbedder {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        // TODO(impl): call fastembed
        Ok(vec![0.0f32; 384])
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.0f32; 384]).collect())
    }

    fn dimension(&self) -> usize {
        384
    }
}
