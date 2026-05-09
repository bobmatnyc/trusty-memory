//! Storage backends: vector index (HNSW) + temporal knowledge graph (SQLite).
//!
//! Why: Two complementary data shapes — dense vectors for semantic recall and
//! triples-with-time for relational facts — covered by separate modules so each
//! can evolve independently.
//! What: Re-exports `VectorStore` trait and `KnowledgeGraph` type.
//! Test: See submodule tests.

pub mod kg;
pub mod kuzu;
pub mod vector;

pub use kg::{KnowledgeGraph, Triple};
pub use vector::{VectorHit, VectorStore};
