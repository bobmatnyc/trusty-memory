//! 4-layer progressive retrieval: L0 (identity) -> L1 (essential) ->
//! L2 (on-demand vector) -> L3 (deep search).
//!
//! Why: LLM context windows are precious. Always loading L0+L1 (~900 tokens)
//! gives the agent baseline grounding; L2/L3 are paid only when the query
//! demands them. This dramatically improves cost and latency vs. dumping the
//! whole memory store into context.
//! What: Layer types and async loaders. L0/L1 are pre-cached on palace open.
//! Test: `load_l0` / `load_l1` return placeholder content for an empty palace.

use crate::palace::{Drawer, PalaceId};
use std::path::Path;

/// L0 — palace identity. Tiny (~100 tokens), always loaded, read from
/// `<data_dir>/identity.txt` on palace open.
pub struct L0Identity {
    pub content: String,
}

/// L1 — essential drawers (top-15 by importance, ~800 tokens), pre-computed
/// at write time and cached on the `PalaceHandle`.
pub struct L1Essential {
    pub drawers: Vec<Drawer>,
}

pub struct RetrievalLayers;

impl RetrievalLayers {
    /// Load L0 identity for a palace.
    ///
    /// Why: Provides a stable persona / project description that grounds every
    /// reply, without taking up real context budget.
    /// What: Reads `identity.txt` from the palace data dir; returns empty
    /// content if the file does not yet exist.
    /// Test: For a freshly created palace dir, returns `L0Identity { content: "" }`.
    pub async fn load_l0(_palace_data_dir: &Path) -> anyhow::Result<L0Identity> {
        // TODO(impl): tokio::fs::read_to_string with graceful "not found" handling.
        Ok(L0Identity { content: String::new() })
    }

    /// Load L1 essential drawers.
    ///
    /// Why: Top-importance drawers are queried on virtually every request, so
    /// we want them already in memory and pre-ranked.
    /// What: Returns the top-15 drawers across the palace, sorted by importance.
    /// Test: For an empty palace, returns `L1Essential { drawers: [] }`.
    pub async fn load_l1(_palace_id: &PalaceId) -> anyhow::Result<L1Essential> {
        Ok(L1Essential { drawers: Vec::new() })
    }

    // L2 (on-demand metadata-filtered vector search) and L3 (full deep search)
    // will be added once VectorStore + KnowledgeGraph are wired up.
}
