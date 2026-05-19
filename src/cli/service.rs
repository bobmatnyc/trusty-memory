//! `service` subcommand — manage the trusty-memory background service via
//! macOS launchd.
//!
//! Why: Users want `trusty-memory serve` to start at login and be restarted on
//! crash without dealing with daemon wrappers, PID files, or shell scripts.
//! launchd already does process supervision correctly on macOS, so the right
//! integration is a `LaunchAgent` plist that points directly at the binary —
//! no forking, no double-fork, no PID file.
//! What: Implements four sub-subcommands — `install`, `uninstall`, `status`,
//! `logs` — that write `~/Library/LaunchAgents/com.trusty.trusty-memory.plist`
//! and shell out to `launchctl` for lifecycle operations. All launchd-specific
//! code is `#[cfg(target_os = "macos")]`-gated; other platforms get a clear
//! error pointing at systemd as the Linux equivalent.
//! Test: Unit tests cover the plist rendering and the log-resolution helper.
//! Install/uninstall require macOS + a real user session, so they are exercised
//! manually.

use crate::cli::ServiceCommands;
use anyhow::Result;
#[cfg(target_os = "macos")]
use std::path::Path;

/// Reverse-DNS label used for the LaunchAgent and `launchctl` identifiers.
#[cfg(target_os = "macos")]
const SERVICE_LABEL: &str = "com.trusty.trusty-memory";

/// Top-level dispatcher for `trusty-memory service <sub>`.
///
/// Why: Keeps `main.rs` free of platform-specific branching — one call site,
/// all cases handled here.
/// What: Matches on `ServiceCommands` and delegates to the per-action helper.
/// Test: Exercised by the integration `--help` walk; behavioural coverage is
/// per-helper.
pub fn handle(cmd: ServiceCommands) -> Result<()> {
    match cmd {
        ServiceCommands::Install => install(),
        ServiceCommands::Uninstall => uninstall(),
        ServiceCommands::Status => status(),
        ServiceCommands::Logs => logs(),
    }
}

// ─── macOS implementation ───────────────────────────────────────────────────

/// Build the [`LaunchdConfig`] describing the trusty-memory LaunchAgent.
///
/// Why: `install` needs a fully-populated config to render the plist and
/// bootstrap the service; isolating its construction keeps the install path
/// declarative and lets the plist content be unit-tested without filesystem
/// or `launchctl` side effects.
/// What: Resolves the current binary, home-relative log paths, and the
/// FASTEMBED cache directory, returning a `LaunchdConfig` with `KeepAlive`
/// always-on and a 10s respawn throttle.
/// Test: `launchd_config_renders_expected_plist` renders this config and
/// asserts the binary, args, env vars, and log paths are present.
#[cfg(target_os = "macos")]
fn launchd_config() -> Result<trusty_common::launchd::LaunchdConfig> {
    use anyhow::Context;
    use trusty_common::launchd::{KeepAlive, LaunchdConfig};

    let binary = std::env::current_exe().context("resolving current binary path")?;
    let home = dirs::home_dir().context("could not resolve home directory")?;
    let log_dir = home.join("Library").join("Logs").join("trusty-memory");

    Ok(LaunchdConfig {
        label: SERVICE_LABEL.to_string(),
        program: binary,
        program_args: vec!["serve".to_string(), "--http".to_string()],
        stdout_path: log_dir.join("trusty-memory.log"),
        stderr_path: log_dir.join("trusty-memory.error.log"),
        env_vars: vec![
            (
                "FASTEMBED_CACHE_PATH".to_string(),
                format!("{}/.cache/fastembed", home.to_string_lossy()),
            ),
            ("RUST_LOG".to_string(), "info".to_string()),
        ],
        keep_alive: KeepAlive::Always,
        throttle_interval: Some(10),
        working_directory: None,
    })
}

