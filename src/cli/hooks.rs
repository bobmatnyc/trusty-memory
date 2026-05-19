//! `hooks` subcommand — install and fire OS-level hooks that auto-create
//! memories from external events (git commits, Claude Code tool use, etc.).
//!
//! Why: Issue #25 — passive memory enrichment. Users shouldn't have to type
//! `trusty-memory remember ...` after every commit; instead, install hooks
//! that fire on real events and let the daemon ingest the context for them.
//! What: Provides `install` (writes shell hook scripts and patches
//! `~/.claude/settings.json`), `fire <event>` (called by the hooks; reads
//! event-specific stdin/git context and stores a drawer), `list` (shows
//! installation state), and `status` (recent hook-sourced drawers).
//! Test: Unit tests cover pure helpers (formatters, parsers, settings merge).
//! End-to-end install/fire flows are exercised manually and via integration.

use crate::cli::convert::derive_palace_name;
use crate::cli::memory::open_or_create_handle;
use crate::cli::output::OutputConfig;
use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use trusty_memory_core::retrieval::{recall_with_default_embedder, RecallResult};
use trusty_memory_core::RoomType;

/// Top-level args for the `hooks` subcommand.
#[derive(Args, Debug, Clone)]
pub struct HooksArgs {
    #[command(subcommand)]
    pub command: HooksSubcommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum HooksSubcommand {
    /// Install hooks (git, Claude Code, or all).
    Install(HooksInstallArgs),
    /// List installation state of known hooks.
    List,
    /// Fire a hook event (called by the installed hook scripts).
    Fire(HooksFireArgs),
    /// Show recent hook-sourced drawers in the active palace.
    Status,
}

#[derive(Args, Debug, Clone)]
pub struct HooksInstallArgs {
    /// Install the git post-commit hook in the current repo.
    #[arg(long)]
    pub git: bool,
    /// Install Claude Code Stop + PostToolUse hooks via ~/.claude/settings.json.
    #[arg(long)]
    pub claude_code: bool,
    /// Install all available hooks.
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug, Clone)]
pub struct HooksFireArgs {
    /// Event name: git.post-commit | claude.stop | claude.post-tool-use
    pub event: String,
    /// Palace to write to (defaults to current dir name).
    #[arg(long)]
    pub palace: Option<String>,
}

// ── Public entry point ───────────────────────────────────────────────────────

/// Dispatch a `hooks` subcommand.
///
/// Why: Mirrors the per-subcommand handler pattern used by other CLI groups.
/// What: Routes to install/list/fire/status implementations.
/// Test: Integration coverage via `trusty-memory hooks --help`.
pub async fn handle(args: HooksArgs, palace: &str, out: &OutputConfig) -> Result<()> {
    match args.command {
        HooksSubcommand::Install(opts) => handle_install(opts, out),
        HooksSubcommand::List => handle_list(out),
        HooksSubcommand::Fire(opts) => handle_fire(opts, palace).await,
        HooksSubcommand::Status => handle_status(palace, out).await,
    }
}

// ── Install ─────────────────────────────────────────────────────────────────

fn handle_install(opts: HooksInstallArgs, out: &OutputConfig) -> Result<()> {
    let want_git = opts.all || opts.git;
    let want_claude = opts.all || opts.claude_code;

    if !want_git && !want_claude {
        return Err(anyhow!(
            "no hook target specified — pass --git, --claude-code, or --all"
        ));
    }

    if want_git {
        install_git_hook()?;
    }
    if want_claude {
        install_claude_hooks()?;
    }
    out.print_success("hooks installed");
    Ok(())
}

/// Returns the canonical content of the git post-commit shell script.
///
/// Why: Centralizing the script lets us unit-test it and re-use across
/// install/list comparisons.
/// What: A POSIX shell script that calls `trusty-memory hooks fire`.
/// Test: `git_hook_script_content_shape` asserts the shebang + command.
pub fn git_hook_script_content() -> &'static str {
    "#!/bin/sh\ntrusty-memory hooks fire git.post-commit\n"
}

fn install_git_hook() -> Result<()> {
    let cwd = std::env::current_dir().context("get current directory")?;
    let git_dir = cwd.join(".git");
    if !git_dir.is_dir() {
        return Err(anyhow!(
            "not a git repository: {} has no .git directory",
            cwd.display()
        ));
    }
    let hooks_dir = git_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir).context("create .git/hooks directory")?;
    let hook_path = hooks_dir.join("post-commit");
    std::fs::write(&hook_path, git_hook_script_content())
        .with_context(|| format!("write {}", hook_path.display()))?;

    // chmod +x — Unix only; Windows inherits executable bit from filesystem.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&hook_path)
            .context("read hook permissions")?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook_path, perms).context("chmod +x post-commit")?;
    }

    eprintln!("✓ Installed git post-commit hook → {}", hook_path.display());
    Ok(())
}

fn claude_settings_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not resolve home directory"))?;
    Ok(home.join(".claude").join("settings.json"))
}

fn install_claude_hooks() -> Result<()> {
    let path = claude_settings_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create ~/.claude directory")?;
    }

    let existing: Value = if path.exists() {
        // Backup before mutating.
        let backup = path.with_extension("json.bak");
        std::fs::copy(&path, &backup)
            .with_context(|| format!("backup {} to {}", path.display(), backup.display()))?;
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        if raw.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&raw)
                .with_context(|| format!("parse JSON in {}", path.display()))?
        }
    } else {
        json!({})
    };

    let hooks_to_add = trusty_hook_entries();
    let merged = merge_claude_settings(&existing, &hooks_to_add);
    let pretty = serde_json::to_string_pretty(&merged).context("serialize merged settings")?;
    std::fs::write(&path, pretty).with_context(|| format!("write {}", path.display()))?;

    eprintln!("✓ Installed Claude Code hooks → {}", path.display());
    Ok(())
}

