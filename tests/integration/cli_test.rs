//! CLI integration tests.
//!
//! Why: Verify the binary parses the new command surface and that the always-on
//! commands (`--help`, `status`) exit cleanly.
//! What: Spawns the compiled binary via `CARGO_BIN_EXE_trusty-memory`.
//! Test: `cargo test --test integration_tests`.

#[test]
fn cli_help_exits_zero() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_trusty-memory"))
        .arg("--help")
        .output()
        .expect("failed to run trusty-memory");
    assert!(
        output.status.success(),
        "help exit status: {}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("remember"),
        "stdout missing 'remember': {stdout}"
    );
    assert!(
        stdout.contains("recall"),
        "stdout missing 'recall': {stdout}"
    );
    assert!(
        stdout.contains("palace"),
        "stdout missing 'palace': {stdout}"
    );
    assert!(stdout.contains("kuzu"), "stdout missing 'kuzu': {stdout}");
    assert!(stdout.contains("git"), "stdout missing 'git': {stdout}");
}

#[test]
fn cli_status_exits_zero() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_trusty-memory"))
        .arg("status")
        .output()
        .expect("failed to run trusty-memory");
    assert!(output.status.success(), "status exit: {}", output.status);
}

#[test]
fn cli_palace_inferred_from_env() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_trusty-memory"))
        .env("TRUSTY_PALACE", "test-palace")
        .arg("status")
        .output()
        .expect("failed");
    assert!(output.status.success(), "exit: {}", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("test-palace"),
        "stdout missing 'test-palace': {stdout}"
    );
}

#[test]
fn cli_completions_zsh_outputs() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_trusty-memory"))
        .args(["completions", "zsh"])
        .output()
        .expect("failed");
    assert!(output.status.success(), "exit: {}", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("trusty-memory"),
        "completions output missing binary name"
    );
}
