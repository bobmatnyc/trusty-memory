//! Concurrent palace registry.
//!
//! Why: The service is machine-wide and must serve many concurrent requests
//! across multiple palaces; a `DashMap<PalaceId, Arc<PalaceHandle>>` lets
//! lookups proceed without blocking other readers or writers.
//! What: Wraps a `DashMap` with register / get / list helpers.
//! Test: Register two palaces on separate tasks, assert both visible via `list()`.

use crate::palace::{Palace, PalaceId};
use dashmap::DashMap;
use std::sync::Arc;

/// Per-palace handle. Will own the vector store, KG connection pool, and
/// pre-computed L0/L1 caches once those are implemented.
pub struct PalaceHandle {
    pub palace: Palace,
    // TODO(impl): pub vector: Arc<dyn VectorStore>,
    // TODO(impl): pub kg: Arc<KnowledgeGraph>,
    // TODO(impl): pub l0_cache: parking_lot::RwLock<L0Identity>,
    // TODO(impl): pub l1_cache: parking_lot::RwLock<L1Essential>,
}

impl PalaceHandle {
    pub fn new(palace: Palace) -> Self {
        Self { palace }
    }
}

#[derive(Default, Clone)]
pub struct PalaceRegistry {
    palaces: Arc<DashMap<PalaceId, Arc<PalaceHandle>>>,
}

impl PalaceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a new palace handle, replacing any prior entry with the same id.
    pub fn register(&self, handle: PalaceHandle) {
        let id = handle.palace.id.clone();
        self.palaces.insert(id, Arc::new(handle));
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
    use chrono::Utc;
    use std::path::PathBuf;

    fn fixture_palace(id: &str) -> Palace {
        Palace {
            id: PalaceId::new(id),
            name: id.into(),
            description: None,
            created_at: Utc::now(),
            data_dir: PathBuf::from("/tmp").join(id),
        }
    }

    #[test]
    fn register_and_get_roundtrip() {
        let reg = PalaceRegistry::new();
        reg.register(PalaceHandle::new(fixture_palace("alpha")));
        let h = reg.get(&PalaceId::new("alpha")).expect("registered");
        assert_eq!(h.palace.id.as_str(), "alpha");
    }

    #[test]
    fn list_contains_all_registered() {
        let reg = PalaceRegistry::new();
        reg.register(PalaceHandle::new(fixture_palace("a")));
        reg.register(PalaceHandle::new(fixture_palace("b")));
        let ids: Vec<_> = reg.list().into_iter().map(|p| p.0).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"a".to_string()));
        assert!(ids.contains(&"b".to_string()));
    }
}
