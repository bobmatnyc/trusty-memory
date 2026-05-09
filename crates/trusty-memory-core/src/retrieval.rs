//! 4-layer progressive retrieval: L0 (identity) -> L1 (essential) ->
//! L2 (on-demand vector) -> L3 (deep search).
//!
//! Why: LLM context windows are precious. Always loading L0+L1 (~900 tokens)
//! gives the agent baseline grounding; L2/L3 are paid only when the query
//! demands them. This dramatically improves cost and latency vs. dumping the
//! whole memory store into context.
//! What: Layer types, async loaders, and the canonical `PalaceHandle` that
//! owns the per-palace storage handles plus pre-cached L0/L1.
//! Test: `cargo test -p trusty-memory-core retrieval::` exercises L0/L1 cache
//! and L2 vector retrieval end-to-end.

use crate::embed::Embedder;
use crate::palace::{Drawer, PalaceId, RoomType};
use crate::store::kg::KnowledgeGraph;
use crate::store::vector::{UsearchStore, VectorStore};
use anyhow::Result;
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

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

/// A single ranked memory result produced by any retrieval layer.
///
/// Why: All four layers need to produce a comparable, layer-tagged result so
/// callers can stitch them together and present consistent context to the LLM.
/// What: Bundles the matched drawer with an effective score (importance times
/// vector similarity for L2/L3, importance for L1, fixed 1.0 for L0) and the
/// originating layer index.
/// Test: See `l0_l1_always_present` and `l2_returns_relevant_drawer`.
#[derive(Debug, Clone)]
pub struct RecallResult {
    pub drawer: Drawer,
    pub score: f32,
    pub layer: u8,
}

/// Maximum number of drawers held in the L1 cache.
const L1_CAP: usize = 15;

/// Per-palace handle. Cheap to clone (all heavyweight state lives behind `Arc`).
///
/// Why: The registry hands out `Arc<PalaceHandle>` to many concurrent tasks;
/// the handle owns the vector store, KG pool, the in-memory drawer table used
/// by retrieval to map vector hits back to metadata, and the pre-cached L0/L1
/// payloads.
/// What: Bundles `PalaceId`, identity text, an `l1_drawers` Vec (top-15 by
/// importance), `Arc<UsearchStore>`, `Arc<KnowledgeGraph>`, and an
/// `Arc<RwLock<Vec<Drawer>>>` for the in-memory drawer table.
/// Test: See `l0_l1_always_present` (constructor + cache) and
/// `l2_returns_relevant_drawer` (storage handles wired correctly).
pub struct PalaceHandle {
    pub id: PalaceId,
    pub identity: String,
    pub l1_drawers: Vec<Drawer>,
    pub vector_store: Arc<UsearchStore>,
    pub kg: Arc<KnowledgeGraph>,
    pub drawers: Arc<RwLock<Vec<Drawer>>>,
}

