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
    let home = dirs::home_dir().context("resolve home directory")?;
    let settings_files = trusty_common::claude_config::discover_claude_settings(&home);
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

/// Migrate one Claude settings file in place (or report what would change).
///
/// Why: Each discovered file may or may not register `kuzu-memory`; we only
/// touch — and only back up — files that actually need rewriting.
/// What: Parses the JSON, rewrites any `mcpServers.kuzu-memory` entry to
/// `trusty-memory`, and (unless `dry_run`) writes the updated file via
/// `trusty_common::claude_config::write_json_atomic`, which backs the original
/// up to `<path>.bak` and swaps the new content into place atomically.
/// Returns `Ok(true)` when a change was made.
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

    // Atomic write with a `.bak` backup of the original (issue #64 acceptance
    // criteria). Delegated to the shared trusty-common helper.
    trusty_common::claude_config::write_json_atomic(path, &rewritten)?;
    Ok(true)
}

/// Compute the `.bak` backup path for a settings file.
///
/// Why: Used by tests to assert that `migrate_settings_file` produced the
/// backup `trusty_common::claude_config::write_json_atomic` writes. Appending
/// the suffix (rather than replacing the extension) keeps the original name
/// visible — e.g. `settings.json` → `settings.json.bak`.
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

    // Settings-file discovery is now provided by
    // `trusty_common::claude_config::discover_claude_settings` and covered by
    // that crate's `discover_finds_nested_claude_settings` test.

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
