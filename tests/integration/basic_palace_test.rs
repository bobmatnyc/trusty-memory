//! Why: Smoke-test the public surface of trusty-memory-core so refactors
//! that break the registry or palace types fail loudly.
//! What: Constructs a Palace, registers it, and reads it back.
//! Test: `cargo test --test basic_palace_test`.

use chrono::Utc;
use std::path::PathBuf;
use trusty_memory_core::{Palace, PalaceId, PalaceRegistry};
use trusty_memory_core::registry::PalaceHandle;

#[test]
fn registry_register_and_get() {
    let palace = Palace {
        id: PalaceId::new("integration-test"),
        name: "integration-test".into(),
        description: Some("smoke test".into()),
        created_at: Utc::now(),
        data_dir: PathBuf::from("/tmp/trusty-memory/integration-test"),
    };

    let reg = PalaceRegistry::new();
    reg.register(PalaceHandle::new(palace));

    let h = reg
        .get(&PalaceId::new("integration-test"))
        .expect("palace should be registered");
    assert_eq!(h.palace.id.as_str(), "integration-test");
    assert_eq!(reg.len(), 1);
}
