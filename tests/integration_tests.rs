//! Why: Cargo's default test harness only discovers `tests/*.rs`. We keep the
//! integration tests grouped under `tests/integration/` for organization and
//! re-export them from this shim so `cargo test` runs them.
//! What: Includes every file in `tests/integration/` as a module.
//! Test: `cargo test --test integration_tests`.

#[path = "integration/basic_palace_test.rs"]
mod basic_palace_test;
