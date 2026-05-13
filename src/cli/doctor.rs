//! `doctor` command — diagnostic checks for daemon, configuration, and data.
//!
//! Why: When something stops working the user needs a quick, structured
//! readout of "what is healthy and what is not." Mirrors `trusty-search doctor`
//! so the trusty-* CLI surface stays consistent.
//! What: Runs four independent checks (daemon liveness, data dir, palace
//! registry, discovery files) and prints ✓ / ⚠ / ✗ for each. Exits 1 if any
//! check is an error; exits 0 on warnings-only or all-good.
//! Test: `cargo test -p trusty-memory doctor::` covers the formatting helpers;
//! the end-to-end probe is exercised manually via `trusty-memory doctor`.

use crate::cli::daemon_probe::{self, AddrSource, HTTP_PORT_ENV};
use crate::cli::output::OutputConfig;
use crate::cli::palace;
use crate::cli::stop;
use anyhow::Result;

/// Outcome of a single diagnostic check.
///
/// Why: Distinguishes "all good" from "won't crash but worth noting" from
/// "the user needs to do something." Keeping it an enum lets `handle()`
/// count the failure modes after the fact for the summary line.
/// What: Three variants — Ok, Warn(msg), Error(msg). Each carries a human
/// label so the print order is `label: status` and the summary line can
/// quote the failures.
/// Test: Exercised by the printing test in this module.
#[derive(Debug, Clone)]
enum CheckResult {
    Ok(String),
    Warn(String, String),
    Error(String, String),
}

impl CheckResult {
    fn print(&self) {
        match self {
            CheckResult::Ok(label) => println!("  ✓ {label}"),
            CheckResult::Warn(label, msg) => println!("  ⚠ {label}: {msg}"),
            CheckResult::Error(label, msg) => println!("  ✗ {label}: {msg}"),
        }
    }

    fn is_error(&self) -> bool {
        matches!(self, CheckResult::Error(..))
    }

    fn is_warn(&self) -> bool {
        matches!(self, CheckResult::Warn(..))
    }
}

