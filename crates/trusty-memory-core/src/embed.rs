//! Embedding abstraction.
//!
//! Why: We want to swap embedding backends (fastembed local, OpenAI remote, mock
//! for tests) without changing the retrieval code; a trait is the natural seam.
//! What: `Embedder` async trait + `FastEmbedder` (local ONNX, all-MiniLM-L6-v2).
//! Test: `cargo test -p trusty-memory-core embed::` exercises dim and similarity.

use anyhow::{Context, Result};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::task;

/// Output dimension of the all-MiniLM-L6-v2 model.
const EMBED_DIM: usize = 384;

/// Abstraction over embedding backends.
///
/// Why: Decouple retrieval from any specific embedding backend (local ONNX,
/// remote API, deterministic mock for tests).
/// What: Async batched embed + a synchronous dimension accessor.
/// Test: Implementations are exercised via `FastEmbedder` tests below; mock
/// implementations live in unit tests of dependent modules.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Embed a batch of texts. Returns one `Vec<f32>` per input, each of length
    /// `self.dim()`.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Output dimension of the produced embeddings.
    fn dim(&self) -> usize;
}

/// Local CPU embedder backed by fastembed-rs (ONNX runtime, all-MiniLM-L6-v2).
///
/// Why: Default to local-only embeddings so the service has zero external
/// network deps and predictable latency.
/// What: Wraps a single `TextEmbedding` instance behind `Arc<Mutex<_>>`. The
/// `embed` call on `TextEmbedding` requires `&mut self`, so we serialize calls
/// per-embedder; concurrency is achieved at the palace level (one embedder per
/// palace) and batching (callers can pass many texts in a single call).
/// Test: `embed_returns_correct_dim` verifies shape; `similar_sentences_have_
/// high_cosine_similarity` verifies the embeddings carry real semantic signal.
pub struct FastEmbedder {
    model: Arc<Mutex<TextEmbedding>>,
}

impl FastEmbedder {
    /// Construct a new `FastEmbedder`, downloading the model on first use.
    ///
    /// Why: Initialization is heavy (ONNX session creation + optional model
    /// download). Doing it once at startup keeps the hot path cheap. We warm
    /// the model with a tiny batch so the first user query doesn't pay the
    /// one-shot ORT graph compilation cost.
    /// What: Creates a `TextEmbedding` for `AllMiniLML6V2` on a blocking
    /// thread, warms it with 5 short sentences, returns it wrapped in
    /// `Arc<Mutex<_>>`.
    /// Test: `embed_returns_correct_dim` constructs an embedder and calls
    /// `embed` — both succeeding implies `new` worked end-to-end.
    pub async fn new() -> Result<Self> {
        let model = task::spawn_blocking(|| -> Result<TextEmbedding> {
            let mut model =
                TextEmbedding::try_new(TextInitOptions::new(EmbeddingModel::AllMiniLML6V2))
                    .context("failed to initialize fastembed all-MiniLM-L6-v2")?;

            // Warm the ORT session with a small batch so the first real query
            // doesn't pay the graph-compile cost.
            let warmup: Vec<&str> = vec![
                "hello world",
                "the quick brown fox",
                "memory palace warmup",
                "trusty memory service",
                "embedding model ready",
            ];
            let _ = model
                .embed(warmup, None)
                .context("fastembed warmup batch failed")?;
            Ok(model)
        })
        .await
        .context("spawn_blocking joined with error during embedder init")??;

        Ok(Self {
            model: Arc::new(Mutex::new(model)),
        })
    }
}

#[async_trait]
impl Embedder for FastEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let model = Arc::clone(&self.model);
        let texts = texts.to_vec();
        task::spawn_blocking(move || -> Result<Vec<Vec<f32>>> {
            let mut guard = model.lock();
            let out = guard
                .embed(texts, None)
                .context("fastembed embed call failed")?;
            Ok(out)
        })
        .await
        .context("spawn_blocking joined with error during embed")?
    }

    fn dim(&self) -> usize {
        EMBED_DIM
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::OnceCell;

    /// Shared embedder for tests.
    ///
    /// Why: fastembed's on-disk model cache uses a file lock during download;
    /// running multiple `FastEmbedder::new()` calls in parallel (cargo's
    /// default) races on that lock and fails on a cold cache. Sharing one
    /// embedder across tests is also faster.
    static SHARED: OnceCell<Arc<FastEmbedder>> = OnceCell::const_new();

    async fn shared_embedder() -> Arc<FastEmbedder> {
        SHARED
            .get_or_init(|| async { Arc::new(FastEmbedder::new().await.unwrap()) })
            .await
            .clone()
    }

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        dot / (na * nb)
    }

    #[tokio::test]
    async fn embed_returns_correct_dim() {
        let embedder = shared_embedder().await;
        assert_eq!(embedder.dim(), 384);
        let vecs = embedder.embed(&["hello world".to_string()]).await.unwrap();
        assert_eq!(vecs.len(), 1);
        assert_eq!(vecs[0].len(), 384);
    }

    #[tokio::test]
    async fn similar_sentences_have_high_cosine_similarity() {
        let embedder = shared_embedder().await;
        let vecs = embedder
            .embed(&[
                "The cat sat on the mat".to_string(),
                "A cat is sitting on a mat".to_string(),
                "Quantum mechanics and black holes".to_string(),
            ])
            .await
            .unwrap();
        let sim_similar = cosine(&vecs[0], &vecs[1]);
        let sim_different = cosine(&vecs[0], &vecs[2]);
        assert!(sim_similar > 0.7, "similar pair cosine = {}", sim_similar);
        assert!(
            sim_similar > sim_different,
            "similar should be closer than dissimilar (similar={}, different={})",
            sim_similar,
            sim_different
        );
    }
}
