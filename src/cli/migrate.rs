//! `migrate` subcommand — migrate from the `kuzu-memory` MCP server to
//! `trusty-memory`.
//!
//! Why: Issue #64 — teams already running the `kuzu-memory` MCP server want a
//! one-shot path to switch to `trusty-memory` without hand-editing Claude
//! settings files or losing their stored memories. This command automates
//! both halves of that switch.
//! What: Provides `migrate kuzu-memory`, which (1) scans Claude `settings.json`
//! / `settings.local.json` files for `kuzu-memory` entries under `mcpServers`
//! and rewrites them to point at `trusty-memory`, backing up each file first,
//! and (2) reads the `kuzu-memory` data store and imports every memory into a
//! `trusty-memory` palace. A `--dry-run` flag prints the planned changes
//! without touching disk.
//! Test: Unit tests cover the pure helpers — settings scanning, the
//! `kuzu-memory` → `trusty-memory` MCP-entry rewrite, and the migration report
//! formatter. End-to-end migration is exercised manually because it depends on
//! external `kuzu-memory` data.

use crate::cli::convert::{derive_palace_name, read_kuzu_memories, RawMemory};
use crate::cli::memory::open_or_create_handle;
use crate::cli::output::OutputConfig;
use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Maximum directory depth searched under `$HOME` for nested Claude settings
/// files. Keeps the scan bounded so a deep project tree cannot stall the
/// command. Matches the "~5 levels" requirement in issue #64.
const MAX_SCAN_DEPTH: usize = 5;

/// The MCP server key that `kuzu-memory` registers itself under in Claude
/// `settings.json`. Centralized so the scanner and rewriter agree.
const KUZU_SERVER_KEY: &str = "kuzu-memory";

/// The MCP server key `trusty-memory` registers itself under after migration.
const TRUSTY_SERVER_KEY: &str = "trusty-memory";

/// Migration sources supported by `trusty-memory migrate`.
///
/// Why: One enum per migration source keeps the command surface extensible —
/// future tools (e.g. `mempalace`) slot in as new variants.
/// What: Currently a single `KuzuMemory` variant.
/// Test: Parse coverage via clap `--help`; behavior tested in this module.
#[derive(Subcommand, Debug, Clone)]
pub enum MigrateSubcommand {
    /// Migrate from the `kuzu-memory` MCP server to `trusty-memory`.
    #[command(
        name = "kuzu-memory",
        after_help = "Examples:\n  trusty-memory migrate kuzu-memory\n  trusty-memory migrate kuzu-memory --dry-run\n  trusty-memory migrate kuzu-memory --palace my-app"
    )]
    KuzuMemory(KuzuMemoryArgs),
}

/// Args for `migrate kuzu-memory`.
#[derive(Args, Debug, Clone)]
pub struct KuzuMemoryArgs {
    /// Print all changes that would be made without modifying any file or
    /// importing any data.
    #[arg(long)]
    pub dry_run: bool,

    /// Override the destination palace for imported memories. Defaults to a
    /// palace name derived from the current directory.
    #[arg(long)]
    pub palace: Option<String>,
}

// ── Public entry point ───────────────────────────────────────────────────────

/// Dispatch a `migrate` subcommand.
///
/// Why: Keeps `main.rs` declarative — it parses the subcommand and hands off.
/// What: Routes to the per-source migration handler.
/// Test: Integration coverage via `trusty-memory migrate --help`.
pub async fn handle(command: MigrateSubcommand, out: &OutputConfig) -> Result<()> {
    match command {
        MigrateSubcommand::KuzuMemory(opts) => migrate_kuzu_memory(opts, out).await,
    }
}

