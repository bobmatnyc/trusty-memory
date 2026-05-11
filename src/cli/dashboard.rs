//! `dashboard` command — opens the HTTP admin panel in the default browser.
//!
//! Why: The daemon binds to a dynamic port (`127.0.0.1:0` by default) and
//! writes its address to the shared trusty-* discovery file (via
//! `trusty_common::write_daemon_addr`). Users shouldn't have to `cat` that
//! file and paste the URL into a browser — `trusty-memory dashboard` is the
//! obvious one-shot for "open the admin UI now."
//! What: Reads the address file, opens `http://<addr>` in the platform's
//! default browser (`open` on macOS, `xdg-open` on Linux, `cmd /C start` on
//! Windows). If the daemon isn't running, prints a clear hint. If the browser
//! command fails, prints the URL so the user can paste it manually.
//! Test: Covered manually — run `trusty-memory dashboard`, verify the admin
//! panel opens.

use crate::cli::output::OutputConfig;
use anyhow::{Context, Result};
use std::process::Command;

/// Handle the `dashboard` subcommand.
///
/// Why: Single entry point invoked from `main.rs`, mirroring the per-handler
/// dispatch pattern used by every other subcommand in this crate. Daemon
/// auto-start is handled globally by `ensure_daemon_running` in `main.rs`, so
/// this handler only needs to resolve the address and open the browser.
/// What: Reads the daemon's discovery file via the shared trusty-* helper,
/// formats `http://<addr>`, probes it, and opens the browser. If the daemon
/// is not reachable (true edge case since `ensure_daemon_running` already
/// ran), prints a hint.
/// Test: Exercised manually; `open_url_command()` is unit-tested below.
pub async fn handle(_out: &OutputConfig) -> Result<()> {
    // Use the shared trusty-* discovery helper so the path matches what the
    // daemon writes in `main.rs::serve`. `Ok(None)` and `Err` both mean
    // "no usable address" — we treat them identically and fall through to
    // the restart hint.
    let addr = trusty_common::read_daemon_addr("trusty-memory")
        .ok()
        .flatten()
        .unwrap_or_default();

    if !addr.is_empty() && is_daemon_alive(&addr) {
        let url = format!("http://{addr}");
        match open_browser(&url) {
            Ok(()) => println!("Opening dashboard: {url}"),
            Err(_) => {
                println!("Dashboard: {url}");
                println!("(Could not open browser automatically — paste the URL above)");
            }
        }
        return Ok(());
    }

    println!("Daemon not reachable. Run `trusty-memory serve` to start the daemon.");
    Ok(())
}

/// Spawn the platform-appropriate browser-open command for `url`.
///
/// Why: Avoids pulling in a heavier `open`/`webbrowser` crate dependency for
/// a one-off helper — the three commands needed are tiny and stable.
/// What: Selects `open` (macOS), `xdg-open` (Linux), or `cmd /C start`
/// (Windows) via `#[cfg(target_os = ...)]`, executes it, and returns an
/// `Err` if the command can't be launched or exits non-zero.
/// Test: Exercised manually per-platform; a future test could mock the
/// `Command` invocation via a trait, but ROI is low for a single helper.
fn open_browser(url: &str) -> Result<()> {
    let status = open_url_command(url)
        .status()
        .with_context(|| format!("spawning browser-open command for {url}"))?;
    if !status.success() {
        anyhow::bail!("browser-open command exited with status {status}");
    }
    Ok(())
}

/// Returns true if a TCP connection to `addr` succeeds within 500 ms.
///
/// Why: the discovery file is best-effort; a stale entry from a previous
/// daemon run would open a "page can't be found" error in the browser.
/// A quick TCP probe lets us detect and report this gracefully.
/// What: Parses `addr` as a `SocketAddr`, calls `TcpStream::connect_timeout`
/// with a 500 ms budget, returns true on success.
/// Test: Covered indirectly — if the daemon isn't running, `handle()` prints
/// the restart hint instead of opening a dead URL.
fn is_daemon_alive(addr: &str) -> bool {
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;
    addr.parse::<SocketAddr>()
        .ok()
        .and_then(|sa| TcpStream::connect_timeout(&sa, Duration::from_millis(500)).ok())
        .is_some()
}

/// Build (but don't execute) the platform-specific browser-open `Command`.
///
/// Why: Separated from `open_browser` so the command construction can be
/// unit-tested without actually spawning a browser.
/// What: Returns a `Command` configured with the correct program and args
/// for the current OS. Falls back to `xdg-open` on unknown unix-likes.
/// Test: `open_url_command_uses_expected_program` below.
fn open_url_command(url: &str) -> Command {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("open");
        cmd.arg(url);
        cmd
    }
    #[cfg(target_os = "windows")]
    {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "start", "", url]);
        cmd
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let mut cmd = Command::new("xdg-open");
        cmd.arg(url);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_url_command_uses_expected_program() {
        let cmd = open_url_command("http://127.0.0.1:1234");
        let program = cmd.get_program().to_string_lossy().to_string();
        #[cfg(target_os = "macos")]
        assert_eq!(program, "open");
        #[cfg(target_os = "windows")]
        assert_eq!(program, "cmd");
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert_eq!(program, "xdg-open");
    }
}
