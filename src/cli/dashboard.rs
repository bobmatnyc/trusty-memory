//! `dashboard` command — opens the HTTP admin panel in the default browser.
//!
//! Why: The daemon binds to a dynamic port (`127.0.0.1:0` by default) and
//! writes its address to `~/.trusty-memory/http_addr`. Users shouldn't have to
//! `cat` that file and paste the URL into a browser — `trusty-memory dashboard`
//! is the obvious one-shot for "open the admin UI now."
//! What: Reads the address file, opens `http://<addr>` in the platform's
//! default browser (`open` on macOS, `xdg-open` on Linux, `cmd /C start` on
//! Windows). If the daemon isn't running, prints a clear hint. If the browser
//! command fails, prints the URL so the user can paste it manually.
//! Test: Covered manually — start `trusty-memory serve` in one shell, run
//! `trusty-memory dashboard` in another, verify the admin panel opens.

use crate::cli::output::OutputConfig;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Handle the `dashboard` subcommand.
///
/// Why: Single entry point invoked from `main.rs`, mirroring the per-handler
/// dispatch pattern used by every other subcommand in this crate.
/// What: Resolves the running daemon's HTTP address from
/// `~/.trusty-memory/http_addr`, formats `http://<addr>`, and tries to open it
/// in the default browser. On any failure mode (missing file, empty file,
/// browser launch failure) prints actionable text rather than erroring out.
/// Test: Exercised manually; unit-tested via `addr_file_path()` and
/// `open_url_command()` helpers below.
pub async fn handle(_out: &OutputConfig) -> Result<()> {
    let path = addr_file_path().context("resolving http_addr file path")?;

    // Fast path: existing http_addr file with a live daemon.
    if let Ok(s) = std::fs::read_to_string(&path) {
        let addr = s.trim().to_string();
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
    }

    // Daemon not running (missing file or stale address). Auto-start it.
    println!("Starting trusty-memory daemon...");
    spawn_daemon().context("spawning trusty-memory serve")?;

    wait_for_daemon_and_open(&path)
}

/// Spawn `trusty-memory serve` as a detached background process.
///
/// Why: The dashboard command should "just work" — users shouldn't need to
/// start the daemon in a separate terminal first. Spawning with null stdio
/// detaches the child so it survives this CLI invocation.
/// What: Resolves the path of the currently running binary via
/// `std::env::current_exe()` (so we always launch the matching version, even
/// outside PATH), spawns `<exe> serve` with stdin/stdout/stderr all wired to
/// `Stdio::null()`, and drops the `Child` handle so we don't wait on it.
/// Test: Covered manually — run `trusty-memory dashboard` with no daemon
/// running and verify the daemon starts and the browser opens within 10 s.
fn spawn_daemon() -> Result<()> {
    let exe = std::env::current_exe().context("resolve current executable")?;
    Command::new(&exe)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn trusty-memory serve")?;
    Ok(())
}

/// Poll for the daemon to become ready, then open the browser.
///
/// Why: Spawning the daemon is async w.r.t. its HTTP listener — the
/// `http_addr` file is written only after axum binds. We need to wait for
/// both the file to appear and the TCP probe to succeed before launching
/// the browser, otherwise the user sees a "page can't be found" error.
/// What: Polls every 200 ms for up to 10 s. On success, prints the admin
/// panel URL, opens the browser, and returns `Ok(())`. On timeout, prints
/// an error hint and returns `Ok(())` (non-fatal).
/// Test: Covered manually — kill any running daemon, run
/// `trusty-memory dashboard`, verify the readiness message and browser open.
fn wait_for_daemon_and_open(path: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let poll_interval = Duration::from_millis(200);

    loop {
        std::thread::sleep(poll_interval);
        if let Ok(s) = std::fs::read_to_string(path) {
            let addr = s.trim().to_string();
            if !addr.is_empty() && is_daemon_alive(&addr) {
                let url = format!("http://{addr}");
                println!("trusty-memory — HTTP admin panel: {url}");
                if open_browser(&url).is_err() {
                    println!("(Could not open browser automatically — paste the URL above)");
                }
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            println!(
                "Daemon did not start within 10 s. Try running `trusty-memory serve` manually."
            );
            return Ok(());
        }
    }
}

/// Resolve the path to the daemon's address-discovery file.
///
/// Why: Centralizes the `~/.trusty-memory/http_addr` location so it stays in
/// sync with where `main.rs::write_http_addr` writes it on `serve` startup.
/// What: Returns `<home>/.trusty-memory/http_addr` or an error if the home
/// directory can't be resolved.
/// Test: Implicitly covered by `handle()`; explicit unit test ensures the
/// path ends with the expected suffix.
fn addr_file_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("home dir not found")?
        .join(".trusty-memory")
        .join("http_addr"))
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
/// Why: `http_addr` is a best-effort file; a stale entry from a previous
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
    fn addr_file_path_ends_with_expected_suffix() {
        let p = addr_file_path().expect("home dir resolvable in test env");
        let s = p.to_string_lossy();
        assert!(
            s.ends_with(".trusty-memory/http_addr") || s.ends_with(".trusty-memory\\http_addr"),
            "unexpected path: {s}"
        );
    }

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
