//! `start` command — spawn the daemon as a detached background process.
//!
//! Why: Users expect a one-shot "start the daemon" command that returns to
//! the shell promptly. `serve` is the foreground/launchd-friendly entry; this
//! wraps it with detached stdio so the calling terminal isn't blocked.
//! What: If the daemon is already running (PID file + alive TCP listener),
//! exits with status 0 and a "already running" message. Otherwise spawns
//! `trusty-memory serve --http <addr>` with null stdio, then polls the
//! discovery file every 200 ms until the daemon reports its bound address
//! or 5 s elapses.
//! Test: integration smoke via `trusty-memory start && trusty-memory status`;
//! `daemon_alive` is the same probe used in `main.rs::ensure_daemon`.

use crate::cli::output::OutputConfig;
use anyhow::{Context, Result};
use std::net::SocketAddr;

/// Handle the `start` subcommand.
///
/// Why: Single entry point invoked from `main.rs`, mirroring the per-handler
/// dispatch used by every other subcommand. Encapsulates the spawn-detached
/// and wait-for-readiness dance so `main.rs` stays tidy.
/// What: Probes `read_daemon_addr` + TCP connect; if alive, prints address
/// and returns. Otherwise spawns the current binary with `serve --http <addr>`,
/// stdio attached to `/dev/null`, then polls for up to 5 s.
/// Test: integration smoke; unit test for the alive-probe lives in `stop.rs`.
pub async fn handle(http: SocketAddr, _out: &OutputConfig) -> Result<()> {
    if let Some(addr) = running_daemon_addr() {
        println!("Daemon already running: http://{addr}");
        return Ok(());
    }

    let exe = std::env::current_exe().context("resolving current executable path")?;
    let child = std::process::Command::new(&exe)
        .arg("serve")
        .arg("--http")
        .arg(http.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("spawning detached `trusty-memory serve` process")?;

    println!("Started trusty-memory daemon (pid {})…", child.id());

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if let Some(addr) = running_daemon_addr() {
            println!("Daemon ready: http://{addr}");
            return Ok(());
        }
    }

    println!("Daemon did not report ready within 5 s; check `trusty-memory status`.");
    Ok(())
}

/// Returns the daemon's bound address if it is currently accepting TCP
/// connections within 300 ms.
///
/// Why: A stale `http_addr` file from a crashed daemon would otherwise make
/// `start` think the service is live. A short TCP probe distinguishes a
/// healthy daemon from leftover discovery state.
/// What: Reads `trusty_common::read_daemon_addr`, parses as `SocketAddr`, and
/// attempts a 300 ms `TcpStream::connect_timeout`. Returns `Some(addr)` only
/// on a successful connect.
/// Test: Covered indirectly by the start/stop round-trip integration smoke.
fn running_daemon_addr() -> Option<String> {
    let addr = trusty_common::read_daemon_addr("trusty-memory")
        .ok()
        .flatten()?;
    if addr.is_empty() {
        return None;
    }
    let sa: SocketAddr = addr.parse().ok()?;
    std::net::TcpStream::connect_timeout(&sa, std::time::Duration::from_millis(300)).ok()?;
    Some(addr)
}