/// Default timeout (milliseconds) applied to the `UserPromptSubmit` hook.
///
/// Why: Issue #63 — without a timeout the Claude Code REPL freezes
/// indefinitely if `trusty-memory` is slow (cold start, DB lock). Claude Code
/// kills the hook process once this elapses.
/// What: 5 seconds — long enough for a warm recall, short enough to avoid a
/// visible REPL stall.
/// Test: `trusty_hook_entries_user_prompt_has_timeout` asserts the value.
const USER_PROMPT_HOOK_TIMEOUT_MS: u64 = 5_000;

/// Hook entries to merge into `~/.claude/settings.json`.
///
/// Why: Keep the canonical hook payload in one place so install + list +
/// tests agree on shape.
/// What: A JSON object matching Claude Code's `settings.json` `hooks` schema
/// for Stop + PostToolUse + UserPromptSubmit events. The UserPromptSubmit
/// command carries an explicit `timeout` (see `USER_PROMPT_HOOK_TIMEOUT_MS`)
/// so a slow daemon never freezes the REPL.
/// Test: `merge_claude_settings_*` tests use this exact shape;
/// `trusty_hook_entries_user_prompt_has_timeout` asserts the timeout.
fn trusty_hook_entries() -> Value {
    json!({
        "hooks": {
            "Stop": [
                {
                    "matcher": "",
                    "hooks": [
                        {"type": "command", "command": "trusty-memory hooks fire claude.stop"}
                    ]
                }
            ],
            "PostToolUse": [
                {
                    "matcher": "Write|Edit|Bash",
                    "hooks": [
                        {"type": "command", "command": "trusty-memory hooks fire claude.post-tool-use"}
                    ]
                }
            ],
            "UserPromptSubmit": [
                {
                    "matcher": "",
                    "hooks": [
                        {
                            "type": "command",
                            "command": "trusty-memory hooks fire claude.user-prompt",
                            "timeout": USER_PROMPT_HOOK_TIMEOUT_MS
                        }
                    ]
                }
            ]
        }
    })
}

/// Merge `additions.hooks` into `existing.hooks` without clobbering existing
/// entries.
///
/// Why: Users may already have other hook handlers in `settings.json`; we
/// must not overwrite them. We append our entries to each event array.
/// What: For each event in `additions.hooks`, append our entries to the
/// existing array (or create it). Skip exact-duplicate command entries.
/// Test: `merge_claude_settings_preserves_existing` and
/// `merge_claude_settings_no_duplicate` cover the core invariants.
pub fn merge_claude_settings(existing: &Value, additions: &Value) -> Value {
    let mut merged = existing.clone();
    if !merged.is_object() {
        merged = json!({});
    }
    let merged_obj = merged.as_object_mut().expect("ensured object above");
    let hooks_entry = merged_obj
        .entry("hooks".to_string())
        .or_insert_with(|| json!({}));
    if !hooks_entry.is_object() {
        *hooks_entry = json!({});
    }
    let hooks_obj = hooks_entry.as_object_mut().expect("ensured object above");

    let Some(add_hooks) = additions.get("hooks").and_then(|h| h.as_object()) else {
        return merged;
    };

    for (event, new_arr) in add_hooks {
        let Some(new_entries) = new_arr.as_array() else {
            continue;
        };
        let target = hooks_obj.entry(event.clone()).or_insert_with(|| json!([]));
        if !target.is_array() {
            *target = json!([]);
        }
        let target_arr = target.as_array_mut().expect("ensured array above");
        for entry in new_entries {
            if !target_arr
                .iter()
                .any(|existing_entry| contains_command(existing_entry, entry))
            {
                target_arr.push(entry.clone());
            }
        }
    }

    // Backfill: older installs wrote the UserPromptSubmit hook without a
    // `timeout`. Even when the command already exists (so the loop above
    // skipped it), ensure every trusty-memory hook command has the default
    // timeout so a slow daemon never freezes the Claude Code REPL (issue #63).
    backfill_user_prompt_timeout(hooks_obj);

    merged
}

/// Ensure every installed `trusty-memory hooks fire` command entry carries the
/// default `timeout` value.
///
/// Why: Issue #63 — hooks installed before the timeout fix lack a `timeout`
/// field, so re-running `hooks install` must patch them in place rather than
/// leave the REPL exposed to an indefinite freeze.
/// What: Walks every event array, finds inner `hooks` command objects whose
/// `command` starts with `trusty-memory hooks fire`, and inserts/overwrites
/// `timeout` with `USER_PROMPT_HOOK_TIMEOUT_MS` when it is missing.
/// Test: `merge_claude_settings_backfills_missing_timeout` adds a timeout-less
/// entry and asserts the merge result has the timeout.
fn backfill_user_prompt_timeout(hooks_obj: &mut serde_json::Map<String, Value>) {
    for event_arr in hooks_obj.values_mut() {
        let Some(entries) = event_arr.as_array_mut() else {
            continue;
        };
        for entry in entries.iter_mut() {
            let Some(inner) = entry.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
                continue;
            };
            for cmd in inner.iter_mut() {
                let Some(obj) = cmd.as_object_mut() else {
                    continue;
                };
                let is_trusty = obj
                    .get("command")
                    .and_then(|c| c.as_str())
                    .map(|s| s.starts_with("trusty-memory hooks fire"))
                    .unwrap_or(false);
                if is_trusty && !obj.contains_key("timeout") {
                    obj.insert("timeout".to_string(), json!(USER_PROMPT_HOOK_TIMEOUT_MS));
                }
            }
        }
    }
}

