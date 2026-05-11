//! `stop` command — terminate the running background daemon.
//!
//! Why: `trusty-memory start` spawns the daemon detached; without a matching
//! `stop` users have to hunt the PID with `ps` and send SIGTERM by hand. This
//! handler reads the PID file written by `serve` and delivers SIGTERM, then
//! polls the discovery file until shutdown completes.
//! What: PID file lives at `{resolve_data_dir("trusty-memory")}/trusty-memory.pid`
//! next to `http_addr`. We read it, send `kill -TERM`, and wait up to 5 s for
//! the addr file to disappear (the daemon clears it on graceful shutdown).
//! Test: integration smoke via `trusty-memory start && trusty-memory stop`;
//! `pid_file_path` is implicitly covered by the round-trip with `serve`.

use crate::cli::output::OutputConfig;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Resolve the PID file path inside the shared trusty-memory data dir.
///
/// Why: Centralizes the path so `serve` (writer) and `stop` (reader) cannot
/// drift apart. Sits next to `http_addr` produced by `trusty_common::write_daemon_addr`
/// so both lifecycle artifacts live in the same directory.
/// What: `{resolve_data_dir("trusty-memory")}/trusty-memory.pid`.
/// Test: Covered by the start/stop round-trip integration smoke.
pub fn pid_file_path() -> Result<PathBuf> {
    let dir = trusty_common::resolve_data_dir("trusty-memory")
        .context("resolving trusty-memory data dir for pid file")?;
    Ok(dir.join("trusty-memory.pid"))
}

/// Write the current process's PID into the PID file.
///
/// Why: `stop` needs a stable place to find the daemon's PID across
/// invocations. Called from `serve` after the HTTP listener binds.
/// What: Writes `pid` (decimal, no newline) to `pid_file_path()`. Creates
/// the parent directory if missing (delegated to `resolve_data_dir`).
/// Test: Round-trip via `read_pid_file` in unit test below.
pub fn write_pid_file(pid: u32) -> Result<()> {
    let path = pid_file_path()?;
    std::fs::write(&path, pid.to_string())
        .with_context(|| format!("writing pid file at {}", path.display()))?;
    Ok(())
}

/// Read the PID from the PID file, if it exists and parses.
///
/// Why: `stop` and `doctor` both need to know whether a daemon claims to be
/// running. Returns `None` for missing or unparseable files rather than
/// erroring so callers can treat absence as "no daemon."
/// What: Reads `pid_file_path()`, trims whitespace, parses as `u32`.
/// Test: Round-trip via `write_pid_file` in unit test below.
pub fn read_pid_file() -> Option<u32> {
    let path = pid_file_path().ok()?;
    let contents = std::fs::read_to_string(&path).ok()?;
    contents.trim().parse::<u32>().ok()
}

/// Remove the PID file. Best-effort — missing file is not an error.
///
/// Why: Called on daemon shutdown so a subsequent `stop` doesn't try to
/// signal a dead PID. Also useful when `doctor` repairs stale state.
/// What: `std::fs::remove_file` on `pid_file_path()`; ignores `NotFound`.
/// Test: Implicit via the shutdown path in `main.rs`.
pub fn remove_pid_file() -> Result<()> {
    let path = pid_file_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing pid file at {}", path.display())),
    }
}

/// Handle the `stop` subcommand.
///
/// Why: Single entry point invoked from `main.rs`. Mirrors the per-handler
/// dispatch pattern used by every other subcommand. Reads the PID file,
/// signals SIGTERM, and polls for graceful shutdown.
/// What: Reads `pid_file_path()`. If absent → prints a friendly "no daemon"
/// message and returns. Otherwise sends `kill -TERM <pid>`, polls
/// `read_daemon_addr` up to 5 s, then prints the outcome.
/// Test: integration smoke; unit tests cover pid-file round-trip.
pub async fn handle(_out: &OutputConfig) -> Result<()> {
    let Some(pid) = read_pid_file() else {
        println!("No daemon running (no PID file).");
        return Ok(());
    };

    println!("Stopping daemon (PID {pid})…");
    let status = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status();

    match status {
        Ok(s) if s.success() => {
            // Poll up to 5 s for the daemon to clear its addr file.
            for _ in 0..50 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let cleared = trusty_common::read_daemon_addr("trusty-memory")
                    .ok()
                    .flatten()
                    .map(|s| s.is_empty())
                    .unwrap_or(true);
                if cleared {
                    println!("Daemon stopped.");
                    // Best-effort cleanup in case the daemon was killed before
                    // it could clear its own pid file.
                    let _ = remove_pid_file();
                    return Ok(());
                }
            }
            println!("Daemon may still be shutting down (timeout after 5 s).");
        }
        _ => {
            println!("Failed to send SIGTERM (process may already be gone).");
            // Stale pid file — clean it up so the next `stop` is honest.
            let _ = remove_pid_file();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_file_round_trips() {
        // Resolves to the user's real data dir; just exercise the path helpers
        // and round-trip with a sentinel value. We restore prior state so the
        // running daemon (if any) isn't disturbed.
        let prior = read_pid_file();
        write_pid_file(424242).unwrap();
        assert_eq!(read_pid_file(), Some(424242));
        match prior {
            Some(p) => write_pid_file(p).unwrap(),
            None => remove_pid_file().unwrap(),
        }
    }
}
