//! Why: Cargo's default test harness only discovers `tests/*.rs`. We keep the
//! integration tests grouped under `tests/integration/` for organization and
//! re-export them from this shim so `cargo test` runs them.
//! What: Includes every file in `tests/integration/` as a module.
//! Test: `cargo test --test integration_tests`.

#[path = "integration/basic_palace_test.rs"]
mod basic_palace_test;

#[path = "integration/mcp_server_test.rs"]
mod mcp_server_test;

#[path = "integration/cli_test.rs"]
mod cli_test;

#[path = "integration/backup_restore_test.rs"]
mod backup_restore_test;
