//! Library surface of the `trusty-memory` binary crate.
//!
//! Why: Integration tests (and potentially other consumers) need direct access
//! to the CLI handler modules — particularly `cli::backup` for round-trip
//! tests that must run against an isolated tempdir-backed data root rather
//! than the user's real `~/Library/Application Support/trusty-memory/`.
//! What: Re-exports the `cli` module tree. Adding `src/lib.rs` automatically
//! gives Cargo a `[lib]` target named `trusty-memory`, which integration tests
//! can import as `use trusty_memory::cli::...;`.
//! Test: `tests/integration/backup_restore_test.rs` exercises this surface.

pub mod cli;
