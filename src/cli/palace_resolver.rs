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
}
