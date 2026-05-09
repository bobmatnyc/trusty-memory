//! Concurrent palace registry.
//!
//! Why: The service is machine-wide and must serve many concurrent requests
//! across multiple palaces; a `DashMap<PalaceId, Arc<PalaceHandle>>` lets
//! lookups proceed without blocking other readers or writers.
//! What: Wraps a `DashMap` with register / get / list helpers. The
//! `PalaceHandle` type re-exported here is the canonical retrieval handle from
//! [`crate::retrieval`] — there is exactly one `PalaceHandle` in the crate.
//! Test: Register two palaces on separate tasks, assert both visible via `list()`.

use crate::palace::PalaceId;
use crate::retrieval::PalaceHandle;
use dashmap::DashMap;
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