impl PalaceHandle {
    /// Construct a new `PalaceHandle` with empty drawer table and L1 cache.
    ///
    /// Why: The registry creates handles eagerly when a palace is opened; the
    /// drawer table and L1 cache are populated incrementally as memories are
    /// loaded or written.
    /// What: Wraps the storage handles in `Arc`s and initializes the drawer
    /// table and L1 cache to empty.
    /// Test: `make_handle` in tests round-trips through this constructor.
    pub fn new(
        id: PalaceId,
        identity: String,
        vector_store: UsearchStore,
        kg: KnowledgeGraph,
    ) -> Self {
        Self {
            id,
            identity,
            l1_drawers: Vec::new(),
            vector_store: Arc::new(vector_store),
            kg: Arc::new(kg),
            drawers: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Append a drawer to the in-memory drawer table.
    ///
    /// Why: Retrieval needs to map vector hits back to drawer metadata; until
    /// we have a persistent drawer table the in-memory `Vec<Drawer>` is the
    /// source of truth.
    /// What: Acquires the write lock on `drawers` and pushes `drawer`. Caller
    /// is responsible for invoking `refresh_l1` if importance ranking might
    /// have changed.
    /// Test: `l0_l1_always_present` exercises this path.
    pub fn add_drawer(&self, drawer: Drawer) {
        let mut drawers = self.drawers.write();
        drawers.push(drawer);
    }

    /// Rebuild the L1 cache (top-15 drawers by importance, descending).
    ///
    /// Why: L1 is the always-on essential context; we keep it pre-sorted so
    /// reads are constant-time. The L1 cap is small enough that a full re-sort
    /// is cheaper than maintaining a heap.
    /// What: Reads the drawer table, sorts a clone by importance descending,
    /// and stores the first `L1_CAP` entries on `self.l1_drawers`.
    /// Test: `l0_l1_always_present` asserts a high-importance drawer makes it
    /// into L1 after `refresh_l1` is called.
    pub fn refresh_l1(&mut self) {
        let drawers = self.drawers.read();
        let mut sorted: Vec<Drawer> = drawers.clone();
        sorted.sort_by(|a, b| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.l1_drawers = sorted.into_iter().take(L1_CAP).collect();
    }
}

/// Compare two UUIDs by their first 8 bytes.
///
/// Why: The vector store keys vectors by the first 8 bytes of a UUID, so
/// search results carry a `Uuid` whose last 8 bytes are zero. Matching these
/// back to drawers must therefore compare prefixes only.
/// What: Returns true if `a` and `b` agree on bytes `0..8`.
/// Test: Implicitly exercised by `l2_returns_relevant_drawer`.
fn uuid_prefix_eq(a: Uuid, b: Uuid) -> bool {
    a.as_bytes()[..8] == b.as_bytes()[..8]
}

/// Build the always-on L0 + L1 portion of a recall.
///
/// Why: Every retrieval flow includes L0+L1; centralizing the construction
/// keeps `recall` and `recall_deep` short and makes L0/L1 layering testable
/// in isolation.
/// What: Emits one `RecallResult { layer: 0, score: 1.0 }` for the identity
/// (only when non-empty), followed by one result per cached L1 drawer with
/// `score = drawer.importance` and `layer: 1`. The L0 result reuses the
/// identity text inside a synthetic `Drawer` so callers can render every
/// layer uniformly.
/// Test: `l0_l1_always_present` asserts both layers appear.
pub fn retrieve_l0_l1(handle: &PalaceHandle) -> Vec<RecallResult> {
    let mut out: Vec<RecallResult> = Vec::with_capacity(1 + handle.l1_drawers.len());

    if !handle.identity.is_empty() {
        // Synthesize a Drawer for the identity so RecallResult stays uniform.
        let identity_drawer = Drawer {
            id: Uuid::nil(),
            room_id: Uuid::nil(),
            content: handle.identity.clone(),
            importance: 1.0,
            source_file: None,
            created_at: chrono::Utc::now(),
            tags: Vec::new(),
            last_accessed_at: None,
            access_count: 0,
        };
        out.push(RecallResult {
            drawer: identity_drawer,
            score: 1.0,
            layer: 0,
        });
    }

    for d in &handle.l1_drawers {
        out.push(RecallResult {
            drawer: d.clone(),
            score: d.importance,
            layer: 1,
        });
    }
    out
}

/// L2 retrieval: metadata-filtered HNSW search.
///
/// Why: Most queries don't need a full deep search — a topic-scoped vector
/// search returns relevant drawers cheaply. Filtering by `RoomType` lets
/// callers narrow into a domain (e.g. only Backend rooms) when intent is
/// known.
/// What: Embeds the query, searches the vector store with `top_k * 3` to
/// leave room for filtering, maps each hit back to a drawer via UUID-prefix
/// match, applies the optional room filter (currently a TODO — see below),
/// scores as `drawer.importance * hit.score`, and returns the top `top_k`
/// drawers tagged with `layer: 2`.
/// Test: `l2_returns_relevant_drawer` upserts a Rust-themed drawer and
/// asserts a Rust-themed query retrieves it at rank 0.
pub async fn retrieve_l2(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    room_filter: Option<RoomType>,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    if top_k == 0 {
        return Ok(Vec::new());
    }
    let embeddings = embedder.embed(&[query.to_string()]).await?;
    let Some(query_vec) = embeddings.into_iter().next() else {
        return Ok(Vec::new());
    };

    let overfetch = top_k.saturating_mul(3).max(top_k);
    let hits = handle.vector_store.search(&query_vec, overfetch).await?;

    let drawers = handle.drawers.read();
    let mut results: Vec<RecallResult> = Vec::with_capacity(hits.len());

    for hit in hits {
        let Some(drawer) = drawers.iter().find(|d| uuid_prefix_eq(d.id, hit.drawer_id)) else {
            // Vector hit refers to a drawer we no longer have metadata for;
            // skip silently — this can happen during partial loads.
            continue;
        };

        // TODO(room-filter): RoomType lives on Room, not Drawer. Once a Room
        // table is wired into PalaceHandle (drawer.room_id -> RoomType), apply
        // the filter here. For now, accept all drawers regardless of filter.
        if room_filter.is_some() {
            // Filter is acknowledged but not yet enforceable — see TODO above.
        }

        results.push(RecallResult {
            drawer: drawer.clone(),
            score: drawer.importance * hit.score,
            layer: 2,
        });
    }
    drop(drawers);

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(top_k);
    Ok(results)
}

/// L3 retrieval: full HNSW deep search across the palace.
///
/// Why: For deep / exploratory queries the agent wants the broadest possible
/// recall; L3 skips the overfetch+filter dance and just returns the top-k
/// nearest neighbors with `layer: 3`.
/// What: Embeds the query, searches with exactly `top_k`, joins each hit to
/// its drawer via UUID-prefix match, scores as `importance * hit.score`,
/// sorts descending, and returns at most `top_k` `RecallResult`s.
/// Test: Symmetric with `l2_returns_relevant_drawer`; same join logic.
pub async fn retrieve_l3(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    if top_k == 0 {
        return Ok(Vec::new());
    }
    let embeddings = embedder.embed(&[query.to_string()]).await?;
    let Some(query_vec) = embeddings.into_iter().next() else {
        return Ok(Vec::new());
    };

    let hits = handle.vector_store.search(&query_vec, top_k).await?;

    let drawers = handle.drawers.read();
    let mut results: Vec<RecallResult> = Vec::with_capacity(hits.len());
    for hit in hits {
        let Some(drawer) = drawers.iter().find(|d| uuid_prefix_eq(d.id, hit.drawer_id)) else {
            continue;
        };
        results.push(RecallResult {
            drawer: drawer.clone(),
            score: drawer.importance * hit.score,
            layer: 3,
        });
    }
    drop(drawers);

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(top_k);
    Ok(results)
}

/// Standard recall = L0 + L1 + L2, deduplicated.
///
/// Why: This is the default path for "hey memory, what do you know about X?"
/// — always-on identity + essentials, plus the cheapest topic search.
/// What: Concatenates `retrieve_l0_l1` and `retrieve_l2` (no room filter),
/// then deduplicates by drawer id keeping the *earlier* (lower-numbered layer)
/// occurrence so L0/L1 always win over an L2 duplicate.
/// Test: Composition is exercised indirectly via the per-layer tests.
pub async fn recall(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    let mut combined = retrieve_l0_l1(handle);
    let l2 = retrieve_l2(handle, embedder, query, None, top_k).await?;
    dedup_extend(&mut combined, l2);
    Ok(combined)
}

/// Deep recall = L0 + L1 + L3, deduplicated.
///
/// Why: When the user explicitly asks for deep search, fall through to L3
/// instead of the metadata-filtered L2.
/// What: Same as `recall` but uses `retrieve_l3` for the heavy layer.
/// Test: Symmetric with `recall`; covered indirectly.
pub async fn recall_deep(
    handle: &PalaceHandle,
    embedder: &dyn Embedder,
    query: &str,
    top_k: usize,
) -> Result<Vec<RecallResult>> {
    let mut combined = retrieve_l0_l1(handle);
    let l3 = retrieve_l3(handle, embedder, query, top_k).await?;
    dedup_extend(&mut combined, l3);
    Ok(combined)
}

/// Extend `base` with entries from `extra` whose drawer id isn't already in
/// `base`. L0/L1 priority is implied by call ordering: pass L0/L1 first.
fn dedup_extend(base: &mut Vec<RecallResult>, extra: Vec<RecallResult>) {
    for r in extra {
        if !base.iter().any(|b| b.drawer.id == r.drawer.id) {
            base.push(r);
        }
    }
}

// -- Legacy stubs (kept for backwards compatibility with existing callers) --

pub struct RetrievalLayers;

impl RetrievalLayers {
    /// Load L0 identity for a palace.
    ///
    /// Why: Provides a stable persona / project description that grounds every
    /// reply, without taking up real context budget.
    /// What: Reads `identity.txt` from the palace data dir; returns empty
    /// content if the file does not yet exist.
    /// Test: For a freshly created palace dir, returns `L0Identity { content: "" }`.
    pub async fn load_l0(_palace_data_dir: &Path) -> Result<L0Identity> {
        Ok(L0Identity {
            content: String::new(),
        })
    }

    /// Load L1 essential drawers.
    ///
    /// Why: Top-importance drawers are queried on virtually every request, so
    /// we want them already in memory and pre-ranked.
    /// What: Returns the top-15 drawers across the palace, sorted by importance.
    /// Test: For an empty palace, returns `L1Essential { drawers: [] }`.
    pub async fn load_l1(_palace_id: &PalaceId) -> Result<L1Essential> {
        Ok(L1Essential {
            drawers: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{kg::KnowledgeGraph, vector::UsearchStore};
    use tempfile::tempdir;

    fn make_handle(dir: &std::path::Path) -> PalaceHandle {
        let vs = UsearchStore::new(dir.join("idx.usearch"), 384).unwrap();
        let kg = KnowledgeGraph::open(&dir.join("kg.db")).unwrap();
        PalaceHandle::new(PalaceId::new("test"), "Test palace".to_string(), vs, kg)
    }

    #[test]
    fn l0_l1_always_present() {
        let dir = tempdir().unwrap();
        let mut handle = make_handle(dir.path());
        let room_id = uuid::Uuid::new_v4();
        let mut d = Drawer::new(room_id, "important fact");
        d.importance = 0.9;
        handle.add_drawer(d);
        handle.refresh_l1();

        let results = retrieve_l0_l1(&handle);
        assert!(results.iter().any(|r| r.layer == 0), "L0 identity missing");
        assert!(results.iter().any(|r| r.layer == 1), "L1 drawer missing");
    }

    #[tokio::test]
    async fn l2_returns_relevant_drawer() {
        let dir = tempdir().unwrap();
        let handle = make_handle(dir.path());
        let embedder = crate::embed::FastEmbedder::new().await.unwrap();

        let room_id = uuid::Uuid::new_v4();
        let drawer = Drawer::new(room_id, "Rust is a systems programming language");
        let drawer_id = drawer.id;

        let vecs = embedder
            .embed(std::slice::from_ref(&drawer.content))
            .await
            .unwrap();
        handle
            .vector_store
            .upsert(drawer_id, vecs[0].clone())
            .await
            .unwrap();
        handle.add_drawer(drawer);

        let results = retrieve_l2(&handle, &embedder, "systems programming Rust", None, 5)
            .await
            .unwrap();
        assert!(!results.is_empty(), "L2 should return results");
        assert!(
            uuid_prefix_eq(results[0].drawer.id, drawer_id),
            "Top L2 result should match the upserted drawer (got {:?}, want {:?})",
            results[0].drawer.id,
            drawer_id
        );
        assert_eq!(results[0].layer, 2);
    }
}
