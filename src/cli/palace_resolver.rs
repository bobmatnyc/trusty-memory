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
/// `.trusty-memory.toml` / `.trusty-memory` marker (`palace=<name>`) found in
/// the cwd or any ancestor, then falls back to the directory name of the git
/// repository root, and finally the cwd's directory name. The result is always
/// sanitized to lowercase kebab-case. Returns `Some("default")` when nothing
/// can be detected, and `None` only when the cwd cannot be determined.
/// Test: `detect_serve_palace_*` unit tests cover explicit override, marker
/// parsing, and directory-name fallback.
pub fn detect_serve_palace(explicit: Option<&str>) -> Option<String> {
    // 1. Explicit `--palace` flag wins.
    if let Some(p) = explicit {
        return Some(to_palace_id(p));
    }
    let cwd = std::env::current_dir().ok()?;
    // 2. `.trusty-memory.toml` / `.trusty-memory` marker file in cwd/ancestors.
    if let Some(name) = read_marker_palace(&cwd) {
        let id = to_palace_id(&name);
        if !id.is_empty() {
            return Some(id);
        }
    }
    // 3. Directory name of the git repository root.
    if let Some(name) = git_root_dir_name(&cwd) {
        let id = to_palace_id(&name);
        if !id.is_empty() {
            return Some(id);
        }
    }
    // 4. Directory name of the cwd.
    if let Some(name) = cwd.file_name() {
        let id = to_palace_id(&name.to_string_lossy());
        if !id.is_empty() {
            return Some(id);
        }
    }
    // 5. Last-resort fallback.
    Some("default".to_string())
}

/// Determine the directory name of the git repository root containing `cwd`.
///
/// Why: A palace maps naturally to a project, and the git root is the most
/// reliable project boundary. Running `git rev-parse --show-toplevel` honours
/// worktrees and submodule layouts that a pure ancestor-walk would miss.
/// What: Runs `git -C <cwd> rev-parse --show-toplevel`; on success returns the
/// last path component of the reported root. Returns `None` if `cwd` is not in
/// a git repository or `git` is unavailable.
/// Test: `git_root_dir_name_outside_repo_is_none` covers the non-repo path.
fn git_root_dir_name(cwd: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8(output.stdout).ok()?;
    let root = root.trim();
    if root.is_empty() {
        return None;
    }
    Path::new(root)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// Walk up from `start` looking for a `.trusty-memory.toml` or `.trusty-memory`
/// marker file and parse the `palace=<name>` line from it.
///
/// Why: A project can pin its palace name explicitly via a committed marker
/// file, overriding the directory-name heuristic. `.trusty-memory.toml` is the
/// preferred name; `.trusty-memory` is accepted for backward compatibility.
/// What: Returns the first `palace=` value found in the nearest ancestor
/// marker file, or `None` if no marker exists or it has no `palace=` line.
/// `.trusty-memory.toml` takes precedence over `.trusty-memory` in the same
/// directory.
/// Test: Covered by `detect_serve_palace_reads_marker` via a temp directory.
fn read_marker_palace(start: &Path) -> Option<String> {
    const MARKER_NAMES: [&str; 2] = [".trusty-memory.toml", ".trusty-memory"];
    let mut dir = start.to_path_buf();
    loop {
        for marker_name in MARKER_NAMES {
            let marker = dir.join(marker_name);
            if marker.is_file() {
                if let Ok(contents) = std::fs::read_to_string(&marker) {
                    if let Some(name) = parse_marker(&contents) {
                        return Some(name);
                    }
                }
            }
        }
        if !pop_in_place(&mut dir) {
            return None;
        }
    }
}

/// Extract the `palace` value from `.trusty-memory` / `.trusty-memory.toml`
/// file contents.
///
/// Why: Keep the tiny parser separate so it can be unit-tested without disk IO.
/// Supporting both the bare `palace=name` form and the TOML `palace = "name"`
/// form lets one parser serve both marker file variants.
/// What: Scans lines for the first `palace` key (ignoring `#` comments and
/// surrounding whitespace), accepts optional spaces around `=`, strips matching
/// surrounding single/double quotes, and returns the trimmed value.
/// Test: `parse_marker_extracts_palace` covers comments, whitespace, quotes,
/// and misses.
fn parse_marker(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "palace" {
            continue;
        }
        let value = value.trim();
        // Strip a single matching pair of surrounding quotes (TOML strings).
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| {
                value
                    .strip_prefix('\'')
                    .and_then(|v| v.strip_suffix('\''))
            })
            .unwrap_or(value)
            .trim();
        if !value.is_empty() {
            return Some(value.to_string());
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
        // Bare form.
        assert_eq!(
            parse_marker("# comment\npalace=client-acme\n"),
            Some("client-acme".to_string())
        );
        // Spaces around `=` are tolerated (TOML-friendly).
        assert_eq!(
            parse_marker("  palace =  spaced  \n"),
            Some("spaced".to_string())
        );
        // TOML quoted-string form.
        assert_eq!(
            parse_marker("palace = \"quoted-name\"\n"),
            Some("quoted-name".to_string())
        );
        assert_eq!(
            parse_marker("palace = 'single'\n"),
            Some("single".to_string())
        );
        // First matching key wins.
        assert_eq!(
            parse_marker("palace = first\npalace = second\n"),
            Some("first".to_string())
        );
        assert_eq!(parse_marker("# only comments\n\n"), None);
        assert_eq!(parse_marker("palace=\n"), None);
        assert_eq!(parse_marker("other = value\n"), None);
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

    #[test]
    fn detect_serve_palace_reads_toml_marker() {
        let dir = std::env::temp_dir().join(format!(
            "trusty-toml-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(
            dir.join(".trusty-memory.toml"),
            "# trusty-memory project config\npalace = client-beta\n",
        )
        .expect("write toml marker");
        let found = read_marker_palace(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            found.map(|n| to_palace_id(&n)),
            Some("client-beta".to_string())
        );
    }

    #[test]
    fn git_root_dir_name_outside_repo_is_none() {
        // The OS temp dir is not a git repository, so `git rev-parse` fails.
        let tmp = std::env::temp_dir();
        assert_eq!(git_root_dir_name(&tmp), None);
    }
}