/// Returns true if `existing` already includes the same trusty-memory
/// command(s) as `candidate` (used to suppress duplicate hook installs).
fn contains_command(existing: &Value, candidate: &Value) -> bool {
    let cand_cmds: Vec<&str> = candidate
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.get("command").and_then(|c| c.as_str()))
                .collect()
        })
        .unwrap_or_default();
    if cand_cmds.is_empty() {
        return false;
    }
    let existing_cmds: Vec<&str> = existing
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.get("command").and_then(|c| c.as_str()))
                .collect()
        })
        .unwrap_or_default();
    cand_cmds.iter().all(|c| existing_cmds.contains(c))
}

// ── List ────────────────────────────────────────────────────────────────────

fn handle_list(_out: &OutputConfig) -> Result<()> {
    // Git hook in cwd
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let git_hook_path = cwd.join(".git").join("hooks").join("post-commit");
    let git_installed = git_hook_path.exists()
        && std::fs::read_to_string(&git_hook_path)
            .map(|c| c.contains("trusty-memory"))
            .unwrap_or(false);
    println!(
        "git post-commit ({}): {}",
        git_hook_path.display(),
        if git_installed {
            "installed"
        } else {
            "not installed"
        }
    );

    // Claude Code hooks
    let claude_path = claude_settings_path()?;
    let claude_installed = if claude_path.exists() {
        std::fs::read_to_string(&claude_path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .map(|v| settings_has_trusty_hook(&v))
            .unwrap_or(false)
    } else {
        false
    };
    println!(
        "claude code ({}): {}",
        claude_path.display(),
        if claude_installed {
            "installed"
        } else {
            "not installed"
        }
    );
    Ok(())
}

fn settings_has_trusty_hook(settings: &Value) -> bool {
    let Some(hooks) = settings.get("hooks").and_then(|h| h.as_object()) else {
        return false;
    };
    for arr in hooks.values() {
        let Some(arr) = arr.as_array() else { continue };
        for entry in arr {
            let Some(inner) = entry.get("hooks").and_then(|h| h.as_array()) else {
                continue;
            };
            for cmd in inner {
                if let Some(s) = cmd.get("command").and_then(|c| c.as_str()) {
                    if s.contains("trusty-memory hooks fire") {
                        return true;
                    }
                }
            }
        }
    }
    false
}

// ── Fire ─────────────────────────────────────────────────────────────────────

async fn handle_fire(opts: HooksFireArgs, palace_arg: &str) -> Result<()> {
    // Determine palace: explicit --palace overrides everything; otherwise use
    // the resolved palace from the global flag (already cwd-derived).
    let palace_name = match opts.palace.as_deref() {
        Some(p) => p.to_string(),
        None => {
            // The global resolver may have produced a usable name; fall back to
            // cwd-based derivation if it's empty.
            if !palace_arg.is_empty() {
                palace_arg.to_string()
            } else {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                derive_palace_name(&cwd)
            }
        }
    };

    match opts.event.as_str() {
        "git.post-commit" => fire_git_post_commit(&palace_name).await,
        "claude.stop" => fire_claude_stop(&palace_name).await,
        "claude.post-tool-use" => fire_claude_post_tool_use(&palace_name).await,
        "claude.user-prompt" => fire_claude_user_prompt(&palace_name).await,
        other => Err(anyhow!(
            "unknown hook event: {other} (expected git.post-commit | claude.stop | claude.post-tool-use | claude.user-prompt)"
        )),
    }
}

// ── git.post-commit ──────────────────────────────────────────────────────────

async fn fire_git_post_commit(palace: &str) -> Result<()> {
    let cwd = std::env::current_dir().context("get current directory")?;
    let toplevel = git_toplevel(&cwd).unwrap_or(cwd);
    let info = match read_last_commit(&toplevel) {
        Ok(info) => info,
        Err(e) => {
            // Hooks should never break the user's workflow — log & skip.
            eprintln!("trusty-memory: skipping commit ingest ({e:#})");
            return Ok(());
        }
    };

    let short = info.hash.chars().take(7).collect::<String>();
    let tag_commit = format!("commit:{short}");

    let handle = open_or_create_handle(palace).await?;

    // Idempotency: skip if a drawer with this commit tag already exists.
    let existing = handle.list_drawers(None, Some(tag_commit.clone()), 1);
    if !existing.is_empty() {
        eprintln!("trusty-memory: commit {short} already stored, skipping");
        return Ok(());
    }

    let diff_stat = read_diff_stat(&toplevel).unwrap_or_default();
    let content = format_commit_content(&info.subject, &info.body, &diff_stat);

    let tags = vec![
        "source:git".to_string(),
        "event:commit".to_string(),
        tag_commit,
    ];
    handle
        .remember(content, RoomType::General, tags, 0.6)
        .await
        .context("remember git commit")?;
    eprintln!("✓ Stored commit {short} → palace '{palace}'");
    Ok(())
}

struct CommitInfo {
    hash: String,
    subject: String,
    body: String,
}

fn git_toplevel(cwd: &Path) -> Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .context("run git rev-parse")?;
    if !out.status.success() {
        return Err(anyhow!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        return Err(anyhow!("not a git repository"));
    }
    Ok(PathBuf::from(s))
}

fn read_last_commit(repo: &Path) -> Result<CommitInfo> {
    let out = Command::new("git")
        .args(["log", "-1", "--format=%H%n%s%n%b"])
        .current_dir(repo)
        .output()
        .context("run git log")?;
    if !out.status.success() {
        return Err(anyhow!(
            "git log failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let raw = String::from_utf8_lossy(&out.stdout).to_string();
    let mut lines = raw.lines();
    let hash = lines.next().unwrap_or_default().to_string();
    let subject = lines.next().unwrap_or_default().to_string();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    if hash.is_empty() {
        return Err(anyhow!("empty git log output"));
    }
    Ok(CommitInfo {
        hash,
        subject,
        body,
    })
}

fn read_diff_stat(repo: &Path) -> Result<String> {
    // Try HEAD~1..HEAD; on first commit, fall back to `git show --stat HEAD`.
    let out = Command::new("git")
        .args(["diff", "HEAD~1", "HEAD", "--stat"])
        .current_dir(repo)
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !s.is_empty() {
                return Ok(s);
            }
        }
    }
    let fallback = Command::new("git")
        .args(["show", "--stat", "--format=", "HEAD"])
        .current_dir(repo)
        .output()
        .context("git show --stat fallback")?;
    if !fallback.status.success() {
        return Err(anyhow!("git show --stat failed"));
    }
    Ok(String::from_utf8_lossy(&fallback.stdout).trim().to_string())
}

/// Compose the drawer content for a git commit event.
///
/// Why: Pure formatter so we can unit-test the multi-line shape without git.
/// What: `git commit: {subject}\n\n{body}\n\nChanges: {stat}`. Empty body or
/// stat segments are omitted to keep the drawer tidy.
/// Test: `format_commit_content_*` covers full + minimal forms.
pub fn format_commit_content(subject: &str, body: &str, diff_stat: &str) -> String {
    let mut out = format!("git commit: {subject}");
    if !body.trim().is_empty() {
        out.push_str("\n\n");
        out.push_str(body.trim());
    }
    if !diff_stat.trim().is_empty() {
        out.push_str("\n\nChanges: ");
        out.push_str(diff_stat.trim());
    }
    out
}

// ── claude.stop ──────────────────────────────────────────────────────────────

/// Parsed payload for the Claude Code Stop hook.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ClaudeStopPayload {
    pub session_id: Option<String>,
    pub transcript_path: Option<String>,
}

/// Parse a Claude Code `Stop` hook stdin payload.
///
/// Why: Pure parser keeps the fire path simple and unit-testable.
/// What: Extracts `session_id` and `transcript_path` from the JSON payload.
/// Test: `parse_claude_stop_payload_*` tests cover full + missing forms.
pub fn parse_claude_stop_payload(json_str: &str) -> ClaudeStopPayload {
    let v: Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return ClaudeStopPayload::default(),
    };
    ClaudeStopPayload {
        session_id: v
            .get("session_id")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        transcript_path: v
            .get("transcript_path")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
    }
}