#[cfg(target_os = "macos")]
fn install() -> Result<()> {
    use anyhow::Context;
    use std::fs;

    let cfg = launchd_config()?;
    let plist_path = cfg.plist_path()?;
    let log_dir = cfg
        .stdout_path
        .parent()
        .map(Path::to_path_buf)
        .context("resolving log directory")?;

    // Idempotent: if the plist already exists, don't clobber whatever the user
    // (or a previous install) configured.
    if plist_path.exists() {
        println!(
            "trusty-memory service already installed at {}",
            plist_path.display()
        );
        println!("Run `trusty-memory service uninstall` first if you want to reinstall.");
        return Ok(());
    }

    fs::create_dir_all(&log_dir)
        .with_context(|| format!("creating log directory {}", log_dir.display()))?;

    // Render + write the plist into ~/Library/LaunchAgents.
    cfg.install()?;

    // Load immediately via `launchctl bootstrap gui/$UID <plist>`. A non-zero
    // exit because the service is already loaded is benign — the plist is now
    // on disk regardless.
    if let Err(e) = cfg.bootstrap() {
        let msg = e.to_string();
        if !msg.contains("already loaded") && !msg.contains("service already") {
            return Err(e);
        }
    }

    println!("Installed trusty-memory service:");
    println!("  plist:   {}", plist_path.display());
    println!("  stdout:  {}", cfg.stdout_path.display());
    println!("  stderr:  {}", cfg.stderr_path.display());
    println!("  http:    dynamic port — discover via `trusty-memory status`");
    println!("           or `trusty-memory service status`");
    println!();
    println!("Run `trusty-memory service status` to verify it is running.");
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall() -> Result<()> {
    use anyhow::Context;
    use std::fs;

    let cfg = launchd_config()?;
    let plist_path = cfg.plist_path()?;

    // Best-effort bootout — a failure because the service was never loaded is
    // benign, so we only log unexpected errors rather than aborting.
    if let Err(e) = cfg.bootout() {
        let msg = e.to_string();
        if !msg.contains("No such process") && !msg.contains("not loaded") {
            tracing::debug!("launchctl bootout failed: {msg}");
        }
    }

    if plist_path.exists() {
        fs::remove_file(&plist_path)
            .with_context(|| format!("removing {}", plist_path.display()))?;
        println!(
            "Uninstalled trusty-memory service ({} removed).",
            plist_path.display()
        );
    } else {
        println!("trusty-memory service was not installed (no plist found).");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn status() -> Result<()> {
    use anyhow::Context;

    let output = std::process::Command::new("launchctl")
        .args(["list", SERVICE_LABEL])
        .output()
        .context("invoking launchctl list")?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        print!("{stdout}");
        if !stdout.ends_with('\n') {
            println!();
        }
        // Surface the dynamic HTTP address if the running daemon has written
        // one to its discovery file. Best-effort — silent if absent.
        if let Some(addr) = read_http_addr() {
            println!("HTTP: http://{addr}");
        }
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Could not find service") || stderr.contains("No such") {
            println!("trusty-memory service is not installed.");
            println!("Run `trusty-memory service install` to set it up.");
        } else {
            anyhow::bail!(
                "launchctl list failed (exit {}): {}",
                output.status,
                stderr.trim()
            );
        }
    }
    Ok(())
}

/// Read the daemon's discovery file via the shared trusty-* helper.
///
/// Why: The HTTP port is dynamic, so `status` can't print a static address;
/// it must read whatever the running daemon recorded on startup. Delegating
/// to `trusty_common::read_daemon_addr` keeps the discovery path in sync
/// with the writer in `main.rs::serve` and with sibling trusty-* daemons.
/// What: Returns the trimmed address (e.g. `"127.0.0.1:54321"`), or `None`
/// if the file is absent, empty, or unreadable.
/// Test: Manual — start the daemon, run `trusty-memory service status`,
/// confirm the printed address matches the daemon-written discovery file.
#[cfg(target_os = "macos")]
fn read_http_addr() -> Option<String> {
    trusty_common::read_daemon_addr("trusty-memory")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

#[cfg(target_os = "macos")]
fn logs() -> Result<()> {
    use anyhow::Context;
    use std::fs;
    use std::io::{BufRead, BufReader};

    let home = dirs::home_dir().context("could not resolve home directory")?;
    let log_path = home
        .join("Library")
        .join("Logs")
        .join("trusty-memory")
        .join("trusty-memory.log");

    if !log_path.exists() {
        println!("No logs yet — start the service first with `trusty-memory service install`.");
        return Ok(());
    }

    let file =
        fs::File::open(&log_path).with_context(|| format!("opening {}", log_path.display()))?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();
    let start = lines.len().saturating_sub(50);
    for line in &lines[start..] {
        println!("{line}");
    }

    println!();
    println!(
        "(showing last {} of {} lines — tail -f {} for live streaming)",
        lines.len() - start,
        lines.len(),
        log_path.display()
    );
    Ok(())
}

// ─── Non-macOS stubs ────────────────────────────────────────────────────────

#[cfg(not(target_os = "macos"))]
fn install() -> Result<()> {
    unsupported()
}

#[cfg(not(target_os = "macos"))]
fn uninstall() -> Result<()> {
    unsupported()
}

#[cfg(not(target_os = "macos"))]
fn status() -> Result<()> {
    unsupported()
}

#[cfg(not(target_os = "macos"))]
fn logs() -> Result<()> {
    unsupported()
}

#[cfg(not(target_os = "macos"))]
fn unsupported() -> Result<()> {
    anyhow::bail!(
        "Service management via launchd is only supported on macOS. \
         On Linux, use systemd user services (`systemctl --user`)."
    )
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn launchd_config_renders_expected_plist() {
        // launchd_config() resolves the running test binary via current_exe()
        // and HOME-relative log paths — exact values vary by host, so we assert
        // on the structural keys the install path depends on.
        let cfg = launchd_config().expect("build launchd config");
        let plist = cfg.render_plist();

        // Service label and program arguments.
        assert!(plist.contains(SERVICE_LABEL));
        assert!(plist.contains("<string>serve</string>"));
        // --http keeps the daemon alive when stdin is /dev/null (launchd default).
        assert!(plist.contains("<string>--http</string>"));
        // RUST_LOG=info ensures startup, dream, and FASTEMBED lines are captured.
        assert!(plist.contains("<key>RUST_LOG</key>"));
        assert!(plist.contains("<string>info</string>"));
        // FASTEMBED_CACHE_PATH prevents read-only filesystem errors on SIP paths.
        assert!(plist.contains("<key>FASTEMBED_CACHE_PATH</key>"));
        assert!(plist.contains(".cache/fastembed"));
        // KeepAlive=Always restarts the daemon on crash and runs it at load.
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<true/>"));
        // 10s throttle prevents crash-loop hammering.
        assert!(plist.contains("<key>ThrottleInterval</key>"));
        // Separate stdout/stderr log files under ~/Library/Logs/trusty-memory/.
        assert!(plist.contains("trusty-memory.log"));
        assert!(plist.contains("trusty-memory.error.log"));
    }

    #[test]
    fn launchd_config_renders_well_formed_xml() {
        let plist = launchd_config()
            .expect("build launchd config")
            .render_plist();
        assert!(plist.starts_with("<?xml"));
        assert!(plist.trim_end().ends_with("</plist>"));
    }
}