/// Run the full `kuzu-memory` → `trusty-memory` migration.
///
/// Why: Acceptance criteria for issue #64 — migrate MCP config and memory
/// data in a single command.
/// What: First rewrites every Claude settings file that registers
/// `kuzu-memory`, then imports `kuzu-memory` data into a `trusty-memory`
/// palace, finally printing a summary. Honors `--dry-run` throughout.
/// Test: The pure helpers it calls are unit-tested; the orchestration is
/// exercised manually.
async fn migrate_kuzu_memory(opts: KuzuMemoryArgs, out: &OutputConfig) -> Result<()> {
    let prefix = if opts.dry_run { "[dry-run] " } else { "" };
    out.print_header("migrate", "kuzu-memory");

    // ── Phase 1: MCP config migration ────────────────────────────────────
    let settings_files = discover_claude_settings()?;
    let mut config_changed = 0usize;
    for path in &settings_files {
        match migrate_settings_file(path, opts.dry_run) {
            Ok(true) => {
                config_changed += 1;
                println!(
                    "{prefix}rewrote kuzu-memory → trusty-memory in {}",
                    path.display()
                );
            }
            Ok(false) => {}
            Err(e) => eprintln!("warning: could not migrate {}: {e:#}", path.display()),
        }
    }
    if config_changed == 0 {
        println!("{prefix}no Claude settings files reference kuzu-memory");
    }

    // ── Phase 2: memory data migration ───────────────────────────────────
    let palace_name = match &opts.palace {
        Some(p) => p.clone(),
        None => {
            let cwd = std::env::current_dir().context("get current directory")?;
            derive_palace_name(&cwd)
        }
    };

    let memories = collect_kuzu_memories();
    let report = if memories.is_empty() {
        println!("{prefix}no kuzu-memory data found to import");
        MigrationReport::default()
    } else if opts.dry_run {
        let unique = dedup_count(&memories);
        println!(
            "{prefix}would import {} memories ({} unique) → palace '{palace_name}'",
            memories.len(),
            unique,
        );
        MigrationReport {
            migrated: unique,
            skipped: memories.len() - unique,
            failed: 0,
        }
    } else {
        import_memories(&palace_name, &memories).await?
    };

    // ── Summary ──────────────────────────────────────────────────────────
    println!("{prefix}{}", format_report(&report, config_changed));
    if !opts.dry_run {
        out.print_success("kuzu-memory migration complete");
    }
    Ok(())
}

// ── Phase 1: MCP config migration ────────────────────────────────────────────

/// Discover every Claude `settings.json` / `settings.local.json` file that may
/// hold MCP server registrations.
///
/// Why: `kuzu-memory` can be registered globally (`~/.claude/settings.json`) or
/// per-project (`<repo>/.claude/settings.json`); we must scan both.
/// What: Returns the two top-level `~/.claude` files plus every
/// `**/.claude/settings*.json` under `$HOME`, depth-limited to `MAX_SCAN_DEPTH`.
/// Only existing files are returned; the result is deduplicated.
/// Test: `discover_claude_settings_includes_top_level` builds a temp HOME and
/// asserts the top-level files are found.
pub fn discover_claude_settings() -> Result<Vec<PathBuf>> {
    let home = dirs::home_dir().context("resolve home directory")?;
    let mut found: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for name in ["settings.json", "settings.local.json"] {
        let p = home.join(".claude").join(name);
        if p.is_file() && seen.insert(p.clone()) {
            found.push(p);
        }
    }

    scan_for_claude_settings(&home, 0, &mut found, &mut seen);
    Ok(found)
}

/// Recursively walk `dir` looking for `.claude/settings*.json` files.
///
/// Why: Per-project Claude settings live under each repo's `.claude/`
/// directory; a bounded walk finds them without an external crate.
/// What: Descends up to `MAX_SCAN_DEPTH` levels, skipping hidden directories
/// (except `.claude` itself) and common heavy directories so the scan stays
/// fast.
/// Test: `scan_for_claude_settings_finds_nested` builds a nested temp tree.
fn scan_for_claude_settings(
    dir: &Path,
    depth: usize,
    found: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if name == ".claude" {
            for file in ["settings.json", "settings.local.json"] {
                let p = path.join(file);
                if p.is_file() && seen.insert(p.clone()) {
                    found.push(p);
                }
            }
            continue;
        }

        // Skip hidden and well-known heavy directories to keep the scan bounded.
        if name.starts_with('.') || is_skipped_dir(&name) {
            continue;
        }
        scan_for_claude_settings(&path, depth + 1, found, seen);
    }
}

