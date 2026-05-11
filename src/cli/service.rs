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
use anyhow::{Context, Result};

/// Reverse-DNS label used for the LaunchAgent and `launchctl` identifiers.
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

#[cfg(target_os = "macos")]
fn install() -> Result<()> {
    use std::fs;

    let binary = std::env::current_exe().context("resolving current binary path")?;
    let home = dirs::home_dir().context("could not resolve home directory")?;

    let plist_path = home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{SERVICE_LABEL}.plist"));
    let log_dir = home.join(".trusty-memory").join("logs");
    let log_path = log_dir.join("daemon.log");

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
    if let Some(parent) = plist_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating LaunchAgents directory {}", parent.display()))?;
    }

    let plist = render_plist(&binary.to_string_lossy(), &log_path.to_string_lossy());
    fs::write(&plist_path, plist).with_context(|| format!("writing {}", plist_path.display()))?;

    // Load immediately via `launchctl bootstrap gui/$UID <plist>`.
    let uid = nix_uid();
    let domain = format!("gui/{uid}");
    let output = std::process::Command::new("launchctl")
        .args(["bootstrap", &domain])
        .arg(&plist_path)
        .output()
        .context("invoking launchctl bootstrap")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // bootstrap returns non-zero if the service is already loaded; treat
        // that as success since the plist is now on disk.
        if !stderr.contains("already loaded") && !stderr.contains("service already") {
            anyhow::bail!(
                "launchctl bootstrap failed (exit {}): {}",
                output.status,
                stderr.trim()
            );
        }
    }

    println!("Installed trusty-memory service:");
    println!("  plist:   {}", plist_path.display());
    println!("  logs:    {}", log_path.display());
    println!("  http:    dynamic port — discover via `trusty-memory status`");
    println!("           or `trusty-memory service status`");
    println!();
    println!("Run `trusty-memory service status` to verify it is running.");
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall() -> Result<()> {
    use std::fs;

    let home = dirs::home_dir().context("could not resolve home directory")?;
    let plist_path = home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{SERVICE_LABEL}.plist"));

    let uid = nix_uid();
    let domain_target = format!("gui/{uid}/{SERVICE_LABEL}");

    // Best-effort bootout — succeeds whether or not the service is currently
    // loaded.
    let output = std::process::Command::new("launchctl")
        .args(["bootout", &domain_target])
        .output()
        .context("invoking launchctl bootout")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("No such process") && !stderr.contains("not loaded") {
            tracing::debug!(
                "launchctl bootout returned {}: {}",
                output.status,
                stderr.trim()
            );
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
fn read_http_addr() -> Option<String> {
    trusty_common::read_daemon_addr("trusty-memory")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

#[cfg(target_os = "macos")]
fn logs() -> Result<()> {
    use std::fs;
    use std::io::{BufRead, BufReader};

    let home = dirs::home_dir().context("could not resolve home directory")?;
    let log_path = home.join(".trusty-memory").join("logs").join("daemon.log");

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

/// Render the LaunchAgent plist body.
///
/// Why: Isolated for unit testing — keeps the install path easy to verify
/// without writing to the filesystem.
/// What: Returns the full plist XML with the binary path and log path
/// substituted.
/// Test: `render_plist_contains_paths` below.
#[cfg(target_os = "macos")]
fn render_plist(binary: &str, log_path: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{SERVICE_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{binary}</string>
    <string>serve</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{log_path}</string>
  <key>StandardErrorPath</key>
  <string>{log_path}</string>
  <key>ThrottleInterval</key>
  <integer>10</integer>
</dict>
</plist>
"#
    )
}

/// Resolve the real user id for `gui/<uid>` launchctl domain selectors.
///
/// Why: launchctl 2.x requires a domain target like `gui/501` rather than the
/// legacy `launchctl load`. Using `id -u` keeps us off `libc` while still
/// being correct on every macOS host.
/// What: Shells out to `id -u`, parsing the integer. Falls back to 0 only if
/// the call fails (which would already fail the surrounding command).
/// Test: Not unit-tested — exercised end-to-end by install/uninstall.
#[cfg(target_os = "macos")]
fn nix_uid() -> u32 {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0)
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
    fn render_plist_contains_paths() {
        let plist = render_plist(
            "/usr/local/bin/trusty-memory",
            "/Users/u/.trusty-memory/logs/daemon.log",
        );
        assert!(plist.contains("<string>/usr/local/bin/trusty-memory</string>"));
        assert!(plist.contains("<string>serve</string>"));
        assert!(plist.contains("/Users/u/.trusty-memory/logs/daemon.log"));
        assert!(plist.contains(SERVICE_LABEL));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
    }

    #[test]
    fn render_plist_is_well_formed_xml() {
        let plist = render_plist("/bin/trusty-memory", "/tmp/log");
        assert!(plist.starts_with("<?xml"));
        assert!(plist.trim_end().ends_with("</plist>"));
    }
}
