//! Why: Smoke-test the public surface of trusty-memory-core so refactors
//! that break the registry or palace types fail loudly.
//! What: Constructs a Palace + storage handles, builds a real PalaceHandle,
//! registers it, and reads it back.
//! Test: `cargo test --test basic_palace_test`.

use tempfile::tempdir;
use trusty_memory_core::store::{kg::KnowledgeGraph, vector::UsearchStore};
use trusty_memory_core::{PalaceHandle, PalaceId, PalaceRegistry};

#[test]
fn registry_register_and_get() {
    let dir = tempdir().expect("tempdir");
    let id = PalaceId::new("integration-test");

    let vs = UsearchStore::new(dir.path().join("idx.usearch"), 384).expect("usearch");
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).expect("kg");
    let handle = PalaceHandle::new(id.clone(), "integration smoke test".to_string(), vs, kg);

    let reg = PalaceRegistry::new();
    reg.register(handle);

    let h = reg
        .get(&PalaceId::new("integration-test"))
        .expect("palace should be registered");
    assert_eq!(h.id.as_str(), "integration-test");
    assert_eq!(reg.len(), 1);
}
