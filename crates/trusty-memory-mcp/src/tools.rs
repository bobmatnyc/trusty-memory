//! MCP tool surface for trusty-memory.
//!
//! Why: Concentrates the public tool contract in one file so changes are
//! auditable and the MCP schema stays in sync with the implementation.
//! What: Defines `MemoryMcpServer` and the planned tool list.
//! Test: Schema generation will be unit-tested once tools are implemented.
//!
//! Planned tools:
//! - `memory_remember(palace, text, room?, tags?)` -> drawer_id
//! - `memory_recall(palace, query, top_k?)`        -> Vec<Drawer> (L0+L1+L2)
//! - `memory_recall_deep(palace, query, top_k?)`   -> Vec<Drawer> (L3 deep)
//! - `palace_create(name, description?)`           -> PalaceId
//! - `palace_list()`                                -> Vec<PalaceId>
//! - `kg_assert(palace, subject, predicate, object, confidence?)` -> ()
//! - `kg_query(palace, subject)`                    -> Vec<Triple>

pub struct MemoryMcpServer;

impl MemoryMcpServer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MemoryMcpServer {
    fn default() -> Self {
        Self::new()
    }
}