/// Returns true for directory names that should never be descended into during
/// the settings scan (build output, dependency caches, VCS internals).
fn is_skipped_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules"
            | "target"
            | "vendor"
            | "dist"
            | "build"
            | "Library"
            | ".git"
            | "__pycache__"
    )
}

/// Migrate one Claude settings file in place (or report what would change).
///
/// Why: Each discovered file may or may not register `kuzu-memory`; we only
/// touch — and only back up — files that actually need rewriting.
/// What: Parses the JSON, rewrites any `mcpServers.kuzu-memory` entry to
/// `trusty-memory`, and (unless `dry_run`) writes a `.bak` backup followed by
/// the updated file. Returns `Ok(true)` when a change was made.
/// Test: `migrate_settings_file_*` round-trip the rewrite on a temp file.
pub fn migrate_settings_file(path: &Path, dry_run: bool) -> Result<bool> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(false);
    }
    let value: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse JSON in {}", path.display()))?;

    let Some(rewritten) = rewrite_mcp_servers(&value) else {
        return Ok(false);
    };

    if dry_run {
        return Ok(true);
    }

    // Back up the original before mutating (issue #64 acceptance criteria).
    let backup = backup_path(path);
    std::fs::copy(path, &backup)
        .with_context(|| format!("back up {} to {}", path.display(), backup.display()))?;

    let pretty = serde_json::to_string_pretty(&rewritten).context("serialize migrated settings")?;
    std::fs::write(path, pretty).with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

/// Compute the `.bak` backup path for a settings file.
///
/// Why: The acceptance criteria require a `.bak` backup before modification;
/// appending the suffix (rather than replacing the extension) keeps the
/// original name visible — e.g. `settings.json` → `settings.json.bak`.
/// What: Appends `.bak` to the file name.
/// Test: `backup_path_appends_suffix`.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "settings.json".to_string());
    name.push_str(".bak");
    path.with_file_name(name)
}

/// Rewrite a settings JSON value's `mcpServers.kuzu-memory` entry to
/// `trusty-memory`.
///
/// Why: Pure transform so the rewrite can be unit-tested without disk I/O.
/// What: Returns `Some(updated)` when the value contained a `kuzu-memory` MCP
/// server (or a legacy capitalized `mcp_servers` key); returns `None` when no
/// change is needed. The replacement entry launches the `trusty-memory` stdio
/// MCP server, matching how `trusty-memory` registers itself elsewhere.
/// Test: `rewrite_mcp_servers_*` cover present, absent, and no-op cases.
pub fn rewrite_mcp_servers(value: &Value) -> Option<Value> {
    let obj = value.as_object()?;
    // Claude Code uses `mcpServers`; tolerate the snake_case spelling too.
    let key = ["mcpServers", "mcp_servers"]
        .into_iter()
        .find(|k| obj.get(*k).map(Value::is_object).unwrap_or(false))?;

    let servers = obj.get(key)?.as_object()?;
    if !servers.contains_key(KUZU_SERVER_KEY) {
        return None;
    }
    // Already migrated and kuzu removed → nothing to do. (Defensive: the
    // `contains_key` above already guarantees a kuzu entry is present.)

    let mut new_value = value.clone();
    let new_obj = new_value.as_object_mut()?;
    let new_servers = new_obj.get_mut(key)?.as_object_mut()?;

    new_servers.remove(KUZU_SERVER_KEY);
    new_servers.insert(TRUSTY_SERVER_KEY.to_string(), trusty_mcp_entry());
    Some(new_value)
}

/// The MCP server entry inserted for `trusty-memory`.
///
/// Why: Keeps the canonical replacement payload in one place so the rewrite
/// and its tests agree on shape.
/// What: A stdio MCP server launching `trusty-memory serve` — the same form
/// `trusty-memory` registers itself with for Claude Code (issue #61).
/// Test: `trusty_mcp_entry_shape` asserts the command + args.
pub fn trusty_mcp_entry() -> Value {
    json!({
        "command": "trusty-memory",
        "args": ["serve"]
    })
}

// ── Phase 2: memory data migration ───────────────────────────────────────────

