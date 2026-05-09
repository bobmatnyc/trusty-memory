//! Concurrent palace registry.
//!
//! Why: The service is machine-wide and must serve many concurrent requests
//! across multiple palaces; a `DashMap<PalaceId, Arc<PalaceHandle>>` lets
//! lookups proceed without blocking other readers or writers.
//! What: Wraps a `DashMap` with register / get / list helpers. The
//! `PalaceHandle` type re-exported here is the canonical retrieval handle from
//! [`crate::retrieval`] — there is exactly one `PalaceHandle` in the crate.
//! Test: Register two palaces on separate tasks, assert both visible via `list()`.

use crate::palace::{Palace, PalaceId};
use crate::retrieval::PalaceHandle;
use crate::store::palace_store::PalaceStore;
use anyhow::{Context, Result};
use dashmap::DashMap;
use std::path::Path;
use std::sync::Arc;

#[derive(Default, Clone)]
pub struct PalaceRegistry {
    palaces: Arc<DashMap<PalaceId, Arc<PalaceHandle>>>,
}

impl PalaceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a new palace handle, replacing any prior entry with the same id.
    ///
    /// Why: Registry is the single source of truth for live palaces; callers
    /// hand off ownership of a freshly built handle and the registry shares it
    /// behind an `Arc` to all concurrent readers.
    /// What: Reads `handle.id`, wraps the handle in `Arc`, and inserts.
    /// Test: `register_and_get_roundtrip` re-fetches by id and compares.
    pub fn register(&self, handle: PalaceHandle) {
        let id = handle.id.clone();
        self.palaces.insert(id, Arc::new(handle));
    }

    /// Insert an already-shared handle. Useful when the caller wants to keep
    /// its own `Arc` reference (e.g. to mutate L1 caches under a separate lock).
    pub fn register_arc(&self, handle: Arc<PalaceHandle>) {
        let id = handle.id.clone();
        self.palaces.insert(id, handle);
    }

    /// Cheap clone of the `Arc` — no locking, never blocks readers.
    pub fn get(&self, id: &PalaceId) -> Option<Arc<PalaceHandle>> {
        self.palaces.get(id).map(|r| r.clone())
    }

    pub fn list(&self) -> Vec<PalaceId> {
        self.palaces.iter().map(|r| r.key().clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.palaces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.palaces.is_empty()
    }

    /// Open a palace by id, hydrating from `<data_root>/<palace_id>/` on disk.
    ///
    /// Why: The CLI and MCP server look palaces up by id; this is the single
    /// entry point for reconstructing a `PalaceHandle` from disk and
    /// memoizing it in the registry.
    /// What: Returns the cached `Arc<PalaceHandle>` if present; otherwise loads
    /// metadata via `PalaceStore::load_palace`, calls `PalaceHandle::open`, and
    /// inserts the handle.
    /// Test: `registry_create_and_open` round-trips create -> drop -> reopen.
    pub fn open_palace(&self, data_root: &Path, palace_id: &PalaceId) -> Result<Arc<PalaceHandle>> {
        if let Some(h) = self.get(palace_id) {
            return Ok(h);
        }
        let palace_dir = data_root.join(palace_id.as_str());
        let palace = PalaceStore::load_palace(&palace_dir)
            .with_context(|| format!("load palace metadata for {palace_id}"))?;
        let handle = PalaceHandle::open(&palace)?;
        self.register_arc(handle.clone());
        Ok(handle)
    }

    /// Create and persist a new palace, then open it.
    ///
    /// Why: `palace new` saves metadata and immediately wants a working handle
    /// for further operations; combining the steps avoids a TOCTOU between
    /// save and open.
    /// What: Computes `data_dir = data_root/<id>`, writes `palace.json`, and
    /// returns a freshly opened handle (registered in the registry).
    /// Test: `registry_create_and_open`.
    pub fn create_palace(&self, data_root: &Path, mut palace: Palace) -> Result<Arc<PalaceHandle>> {
        // Always anchor data_dir under data_root/<id> so callers can pass a
        // bare Palace without worrying about path layout.
        let palace_dir = data_root.join(palace.id.as_str());
        palace.data_dir = palace_dir.clone();
        std::fs::create_dir_all(&palace_dir)
            .with_context(|| format!("create palace dir {}", palace_dir.display()))?;
        PalaceStore::save_palace(&palace)
            .with_context(|| format!("save palace metadata for {}", palace.id))?;
        let handle = PalaceHandle::open(&palace)?;
        self.register_arc(handle.clone());
        Ok(handle)
    }

    /// List every palace persisted under `data_root`.
    ///
    /// Why: `palace list` and `status` need a registry-wide view that survives
    /// across daemon restarts.
    /// What: Delegates to `PalaceStore::list_palaces`.
    /// Test: `list_palaces_finds_saved_palaces` in the palace_store module
    /// covers the underlying walker.
    pub fn list_palaces(data_root: &Path) -> Result<Vec<Palace>> {
        PalaceStore::list_palaces(data_root)
            .with_context(|| format!("list palaces under {}", data_root.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{kg::KnowledgeGraph, vector::UsearchStore};
    use tempfile::tempdir;

    fn make_handle(id: &str, dir: &std::path::Path) -> PalaceHandle {
        let vs = UsearchStore::new(dir.join(format!("{id}.usearch")), 384).unwrap();
        let kg = KnowledgeGraph::open(&dir.join(format!("{id}.db"))).unwrap();
        PalaceHandle::new(PalaceId::new(id), format!("Identity for {id}"), vs, kg)
    }

    #[test]
    fn register_and_get_roundtrip() {
        let dir = tempdir().unwrap();
        let reg = PalaceRegistry::new();
        reg.register(make_handle("alpha", dir.path()));
        let h = reg.get(&PalaceId::new("alpha")).expect("registered");
        assert_eq!(h.id.as_str(), "alpha");
    }

    #[test]
    fn registry_create_and_open() {
        use crate::palace::Palace;
        use chrono::Utc;

        let dir = tempdir().unwrap();
        let data_root = dir.path();

        let palace = Palace {
            id: PalaceId::new("alpha"),
            name: "Alpha".to_string(),
            description: Some("test".to_string()),
            created_at: Utc::now(),
            data_dir: data_root.join("alpha"),
        };

        // Create through the registry.
        {
            let reg = PalaceRegistry::new();
            let handle = reg
                .create_palace(data_root, palace.clone())
                .expect("create_palace");
            assert_eq!(handle.id, PalaceId::new("alpha"));
            // Persist a tiny identity directly (PalaceHandle.identity is set
            // at open time so we mutate via PalaceStore for the test).
            crate::store::palace_store::PalaceStore::save_identity(
                &handle.id,
                "I am Alpha",
                handle.data_dir.as_ref().expect("data_dir set"),
            )
            .expect("save identity");
        }

        // Drop the registry, reopen from disk.
        let reg2 = PalaceRegistry::new();
        let handle2 = reg2
            .open_palace(data_root, &PalaceId::new("alpha"))
            .expect("open_palace");
        assert_eq!(handle2.id, PalaceId::new("alpha"));
        assert_eq!(handle2.identity, "I am Alpha");

        // list_palaces sees it too.
        let palaces = PalaceRegistry::list_palaces(data_root).unwrap();
        assert_eq!(palaces.len(), 1);
        assert_eq!(palaces[0].name, "Alpha");
    }

    #[test]
    fn list_contains_all_registered() {
        let dir = tempdir().unwrap();
        let reg = PalaceRegistry::new();
        reg.register(make_handle("a", dir.path()));
        reg.register(make_handle("b", dir.path()));
        let ids: Vec<_> = reg.list().into_iter().map(|p| p.0).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"a".to_string()));
        assert!(ids.contains(&"b".to_string()));
    }
}