/// Handle the `doctor` subcommand.
///
/// Why: Single entry point invoked from `main.rs`. Runs all checks, prints
/// per-check results, then a summary line. Exits 1 if any check failed so
/// the command is usable in CI / pre-flight scripts.
/// What: Builds the check list synchronously where possible (path checks),
/// then probes the daemon (which uses async TCP). Final summary mirrors the
/// `trusty-search doctor` output style.
/// Test: Manual via `trusty-memory doctor`; unit tests cover the helpers.
pub async fn handle(_out: &OutputConfig) -> Result<()> {
    println!("\ntrusty-memory doctor\n");
    println!("Checking configuration...\n");

    let mut checks: Vec<CheckResult> = Vec::new();
    checks.push(check_data_dir());
    checks.push(check_palace_registry().await);
    checks.push(check_daemon_running());
    checks.push(check_discovery_files());

    for c in &checks {
        c.print();
    }

    let errors = checks.iter().filter(|c| c.is_error()).count();
    let warnings = checks.iter().filter(|c| c.is_warn()).count();

    println!();
    if errors == 0 && warnings == 0 {
        println!("Everything looks good!");
    } else {
        println!(
            "Issues found: {warnings} warning{}, {errors} error{}",
            if warnings == 1 { "" } else { "s" },
            if errors == 1 { "" } else { "s" },
        );
    }

    if errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Check that the palace data root exists and is accessible.
///
/// Why: Most CLI commands fail opaquely when the data dir is missing; surface
/// it here as a clear ✗ instead.
/// What: Calls `palace::data_root()` and `try_exists` on the result.
/// Test: Covered indirectly by the integration smoke; unit-testing requires
/// overriding `dirs::data_dir`, which is out of scope.
fn check_data_dir() -> CheckResult {
    let label = "data dir".to_string();
    let root = match palace::data_root() {
        Ok(r) => r,
        Err(e) => return CheckResult::Error(label, format!("could not resolve: {e:#}")),
    };
    match std::fs::metadata(&root) {
        Ok(m) if m.is_dir() => CheckResult::Ok(format!("data dir ({})", root.display())),
        Ok(_) => CheckResult::Error(label, format!("{} is not a directory", root.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => CheckResult::Warn(
            label,
            format!(
                "{} does not exist yet (run `trusty-memory palace new <name>` to create)",
                root.display()
            ),
        ),
        Err(e) => CheckResult::Error(label, format!("{} unreadable: {e}", root.display())),
    }
}

/// Check the palace registry: at least one palace present.
///
/// Why: A fresh install has no palaces; this is a warning (not an error) so
/// `doctor` exits 0 but the user knows to run `palace new`.
/// What: Calls `PalaceRegistry::list_palaces`. Warns on zero, OK otherwise.
/// Test: Covered indirectly by the integration smoke.
async fn check_palace_registry() -> CheckResult {
    let label = "palaces".to_string();
    let root = match palace::data_root() {
        Ok(r) => r,
        Err(e) => return CheckResult::Error(label, format!("could not resolve data root: {e:#}")),
    };
    let palaces = tokio::task::spawn_blocking(move || {
        trusty_memory_core::PalaceRegistry::list_palaces(&root)
    })
    .await;
    match palaces {
        Ok(Ok(list)) if list.is_empty() => CheckResult::Warn(
            label,
            "no palaces yet — create one with `trusty-memory palace new <name>`".to_string(),
        ),
        Ok(Ok(list)) => CheckResult::Ok(format!("palaces ({} registered)", list.len())),
        Ok(Err(e)) => CheckResult::Error(label, format!("registry read failed: {e:#}")),
        Err(e) => CheckResult::Error(label, format!("join error: {e}")),
    }
}

/// Check that the background daemon is running and reachable.
///
/// Why: The MCP HTTP server, web admin panel, and dream cycle all depend on
/// the daemon. A dead daemon is the most common cause of "trusty-memory feels
/// broken" reports. Issue #50 — historically this check probed only the
/// discovery file, which produced false "not running" reports when the file
/// was missing (e.g. launchd-managed daemons with a different `HOME`).
/// What: Delegates to `daemon_probe::probe_daemon`, which tries the
/// `TRUSTY_MEMORY_HTTP_PORT` env var, then the discovery file, then a
/// candidate port range (3031..=3050) before declaring the daemon dead.
/// Reports the actual bound address and the discovery source so users can
/// see when the addr file is out of sync.
/// Test: Covered by the start/stop round-trip integration smoke; daemon_probe
/// has its own unit tests.
fn check_daemon_running() -> CheckResult {
    let label = "daemon".to_string();
    match daemon_probe::probe_daemon() {
        Some(found) => {
            let suffix = match found.source {
                AddrSource::EnvVar => format!(" [via ${HTTP_PORT_ENV}]"),
                AddrSource::DiscoveryFile => String::new(),
                AddrSource::CandidatePort => {
                    " [discovery file missing/stale — found via port scan]".to_string()
                }
            };
            CheckResult::Ok(format!(
                "daemon (running at http://{}){suffix}",
                found.addr
            ))
        }
        None => CheckResult::Warn(
            label,
            format!(
                "not running on any known address (checked ${HTTP_PORT_ENV}, discovery file, and ports 3031..=3050) — start it with `trusty-memory start`"
            ),
        ),
    }
}

/// Check the discovery files (addr + PID) for staleness.
///
/// Why: A stale PID file pointing at a dead process is a common failure mode
/// after a crash; surfacing it lets the user clean up before retrying.
/// What: If the PID file exists, check that the daemon addr is also live
/// (since both should rise and fall together). Mismatch → warning.
/// Test: Covered indirectly by integration smoke.
fn check_discovery_files() -> CheckResult {
    let label = "discovery files".to_string();
    let pid = stop::read_pid_file();
    let addr = trusty_common::read_daemon_addr("trusty-memory")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty());
    // If the daemon is reachable on a candidate port but neither PID nor
    // addr file is present, surface that as a warning so the user knows
    // `stop` won't work and `start` could spawn a duplicate.
    let probe = daemon_probe::probe_daemon();
    match (pid, addr, probe) {
        (None, None, None) => {
            CheckResult::Ok("discovery files (none; daemon not running)".to_string())
        }
        (Some(_), Some(_), _) => {
            CheckResult::Ok("discovery files (PID + addr present)".to_string())
        }
        (Some(pid), None, _) => CheckResult::Warn(
            label,
            format!("stale PID file (pid {pid}) without addr file — daemon likely crashed"),
        ),
        (None, Some(addr), _) => CheckResult::Warn(
            label,
            format!("addr file ({addr}) without PID file — `stop` will not find the daemon"),
        ),
        (None, None, Some(found)) => CheckResult::Warn(
            label,
            format!(
                "daemon answering at {} but no PID/addr file — likely launched outside this CLI; `stop` will not find it",
                found.addr
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_result_flags_are_mutually_exclusive() {
        let ok = CheckResult::Ok("x".to_string());
        let warn = CheckResult::Warn("x".to_string(), "y".to_string());
        let err = CheckResult::Error("x".to_string(), "y".to_string());
        assert!(!ok.is_error() && !ok.is_warn());
        assert!(warn.is_warn() && !warn.is_error());
        assert!(err.is_error() && !err.is_warn());
    }
}