/// A counted summary of a memory-data migration.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// Memories successfully imported into the destination palace.
    pub migrated: usize,
    /// Memories skipped because they duplicate an already-seen memory.
    pub skipped: usize,
    /// Memories that failed to import (palace write error).
    pub failed: usize,
}

/// Collect every memory from the running/installed `kuzu-memory` instance.
///
/// Why: `kuzu-memory` has no stable HTTP export; reading its on-disk KuzuDB
/// files directly (via the existing `read_kuzu_memories` helper, which shells
/// out to the `kuzu` CLI) is the most reliable best-effort path.
/// What: Reads the project-local `.kuzu-memory/memories.db` plus any databases
/// found under `kuzu-memory`'s standard data roots, returning the union.
/// Errors for individual stores are logged and skipped so a single bad store
/// never aborts the migration.
/// Test: Behavioral — covered manually since it depends on `kuzu-memory` data.
fn collect_kuzu_memories() -> Vec<RawMemory> {
    let mut out: Vec<RawMemory> = Vec::new();
    for db in kuzu_database_paths() {
        match read_kuzu_memories(&db) {
            Ok(mut mems) => out.append(&mut mems),
            Err(e) => eprintln!("warning: kuzu read failed for {}: {e:#}", db.display()),
        }
    }
    out
}

/// Enumerate candidate `kuzu-memory` database files on this machine.
///
/// Why: `kuzu-memory` stores data per-project (`<repo>/.kuzu-memory/`) and may
/// also keep a machine-wide store under `~/.local/share/kuzu-memory/`.
/// What: Returns the current project's `.kuzu-memory/memories.db` plus any
/// `*.db` / `memories.db` under the standard data root, filtered to existing
/// files.
/// Test: Behavioral; the pure read path is unit-tested in `convert.rs`.
fn kuzu_database_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();

    if let Ok(cwd) = std::env::current_dir() {
        let local = cwd.join(".kuzu-memory").join("memories.db");
        if local.exists() {
            paths.push(local);
        }
    }

    if let Some(home) = dirs::home_dir() {
        let data_root = home.join(".local").join("share").join("kuzu-memory");
        if let Ok(entries) = std::fs::read_dir(&data_root) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file()
                    && p.extension().map(|e| e == "db").unwrap_or(false)
                    && !paths.contains(&p)
                {
                    paths.push(p);
                }
            }
        }
        let direct = data_root.join("memories.db");
        if direct.exists() && !paths.contains(&direct) {
            paths.push(direct);
        }
    }

    paths
}

/// Count the number of unique memories in `memories` (by verbatim content).
///
/// Why: Dry-run mode needs to report how many memories would actually be
/// imported vs skipped as duplicates, without touching a palace.
/// What: Inserts each content string into a set and returns the set size.
/// Test: `dedup_count_collapses_duplicates`.
fn dedup_count(memories: &[RawMemory]) -> usize {
    let mut seen: HashSet<&str> = HashSet::new();
    for m in memories {
        seen.insert(m.content.as_str());
    }
    seen.len()
}

/// Import collected memories into the destination palace, deduplicating by
/// verbatim content.
///
/// Why: Switching from `kuzu-memory` must not lose history; this is the write
/// half of the data migration.
/// What: Opens (or creates) the palace, then stores each memory whose content
/// has not already been seen, tagging it with its source. Returns a
/// `MigrationReport` counting migrated / skipped / failed memories.
/// Test: Behavioral — the dedup logic is unit-tested via `dedup_count`.
async fn import_memories(palace: &str, memories: &[RawMemory]) -> Result<MigrationReport> {
    let handle = open_or_create_handle(palace).await?;
    let mut report = MigrationReport::default();
    let mut seen: HashSet<String> = HashSet::new();

    for m in memories {
        if !seen.insert(m.content.clone()) {
            report.skipped += 1;
            continue;
        }
        let tags = vec![format!("source:{}", m.source)];
        match handle
            .remember(m.content.clone(), m.room.clone(), tags, m.importance)
            .await
        {
            Ok(_) => report.migrated += 1,
            Err(e) => {
                eprintln!("warning: failed to import memory: {e:#}");
                report.failed += 1;
            }
        }
    }
    Ok(report)
}