async fn fire_claude_stop(palace: &str) -> Result<()> {
    let payload_str = read_stdin_with_timeout(Duration::from_millis(100)).await;
    let payload = parse_claude_stop_payload(&payload_str);

    let session_id = payload
        .session_id
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let tail = payload
        .transcript_path
        .as_deref()
        .and_then(|p| read_tail(Path::new(p), 2000).ok())
        .unwrap_or_default();

    let mut content = format!("Claude Code session ended: {session_id}");
    if !tail.is_empty() {
        content.push_str("\n\nSummary:\n");
        content.push_str(&tail);
    }

    let handle = open_or_create_handle(palace).await?;
    let tags = vec!["source:claude".to_string(), "event:stop".to_string()];
    handle
        .remember(content, RoomType::General, tags, 0.5)
        .await
        .context("remember claude stop")?;
    eprintln!("✓ Stored claude.stop → palace '{palace}'");
    Ok(())
}

fn read_tail(path: &Path, n: usize) -> Result<String> {
    let raw = std::fs::read_to_string(path).context("read transcript")?;
    if raw.len() <= n {
        return Ok(raw);
    }
    // Slice safely on a char boundary to avoid panicking on multi-byte UTF-8.
    let start = raw.len().saturating_sub(n);
    let mut idx = start;
    while idx < raw.len() && !raw.is_char_boundary(idx) {
        idx += 1;
    }
    Ok(raw[idx..].to_string())
}

// ── claude.post-tool-use ─────────────────────────────────────────────────────

/// Parsed payload for the Claude Code PostToolUse hook.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PostToolUsePayload {
    pub tool_name: Option<String>,
    pub file_path: Option<String>,
}

/// Parse a Claude Code `PostToolUse` hook stdin payload.
///
/// Why: Pure parser; the fire path needs the tool name + file_path only.
/// What: Extracts `tool_name` and `tool_input.file_path` from the JSON.
/// Test: `parse_post_tool_use_payload_*` covers Write/Edit/Bash shapes.
pub fn parse_post_tool_use_payload(json_str: &str) -> PostToolUsePayload {
    let v: Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return PostToolUsePayload::default(),
    };
    PostToolUsePayload {
        tool_name: v
            .get("tool_name")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        file_path: v
            .get("tool_input")
            .and_then(|i| i.get("file_path"))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
    }
}

fn room_for_tool(tool: &str) -> RoomType {
    match tool {
        "Write" | "Edit" => RoomType::Backend,
        _ => RoomType::General,
    }
}

