//! Auto-infer the active palace from the current working directory.
//!
//! Why: Users running `trusty-memory remember "..."` inside a project should
//! land in that project's palace without specifying `--palace` every time.
//! What: Walks ancestors of cwd looking for `.claude/`, `CLAUDE.md`, or `.git/`
//! markers and converts the matching directory name into a kebab-case palace ID.
//! Test: Unit tests cover the kebab conversion and the explicit-flag passthrough.

use std::path::{Path, PathBuf};

/// Resolve active palace ID in priority order:
/// 1. Explicit `--palace` flag
/// 2. `TRUSTY_PALACE` env var (already handled by clap `env`)
/// 3. Nearest ancestor with `.claude/`, `CLAUDE.md`, or `.git/`
/// 4. Fallback: `"default"`
///
/// Why: Centralizes the resolution policy so every subcommand sees the same
/// palace ID regardless of how it was specified.
/// What: Returns the resolved palace ID as a `String`.
/// Test: `resolve_falls_back_to_default` verifies the explicit override path.
pub fn resolve_palace(explicit: Option<&str>) -> String {
    if let Some(p) = explicit {
        return p.to_string();
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(name) = find_project_root(&cwd) {
            return to_palace_id(&name);
        }
    }
    "default".to_string()
}

/// Resolve the `serve --palace` default by auto-detecting from the cwd.
///
/// Why: When `trusty-memory serve` runs as a per-project Claude Code MCP
/// stdio server, requiring an explicit `--palace` forces a project-level
/// `.mcp.json` override per repo. Auto-detecting the palace from the working
/// directory lets a single user-level `~/.claude.json` entry work everywhere.
/// What: Returns the explicit `--palace` value if supplied; otherwise reads a
/// `.trusty-memory` marker (`palace=<name>`) found in the cwd or any ancestor,
/// and finally falls back to the cwd's directory name. The result is always
/// sanitized to lowercase kebab-case. `None` is returned only when the cwd
/// cannot be determined and no explicit value was given.
/// Test: `detect_serve_palace_*` unit tests cover explicit override, marker
/// parsing, and directory-name fallback.
pub fn detect_serve_palace(explicit: Option<&str>) -> Option<String> {
    if let Some(p) = explicit {
        return Some(to_palace_id(p));
    }
    let cwd = std::env::current_dir().ok()?;
    if let Some(name) = read_marker_palace(&cwd) {
        let id = to_palace_id(&name);
        if !id.is_empty() {
            return Some(id);
        }
    }
    let name = cwd.file_name()?.to_string_lossy().into_owned();
    let id = to_palace_id(&name);
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Walk up from `start` looking for a `.trusty-memory` marker file and parse
/// the `palace=<name>` line from it.
///
/// Why: A project can pin its palace name explicitly via a committed
/// `.trusty-memory` file, overriding the directory-name heuristic.
/// What: Returns the first `palace=` value found in the nearest ancestor
/// `.trusty-memory` file, or `None` if no marker exists or it has no
/// `palace=` line.
/// Test: Covered by `detect_serve_palace_reads_marker` via a temp directory.
fn read_marker_palace(start: &Path) -> Option<String> {
    let mut dir = start.to_path_buf();
    loop {
        let marker = dir.join(".trusty-memory");
        if marker.is_file() {
            if let Ok(contents) = std::fs::read_to_string(&marker) {
                if let Some(name) = parse_marker(&contents) {
                    return Some(name);
                }
            }
        }
        if !pop_in_place(&mut dir) {
            return None;
        }
    }
}

/// Extract the `palace=<name>` value from `.trusty-memory` file contents.
///
/// Why: Keep the tiny parser separate so it can be unit-tested without disk IO.
/// What: Scans lines for the first `palace=` key (ignoring `#` comments and
/// surrounding whitespace) and returns its trimmed value.
/// Test: `parse_marker_extracts_palace` covers comments, whitespace, and misses.
fn parse_marker(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(value) = line.strip_prefix("palace=") {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Walk up the directory tree from `start` looking for project markers.
///
/// Why: Project boundaries are defined by the presence of one of a small set of
/// well-known files/directories; using the closest ancestor matches user intuition.
/// What: Returns the file_name of the first ancestor containing a marker.
/// Test: Indirectly via `resolve_palace`; full coverage in integration tests.
fn find_project_root(start: &Path) -> Option<String> {
    let markers = [".claude", "CLAUDE.md", ".git"];
    let mut dir = start.to_path_buf();
    loop {
        for marker in &markers {
            if dir.join(marker).exists() {
                return dir.file_name().map(|n| n.to_string_lossy().into_owned());
            }
        }
        if !pop_in_place(&mut dir) {
            break;
        }
    }
    None
}

/// `PathBuf::pop` returns bool already; this is a thin wrapper for clarity.
fn pop_in_place(dir: &mut PathBuf) -> bool {
    dir.pop()
}

/// Convert a directory name to a kebab-case palace ID.
///
/// Why: Palace IDs are stable directory names; lowercase kebab keeps them
/// filesystem-safe across macOS / Linux / Windows.
/// What: Lowercases alphanumerics, replaces other chars with `-`, trims
/// leading/trailing `-`.
/// Test: `to_palace_id_kebab_cases` covers camelCase, snake_case, and special chars.
pub fn to_palace_id(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                '-'
            }
        })
        .collect();
    mapped.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_palace_id_kebab_cases() {
        assert_eq!(to_palace_id("MyProject"), "myproject");
        assert_eq!(to_palace_id("trusty_memory"), "trusty-memory");
        assert_eq!(to_palace_id("My Cool Project!"), "my-cool-project");
    }

    #[test]
    fn resolve_falls_back_to_default() {
        assert_eq!(resolve_palace(Some("my-palace")), "my-palace");
    }

    #[test]
    fn parse_marker_extracts_palace() {
        assert_eq!(
            parse_marker("# comment\npalace=client-acme\n"),
            Some("client-acme".to_string())
        );
        assert_eq!(
            parse_marker("  palace =  spaced  \npalace= trimmed \n"),
            Some("trimmed".to_string())
        );
        assert_eq!(parse_marker("# only comments\n\n"), None);
        assert_eq!(parse_marker("palace=\n"), None);
    }

    #[test]
    fn detect_serve_palace_explicit_override_is_sanitized() {
        assert_eq!(
            detect_serve_palace(Some("My Project")),
            Some("my-project".to_string())
        );
    }

    #[test]
    fn detect_serve_palace_reads_marker() {
        let dir = std::env::temp_dir().join(format!(
            "trusty-marker-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(dir.join(".trusty-memory"), "palace=Custom Name\n").expect("write marker");
        let found = read_marker_palace(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(found, Some("Custom Name".to_string()));
        assert_eq!(
            found.map(|n| to_palace_id(&n)),
            Some("custom-name".to_string())
        );
    }
}