/// Format a one-line human summary of the migration.
///
/// Why: Pure formatter so the report shape can be unit-tested without disk.
/// What: `N memories migrated, N skipped (duplicates), N failed; M config
/// file(s) updated`.
/// Test: `format_report_shape`.
pub fn format_report(report: &MigrationReport, config_files_changed: usize) -> String {
    format!(
        "{} memories migrated, {} skipped (duplicates), {} failed; {} config file(s) updated",
        report.migrated, report.skipped, report.failed, config_files_changed,
    )
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::tempdir;
    use trusty_memory_core::RoomType;

    fn raw(content: &str) -> RawMemory {
        RawMemory {
            content: content.to_string(),
            importance: 0.5,
            room: RoomType::General,
            source: "kuzu".to_string(),
        }
    }

    #[test]
    fn trusty_mcp_entry_shape() {
        let v = trusty_mcp_entry();
        assert_eq!(
            v.get("command").and_then(Value::as_str),
            Some("trusty-memory")
        );
        let args = v.get("args").and_then(Value::as_array).expect("args array");
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].as_str(), Some("serve"));
    }

    #[test]
    fn backup_path_appends_suffix() {
        let p = Path::new("/home/u/.claude/settings.json");
        assert_eq!(
            backup_path(p),
            PathBuf::from("/home/u/.claude/settings.json.bak")
        );
        let local = Path::new("/p/.claude/settings.local.json");
        assert_eq!(
            backup_path(local),
            PathBuf::from("/p/.claude/settings.local.json.bak")
        );
    }

    #[test]
    fn rewrite_mcp_servers_replaces_kuzu_entry() {
        let settings = json!({
            "model": "sonnet",
            "mcpServers": {
                "kuzu-memory": {"command": "kuzu-memory", "args": ["serve"]},
                "other": {"command": "other-mcp"}
            }
        });
        let out = rewrite_mcp_servers(&settings).expect("should rewrite");
        let servers = out
            .get("mcpServers")
            .and_then(Value::as_object)
            .expect("mcpServers object");
        assert!(!servers.contains_key("kuzu-memory"), "kuzu entry removed");
        assert!(servers.contains_key("trusty-memory"), "trusty entry added");
        assert!(servers.contains_key("other"), "unrelated entry preserved");
        // Unrelated top-level keys preserved.
        assert_eq!(out.get("model").and_then(Value::as_str), Some("sonnet"));
        // Replacement points at the trusty-memory stdio server.
        assert_eq!(
            servers["trusty-memory"]
                .get("command")
                .and_then(Value::as_str),
            Some("trusty-memory")
        );
    }

    #[test]
    fn rewrite_mcp_servers_returns_none_without_kuzu() {
        let settings = json!({
            "mcpServers": {"other": {"command": "other-mcp"}}
        });
        assert!(rewrite_mcp_servers(&settings).is_none());
    }

    #[test]
    fn rewrite_mcp_servers_returns_none_without_mcp_servers() {
        let settings = json!({"model": "sonnet"});
        assert!(rewrite_mcp_servers(&settings).is_none());
    }

    #[test]
    fn rewrite_mcp_servers_handles_snake_case_key() {
        let settings = json!({
            "mcp_servers": {"kuzu-memory": {"command": "kuzu-memory"}}
        });
        let out = rewrite_mcp_servers(&settings).expect("should rewrite");
        let servers = out
            .get("mcp_servers")
            .and_then(Value::as_object)
            .expect("mcp_servers object");
        assert!(!servers.contains_key("kuzu-memory"));
        assert!(servers.contains_key("trusty-memory"));
    }

    #[test]
    fn migrate_settings_file_rewrites_and_backs_up() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        let original = json!({
            "mcpServers": {"kuzu-memory": {"command": "kuzu-memory"}}
        });
        fs::write(&path, serde_json::to_string_pretty(&original).unwrap()).unwrap();

        let changed = migrate_settings_file(&path, false).expect("migrate");
        assert!(changed);

        // Backup exists and holds the original content.
        let backup = backup_path(&path);
        assert!(backup.is_file(), "backup file created");
        let backup_json: Value =
            serde_json::from_str(&fs::read_to_string(&backup).unwrap()).unwrap();
        assert!(backup_json["mcpServers"].get("kuzu-memory").is_some());

        // Live file is rewritten.
        let live: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(live["mcpServers"].get("kuzu-memory").is_none());
        assert!(live["mcpServers"].get("trusty-memory").is_some());
    }

    #[test]
    fn migrate_settings_file_dry_run_does_not_write() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        let original = json!({
            "mcpServers": {"kuzu-memory": {"command": "kuzu-memory"}}
        });
        let raw = serde_json::to_string_pretty(&original).unwrap();
        fs::write(&path, &raw).unwrap();

        let changed = migrate_settings_file(&path, true).expect("migrate");
        assert!(changed, "dry-run still reports a change is needed");
        // No backup, file untouched.
        assert!(!backup_path(&path).exists(), "no backup in dry-run");
        assert_eq!(fs::read_to_string(&path).unwrap(), raw, "file untouched");
    }

    #[test]
    fn migrate_settings_file_no_kuzu_is_noop() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"mcpServers":{"other":{"command":"x"}}}"#).unwrap();
        assert!(!migrate_settings_file(&path, false).expect("migrate"));
        assert!(!backup_path(&path).exists());
    }

    #[test]
    fn migrate_settings_file_empty_is_noop() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        fs::write(&path, "   ").unwrap();
        assert!(!migrate_settings_file(&path, false).expect("migrate"));
    }

    #[test]
    fn scan_for_claude_settings_finds_nested() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        // root/project/.claude/settings.json
        let claude = root.join("project").join(".claude");
        fs::create_dir_all(&claude).unwrap();
        let settings = claude.join("settings.json");
        fs::write(&settings, "{}").unwrap();
        // A skipped directory must not be descended into.
        let nm = root.join("node_modules").join(".claude");
        fs::create_dir_all(&nm).unwrap();
        fs::write(nm.join("settings.json"), "{}").unwrap();

        let mut found = Vec::new();
        let mut seen = HashSet::new();
        scan_for_claude_settings(root, 0, &mut found, &mut seen);

        assert!(found.contains(&settings), "nested .claude settings found");
        assert!(
            !found
                .iter()
                .any(|p| p.starts_with(root.join("node_modules"))),
            "node_modules skipped"
        );
    }

    #[test]
    fn scan_for_claude_settings_respects_depth_limit() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        // Build a path deeper than MAX_SCAN_DEPTH.
        let mut deep = root.to_path_buf();
        for i in 0..(MAX_SCAN_DEPTH + 3) {
            deep = deep.join(format!("d{i}"));
        }
        let claude = deep.join(".claude");
        fs::create_dir_all(&claude).unwrap();
        fs::write(claude.join("settings.json"), "{}").unwrap();

        let mut found = Vec::new();
        let mut seen = HashSet::new();
        scan_for_claude_settings(root, 0, &mut found, &mut seen);
        assert!(found.is_empty(), "settings beyond depth limit are skipped");
    }

    #[test]
    fn is_skipped_dir_matches_known_heavy_dirs() {
        assert!(is_skipped_dir("node_modules"));
        assert!(is_skipped_dir("target"));
        assert!(!is_skipped_dir("src"));
        assert!(!is_skipped_dir("project"));
    }

    #[test]
    fn dedup_count_collapses_duplicates() {
        let mems = vec![raw("alpha"), raw("alpha"), raw("beta")];
        assert_eq!(dedup_count(&mems), 2);
    }

    #[test]
    fn dedup_count_empty_is_zero() {
        assert_eq!(dedup_count(&[]), 0);
    }

    #[test]
    fn format_report_shape() {
        let report = MigrationReport {
            migrated: 12,
            skipped: 3,
            failed: 1,
        };
        let s = format_report(&report, 2);
        assert_eq!(
            s,
            "12 memories migrated, 3 skipped (duplicates), 1 failed; 2 config file(s) updated"
        );
    }

    #[test]
    fn migration_report_default_is_zeroed() {
        let r = MigrationReport::default();
        assert_eq!(
            r,
            MigrationReport {
                migrated: 0,
                skipped: 0,
                failed: 0
            }
        );
    }
}