async fn fire_claude_post_tool_use(palace: &str) -> Result<()> {
    let payload_str = read_stdin_with_timeout(Duration::from_millis(100)).await;
    let payload = parse_post_tool_use_payload(&payload_str);

    let tool_name = payload
        .tool_name
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let file_path = payload.file_path.clone().unwrap_or_default();

    let mut content = format!("Tool use: {tool_name}");
    if !file_path.is_empty() {
        content.push_str(&format!("\nFile: {file_path}"));
    }

    let tag_tool = format!("tool:{tool_name}");
    let tags = vec![
        "source:claude".to_string(),
        "event:post-tool-use".to_string(),
        tag_tool.clone(),
    ];

    let handle = open_or_create_handle(palace).await?;

    // Dedup: skip if another drawer with the same tool tag was stored within
    // the last 10s. Tight window prevents bursts (rapid successive Edits on
    // the same file) without silently dropping the majority of tool events
    // during an active session.
    let recent = handle.list_drawers(None, Some(tag_tool), 5);
    let now = chrono::Utc::now();
    let too_recent = recent
        .iter()
        .any(|d| (now - d.created_at).num_seconds() < 10);
    if too_recent {
        return Ok(());
    }

    let room = room_for_tool(&tool_name);
    handle
        .remember(content, room, tags, 0.3)
        .await
        .context("remember claude post-tool-use")?;
    Ok(())
}

// ── claude.user-prompt ───────────────────────────────────────────────────────

/// Parsed payload for the Claude Code UserPromptSubmit hook.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct UserPromptPayload {
    pub prompt: Option<String>,
}

/// Parse a Claude Code `UserPromptSubmit` hook stdin payload.
///
/// Why: Pure parser keeps the fire path simple and unit-testable. Claude Code
/// passes the user's submitted prompt via stdin as JSON; we extract just the
/// `prompt` text used as the recall query.
/// What: Extracts `prompt` from the JSON payload. Falls back to treating the
/// entire stdin string as a raw prompt if JSON parsing fails (defensive — keeps
/// the hook useful even if Claude Code's schema changes).
/// Test: `parse_user_prompt_payload_*` covers JSON, missing field, and raw text.
pub fn parse_user_prompt_payload(json_str: &str) -> UserPromptPayload {
    let trimmed = json_str.trim();
    if trimmed.is_empty() {
        return UserPromptPayload::default();
    }
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return UserPromptPayload {
            prompt: v
                .get("prompt")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string()),
        };
    }
    // Not JSON — treat the raw stdin as the prompt text.
    UserPromptPayload {
        prompt: Some(trimmed.to_string()),
    }
}

/// Format recall results into a markdown block suitable for injection.
///
/// Why: Pure formatter so we can unit-test the shape without a palace.
/// What: Renders a short header plus a bulleted list of recall result content,
/// truncated to a reasonable preview length. Empty input returns an empty
/// string so the caller can skip injection entirely.
/// Test: `format_recall_context_*` covers empty + populated cases.
pub fn format_recall_context(results: &[RecallResult]) -> String {
    if results.is_empty() {
        return String::new();
    }
    let mut out = String::from("Relevant memories from trusty-memory:\n");
    const PREVIEW_BYTE_LIMIT: usize = 400;
    for r in results {
        // Use floor_char_boundary so we never slice in the middle of a
        // multi-byte UTF-8 sequence (emoji, CJK, accented chars). Slicing on
        // a non-boundary byte index would panic.
        let preview_len = if r.drawer.content.len() <= PREVIEW_BYTE_LIMIT {
            r.drawer.content.len()
        } else {
            r.drawer.content.floor_char_boundary(PREVIEW_BYTE_LIMIT)
        };
        let mut preview = r.drawer.content[..preview_len].to_string();
        if r.drawer.content.len() > preview_len {
            preview.push('…');
        }
        // Normalize whitespace so the injection stays a clean bullet.
        preview = preview.replace('\n', " ");
        out.push_str(&format!("- (L{}, {:.2}) {}\n", r.layer, r.score, preview));
    }
    out
}

/// Internal self-termination timeout for the `claude.user-prompt` hook.
///
/// Why: Issue #63 — a defense-in-depth backstop in case Claude Code's external
/// timeout is misconfigured or removed. Set slightly longer than
/// `USER_PROMPT_HOOK_TIMEOUT_MS` so, under normal conditions, Claude Code's
/// timeout fires first and this never triggers.
/// What: 8 seconds. If recall exceeds it, we log a warning and exit silently.
/// Test: covered indirectly — `fire_claude_user_prompt` wraps its work in a
/// `tokio::time::timeout` of this duration.
const USER_PROMPT_INTERNAL_TIMEOUT: Duration = Duration::from_secs(8);

async fn fire_claude_user_prompt(palace: &str) -> Result<()> {
    // Read the user's prompt from stdin (Claude Code passes it as JSON).
    let payload_str = read_stdin_with_timeout(Duration::from_millis(200)).await;
    let payload = parse_user_prompt_payload(&payload_str);
    let Some(prompt) = payload.prompt.filter(|s| !s.trim().is_empty()) else {
        // Nothing to query; exit silently.
        return Ok(());
    };

    // Wrap the recall in an internal timeout so the hook self-terminates if a
    // cold start or DB lock makes the operation hang (issue #63). This is a
    // backstop behind Claude Code's external 5s timeout.
    match tokio::time::timeout(
        USER_PROMPT_INTERNAL_TIMEOUT,
        recall_user_prompt_context(palace, &prompt),
    )
    .await
    {
        Ok(Some(context)) => {
            // Emit the JSON envelope Claude Code consumes to inject context.
            let envelope = json!({ "context": context });
            println!("{envelope}");
        }
        Ok(None) => {
            // No usable context or a recoverable failure — exit silently.
        }
        Err(_) => {
            tracing::warn!(
                timeout_secs = USER_PROMPT_INTERNAL_TIMEOUT.as_secs(),
                "claude.user-prompt hook timed out; skipping memory injection"
            );
        }
    }
    Ok(())
}

