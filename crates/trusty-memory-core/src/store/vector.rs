//! Vector store trait and usearch HNSW implementation stub.
//!
//! Why: Most queries hit the vector index; making it pluggable lets us mock it in
//! tests and swap implementations without touching retrieval code.
//! What: `VectorStore` async trait + `UsearchStore` placeholder.
//! Test: Once implemented, `upsert` then `search` should return the inserted id
//! at rank 0 with score >= 0.99 for an identical query vector.

use anyhow::Result;
use async_trait::async_trait;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct VectorHit {
    pub drawer_id: Uuid,
    pub score: f32,
}

#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn upsert(&self, id: Uuid, embedding: Vec<f32>) -> Result<()>;
    async fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<VectorHit>>;
    async fn remove(&self, id: Uuid) -> Result<()>;
}

/// usearch HNSW-backed store.
///
/// Why: usearch gives us high-quality HNSW with disk persistence and a tiny C++
/// dependency. We wrap the index in `Arc<RwLock<_>>` so many concurrent reads
/// (search) never block each other; only mutations take the write lock.
/// What: Stub — real impl will hold an `Arc<RwLock<usearch::Index>>`.
/// Test: Insert a vector, search with the same vector, expect score ~ 1.0.
pub struct UsearchStore;