/// Run the L2 recall for a user prompt and format injectable context.
///
/// Why: Extracted from `fire_claude_user_prompt` so the recall work can be
/// wrapped in a `tokio::time::timeout` backstop (issue #63).
/// What: Opens the palace, runs an L2 recall (top-5), and formats the results.
/// Returns `None` whenever there is nothing useful to inject or a recoverable
/// error occurred — a hook must never block the user's prompt.
/// Test: exercised end-to-end via the `claude.user-prompt` fire path.
async fn recall_user_prompt_context(palace: &str, prompt: &str) -> Option<String> {
    // Open the palace handle. If anything fails we exit silently — a hook
    // must never block the user's prompt.
    let handle = open_or_create_handle(palace).await.ok()?;

    // L2 recall, top-5. Keep it fast; this runs on every prompt.
    let results = recall_with_default_embedder(&handle, prompt, 5)
        .await
        .ok()?;

    let context = format_recall_context(&results);
    if context.is_empty() {
        None
    } else {
        Some(context)
    }
}

// ── stdin / status helpers ──────────────────────────────────────────────────

/// Read stdin to string with a soft timeout. Returns empty string on timeout
/// or any error — hooks should never break callers because of input issues.
async fn read_stdin_with_timeout(timeout: Duration) -> String {
    let read_fut = tokio::task::spawn_blocking(|| {
        use std::io::Read;
        let mut buf = String::new();
        let _ = std::io::stdin().read_to_string(&mut buf);
        buf
    });
    match tokio::time::timeout(timeout, read_fut).await {
        Ok(Ok(s)) => s,
        _ => String::new(),
    }
}

async fn handle_status(palace: &str, _out: &OutputConfig) -> Result<()> {
    let handle = open_or_create_handle(palace).await?;
    // Fetch git + claude tagged drawers separately, merge, sort by created_at.
    let mut combined = handle.list_drawers(None, Some("source:git".to_string()), 50);
    combined.extend(handle.list_drawers(None, Some("source:claude".to_string()), 50));
    combined.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    combined.truncate(10);

    if combined.is_empty() {
        println!("no hook-sourced drawers in palace '{palace}'");
        return Ok(());
    }

    println!("recent hook-sourced drawers in '{palace}':");
    for d in combined {
        let preview_len = d.content.len().min(40);
        let mut preview = d.content[..preview_len].to_string();
        preview = preview.replace('\n', " ");
        println!(
            "  {}  {}  {}",
            d.created_at.format("%Y-%m-%d %H:%M"),
            d.tags.join(","),
            preview
        );
    }
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn git_hook_script_content_shape() {
        let s = git_hook_script_content();
        assert!(s.starts_with("#!/bin/sh"));
        assert!(s.contains("trusty-memory hooks fire git.post-commit"));
    }

    #[test]
    fn format_commit_content_full() {
        let out = format_commit_content(
            "feat: add hooks",
            "Implements issue #25.",
            " src/main.rs | 2 +-\n 1 file changed",
        );
        assert!(out.starts_with("git commit: feat: add hooks"));
        assert!(out.contains("Implements issue #25."));
        assert!(out.contains("Changes:"));
        assert!(out.contains("src/main.rs"));
    }

    #[test]
    fn format_commit_content_subject_only() {
        let out = format_commit_content("fix: bug", "", "");
        assert_eq!(out, "git commit: fix: bug");
    }

    #[test]
    fn format_commit_content_subject_and_stat_no_body() {
        let out = format_commit_content("fix: bug", "", " a.rs | 1 +");
        assert!(out.contains("git commit: fix: bug"));
        assert!(out.contains("Changes: a.rs | 1 +"));
        assert!(!out.contains("\n\n\n"));
    }

    #[test]
    fn parse_claude_stop_payload_full() {
        let json_str =
            r#"{"session_id":"abc-123","transcript_path":"/tmp/t.jsonl","stop_hook_active":true}"#;
        let p = parse_claude_stop_payload(json_str);
        assert_eq!(p.session_id.as_deref(), Some("abc-123"));
        assert_eq!(p.transcript_path.as_deref(), Some("/tmp/t.jsonl"));
    }

    #[test]
    fn parse_claude_stop_payload_missing_fields() {
        let p = parse_claude_stop_payload("{}");
        assert!(p.session_id.is_none());
        assert!(p.transcript_path.is_none());
    }

    #[test]
    fn parse_claude_stop_payload_invalid_json() {
        let p = parse_claude_stop_payload("not json");
        assert_eq!(p, ClaudeStopPayload::default());
    }

    #[test]
    fn parse_post_tool_use_payload_write() {
        let json_str = r#"{"tool_name":"Write","tool_input":{"file_path":"/tmp/x.rs","content":"fn main(){}"},"tool_response":"ok"}"#;
        let p = parse_post_tool_use_payload(json_str);
        assert_eq!(p.tool_name.as_deref(), Some("Write"));
        assert_eq!(p.file_path.as_deref(), Some("/tmp/x.rs"));
    }

    #[test]
    fn parse_post_tool_use_payload_bash_no_file() {
        let json_str = r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        let p = parse_post_tool_use_payload(json_str);
        assert_eq!(p.tool_name.as_deref(), Some("Bash"));
        assert!(p.file_path.is_none());
    }

    #[test]
    fn parse_post_tool_use_payload_invalid_json() {
        let p = parse_post_tool_use_payload("");
        assert_eq!(p, PostToolUsePayload::default());
    }

    #[test]
    fn merge_claude_settings_empty_existing() {
        let merged = merge_claude_settings(&json!({}), &trusty_hook_entries());
        assert!(merged
            .get("hooks")
            .and_then(|h| h.get("Stop"))
            .and_then(|s| s.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false));
        assert!(merged
            .get("hooks")
            .and_then(|h| h.get("PostToolUse"))
            .and_then(|s| s.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false));
    }

    #[test]
    fn merge_claude_settings_preserves_existing() {
        let existing = json!({
            "model": "sonnet",
            "hooks": {
                "Stop": [
                    {
                        "matcher": "",
                        "hooks": [{"type": "command", "command": "echo other"}]
                    }
                ]
            }
        });
        let merged = merge_claude_settings(&existing, &trusty_hook_entries());
        assert_eq!(merged.get("model").and_then(|v| v.as_str()), Some("sonnet"));
        let stop = merged
            .get("hooks")
            .and_then(|h| h.get("Stop"))
            .and_then(|s| s.as_array())
            .expect("Stop array");
        assert_eq!(stop.len(), 2, "must keep existing Stop entry and add ours");
        // Confirm ours is present.
        let cmds: Vec<&str> = stop
            .iter()
            .filter_map(|e| e.get("hooks").and_then(|h| h.as_array()))
            .flat_map(|a| a.iter())
            .filter_map(|c| c.get("command").and_then(|c| c.as_str()))
            .collect();
        assert!(cmds
            .iter()
            .any(|s| s.contains("trusty-memory hooks fire claude.stop")));
        assert!(cmds.contains(&"echo other"));
    }

    #[test]
    fn merge_claude_settings_no_duplicate() {
        let merged_once = merge_claude_settings(&json!({}), &trusty_hook_entries());
        let merged_twice = merge_claude_settings(&merged_once, &trusty_hook_entries());
        let stop = merged_twice
            .get("hooks")
            .and_then(|h| h.get("Stop"))
            .and_then(|s| s.as_array())
            .expect("Stop array");
        assert_eq!(stop.len(), 1, "duplicate install must not double-add");
        let post = merged_twice
            .get("hooks")
            .and_then(|h| h.get("PostToolUse"))
            .and_then(|s| s.as_array())
            .expect("PostToolUse array");
        assert_eq!(post.len(), 1);
    }

    #[test]
    fn settings_has_trusty_hook_detects_install() {
        let v = trusty_hook_entries();
        assert!(settings_has_trusty_hook(&v));
    }

    #[test]
    fn settings_has_trusty_hook_negative() {
        let v = json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "other"}]}]}});
        assert!(!settings_has_trusty_hook(&v));
    }

    #[test]
    fn parse_user_prompt_payload_json() {
        let p = parse_user_prompt_payload(r#"{"prompt":"how do I add a hook?"}"#);
        assert_eq!(p.prompt.as_deref(), Some("how do I add a hook?"));
    }

    #[test]
    fn parse_user_prompt_payload_missing_field() {
        let p = parse_user_prompt_payload(r#"{"session_id":"abc"}"#);
        assert!(p.prompt.is_none());
    }

    #[test]
    fn parse_user_prompt_payload_raw_text_fallback() {
        let p = parse_user_prompt_payload("plain text query");
        assert_eq!(p.prompt.as_deref(), Some("plain text query"));
    }

    #[test]
    fn parse_user_prompt_payload_empty() {
        let p = parse_user_prompt_payload("   ");
        assert_eq!(p, UserPromptPayload::default());
    }

    #[test]
    fn format_recall_context_empty() {
        assert_eq!(format_recall_context(&[]), "");
    }

    #[test]
    fn format_recall_context_populated() {
        use trusty_memory_core::Drawer;
        use uuid::Uuid;
        let now = chrono::Utc::now();
        let drawer = Drawer {
            id: Uuid::nil(),
            room_id: Uuid::nil(),
            content: "hook installation works via merge_claude_settings".to_string(),
            importance: 0.5,
            source_file: None,
            created_at: now,
            tags: vec![],
            access_count: 0,
            last_accessed_at: Some(now),
        };
        let results = vec![RecallResult {
            drawer,
            score: 0.82,
            layer: 2,
        }];
        let out = format_recall_context(&results);
        assert!(out.starts_with("Relevant memories"));
        assert!(out.contains("L2"));
        assert!(out.contains("0.82"));
        assert!(out.contains("merge_claude_settings"));
    }

    #[test]
    fn format_recall_context_multibyte() {
        // Why: Regression test for panic when content contains multi-byte UTF-8
        // (emoji/CJK/accented) and byte offset 400 falls mid-character.
        // What: Builds content >400 bytes packed with 4-byte emojis, calls
        // format_recall_context, asserts no panic and valid UTF-8 output.
        // Test: This very test — if the slice were unguarded, it would panic.
        use trusty_memory_core::Drawer;
        use uuid::Uuid;
        // Prefix with two ASCII bytes so that the 4-byte emoji sequences
        // straddle byte index 400 (400 - 2 = 398, not divisible by 4).
        // Each "🎉" is 4 bytes; without the prefix, 400 would land on a
        // boundary (4 * 100). With the 2-byte prefix, byte 400 falls in the
        // middle of an emoji — exactly the panic condition we're guarding.
        let mut content = String::from("ab");
        content.push_str(&"🎉".repeat(200));
        assert!(content.len() > 400);
        // Make sure byte 400 is NOT a char boundary so the original bug would fire.
        assert!(!content.is_char_boundary(400));
        let now = chrono::Utc::now();
        let drawer = Drawer {
            id: Uuid::nil(),
            room_id: Uuid::nil(),
            content,
            importance: 0.5,
            source_file: None,
            created_at: now,
            tags: vec![],
            access_count: 0,
            last_accessed_at: Some(now),
        };
        let results = vec![RecallResult {
            drawer,
            score: 0.5,
            layer: 2,
        }];
        let out = format_recall_context(&results);
        // Must not panic; output must be valid UTF-8 (guaranteed by String,
        // but verify it round-trips via as_bytes -> from_utf8) and include
        // the ellipsis indicating truncation occurred.
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        assert!(out.contains('…'));
        assert!(out.starts_with("Relevant memories"));
    }

    #[test]
    fn trusty_hook_entries_includes_user_prompt_submit() {
        let v = trusty_hook_entries();
        let arr = v
            .get("hooks")
            .and_then(|h| h.get("UserPromptSubmit"))
            .and_then(|s| s.as_array())
            .expect("UserPromptSubmit array");
        let cmds: Vec<&str> = arr
            .iter()
            .filter_map(|e| e.get("hooks").and_then(|h| h.as_array()))
            .flat_map(|a| a.iter())
            .filter_map(|c| c.get("command").and_then(|c| c.as_str()))
            .collect();
        assert!(cmds
            .iter()
            .any(|s| s.contains("trusty-memory hooks fire claude.user-prompt")));
    }

    #[test]
    fn trusty_hook_entries_user_prompt_has_timeout() {
        let v = trusty_hook_entries();
        let arr = v
            .get("hooks")
            .and_then(|h| h.get("UserPromptSubmit"))
            .and_then(|s| s.as_array())
            .expect("UserPromptSubmit array");
        let cmd = arr
            .iter()
            .filter_map(|e| e.get("hooks").and_then(|h| h.as_array()))
            .flat_map(|a| a.iter())
            .find(|c| {
                c.get("command")
                    .and_then(|s| s.as_str())
                    .map(|s| s.contains("claude.user-prompt"))
                    .unwrap_or(false)
            })
            .expect("user-prompt command entry");
        assert_eq!(
            cmd.get("timeout").and_then(|t| t.as_u64()),
            Some(USER_PROMPT_HOOK_TIMEOUT_MS),
            "user-prompt hook must carry the default timeout"
        );
    }

    #[test]
    fn merge_claude_settings_backfills_missing_timeout() {
        // Simulate an older install: UserPromptSubmit hook present but with no
        // `timeout` field. Re-running install must patch the timeout in place.
        let existing = json!({
            "hooks": {
                "UserPromptSubmit": [
                    {
                        "matcher": "",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "trusty-memory hooks fire claude.user-prompt"
                            }
                        ]
                    }
                ]
            }
        });
        let merged = merge_claude_settings(&existing, &trusty_hook_entries());
        let arr = merged
            .get("hooks")
            .and_then(|h| h.get("UserPromptSubmit"))
            .and_then(|s| s.as_array())
            .expect("UserPromptSubmit array");
        // No duplicate command should have been appended.
        assert_eq!(arr.len(), 1, "existing entry must not be duplicated");
        let cmd = arr
            .iter()
            .filter_map(|e| e.get("hooks").and_then(|h| h.as_array()))
            .flat_map(|a| a.iter())
            .find(|c| {
                c.get("command")
                    .and_then(|s| s.as_str())
                    .map(|s| s.contains("claude.user-prompt"))
                    .unwrap_or(false)
            })
            .expect("user-prompt command entry");
        assert_eq!(
            cmd.get("timeout").and_then(|t| t.as_u64()),
            Some(USER_PROMPT_HOOK_TIMEOUT_MS),
            "missing timeout must be backfilled on re-install"
        );
    }

    #[test]
    fn merge_claude_settings_preserves_explicit_timeout() {
        // A user who set a custom timeout should keep it — backfill only fills
        // a missing field, never overwrites an existing one.
        let existing = json!({
            "hooks": {
                "UserPromptSubmit": [
                    {
                        "matcher": "",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "trusty-memory hooks fire claude.user-prompt",
                                "timeout": 12000
                            }
                        ]
                    }
                ]
            }
        });
        let merged = merge_claude_settings(&existing, &trusty_hook_entries());
        let cmd = merged
            .get("hooks")
            .and_then(|h| h.get("UserPromptSubmit"))
            .and_then(|s| s.as_array())
            .and_then(|arr| arr.first())
            .and_then(|e| e.get("hooks"))
            .and_then(|h| h.as_array())
            .and_then(|a| a.first())
            .expect("user-prompt command entry");
        assert_eq!(
            cmd.get("timeout").and_then(|t| t.as_u64()),
            Some(12_000),
            "explicit user timeout must be preserved"
        );
    }

    #[test]
    fn room_for_tool_mapping() {
        assert_eq!(room_for_tool("Write"), RoomType::Backend);
        assert_eq!(room_for_tool("Edit"), RoomType::Backend);
        assert_eq!(room_for_tool("Bash"), RoomType::General);
        assert_eq!(room_for_tool("Unknown"), RoomType::General);
    }
}
