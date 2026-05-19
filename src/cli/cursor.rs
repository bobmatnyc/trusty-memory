//! `cursor` subcommand — export palace memories into Cursor editor rules.
//!
//! Why: Issue #65 — Cursor reads project context from `.cursor/rules/*.mdc`
//! files. Teams that store knowledge in a trusty-memory palace want that
//! knowledge surfaced to Cursor without hand-copying it. This command bridges
//! the two by writing the top memories of a palace into a Cursor rules file
//! and keeps it fresh with an optional watch loop.
//! What: Provides `cursor sync` (export top-k memories grouped by room into
//! `.cursor/rules/trusty-memory.mdc`) and `cursor status` (report on an
//! existing rules file). The `.mdc` body generation is a set of pure helpers
//! so it can be unit-tested without disk or a live palace.
//! Test: Unit tests cover the pure helpers — front-matter, body grouping,
//! content truncation, the full document assembler, and the status parser.
//! End-to-end sync against a real palace is exercised manually.

use crate::cli::memory::open_or_create_handle;
use crate::cli::output::OutputConfig;
use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Args, Subcommand};
use std::path::{Path, PathBuf};
use std::time::Duration;
use trusty_memory_core::{Drawer, RoomType};

/// Default number of memories exported by `cursor sync`.
const DEFAULT_TOP_K: usize = 50;

/// Default poll interval (seconds) for `cursor sync --watch` when the flag is
/// supplied without an explicit value.
const DEFAULT_WATCH_SECS: u64 = 60;

/// Maximum number of characters kept for a single memory bullet. Memories
/// longer than this are truncated with an ellipsis so one verbose memory
/// cannot bloat the rules file.
const MAX_BULLET_CHARS: usize = 200;

/// Relative path (under the target directory) of the generated rules file.
const RULES_REL_PATH: &str = ".cursor/rules/trusty-memory.mdc";

/// Marker line that precedes the metadata comment in a generated file. Used by
/// `cursor status` to locate the palace / timestamp / count metadata.
const META_PREFIX: &str = "<!-- Palace:";

/// `cursor` parent subcommand.
///
/// Why: Groups the Cursor-integration verbs under one namespace, matching how
/// other multi-verb features (`palace`, `kg`, `git`) are organized.
/// What: Two children — `sync` and `status`.
/// Test: Parse coverage via clap `--help`; behavior tested in this module.
#[derive(Subcommand, Debug, Clone)]
pub enum CursorSubcommand {
    /// Export the top memories of a palace into `.cursor/rules/trusty-memory.mdc`.
    #[command(
        after_help = "Examples:\n  trusty-memory cursor sync --palace my-app\n  trusty-memory cursor sync --palace my-app --top-k 100\n  trusty-memory cursor sync --palace my-app --dry-run\n  trusty-memory cursor sync --palace my-app --watch 30"
    )]
    Sync(SyncArgs),

    /// Report on an existing `.cursor/rules/trusty-memory.mdc` file.
    #[command(
        after_help = "Examples:\n  trusty-memory cursor status\n  trusty-memory cursor status --dir /path/to/project"
    )]
    Status(StatusArgs),
}

/// Args for `cursor sync`.
#[derive(Args, Debug, Clone)]
pub struct SyncArgs {
    /// Palace whose memories are exported.
    #[arg(long)]
    pub palace: String,

    /// Target directory containing the project. Defaults to the current
    /// working directory. The rules file is written under `<dir>/.cursor/rules/`.
    #[arg(long)]
    pub dir: Option<PathBuf>,

    /// Number of memories to export, taken by importance descending.
    #[arg(long, default_value_t = DEFAULT_TOP_K)]
    pub top_k: usize,

    /// Poll every N seconds and re-sync when the memory count changes. When
    /// the flag is given without a value, defaults to 60 seconds. Ctrl-C stops.
    #[arg(long, value_name = "SECONDS", num_args = 0..=1, default_missing_value = "60")]
    pub watch: Option<u64>,

    /// Print the would-be file contents to stdout without touching disk.
    #[arg(long)]
    pub dry_run: bool,
}

/// Args for `cursor status`.
#[derive(Args, Debug, Clone)]
pub struct StatusArgs {
    /// Target directory to inspect. Defaults to the current working directory.
    #[arg(long)]
    pub dir: Option<PathBuf>,
}

// ── Public entry point ───────────────────────────────────────────────────────

/// Dispatch a `cursor` subcommand.
///
/// Why: Keeps `main.rs` declarative — it parses the subcommand and hands off.
/// What: Routes to the `sync` or `status` handler.
/// Test: Integration coverage via `trusty-memory cursor --help`.
pub async fn handle(command: CursorSubcommand, out: &OutputConfig) -> Result<()> {
    match command {
        CursorSubcommand::Sync(args) => sync(args, out).await,
        CursorSubcommand::Status(args) => status(args, out),
    }
}

// ── `cursor sync` ─────────────────────────────────────────────────────────────

/// Run `cursor sync`, optionally in a watch loop.
///
/// Why: Acceptance criteria for issue #65 — export palace memories into a
/// Cursor rules file, with dry-run and watch modes.
/// What: Resolves the target directory, then either performs a single sync or
/// (when `--watch` is set) loops, re-syncing whenever the palace memory count
/// changes. `--dry-run` prints the document and touches nothing.
/// Test: The pure document builder it calls is unit-tested; the orchestration
/// is exercised manually.
async fn sync(args: SyncArgs, out: &OutputConfig) -> Result<()> {
    let dir = resolve_dir(args.dir.as_deref())?;
    out.print_header("cursor sync", &args.palace);

    match args.watch {
        None => {
            sync_once(&args.palace, &dir, args.top_k, args.dry_run, out).await?;
        }
        Some(secs) => {
            let interval = if secs == 0 { DEFAULT_WATCH_SECS } else { secs };
            run_watch(&args.palace, &dir, args.top_k, args.dry_run, interval, out).await?;
        }
    }
    Ok(())
}

/// Perform one sync pass and return the number of memories exported.
///
/// Why: Shared by both the single-shot path and each iteration of the watch
/// loop, so the export logic lives in exactly one place.
/// What: Loads the palace memories, builds the `.mdc` document, then either
/// prints it (`dry_run`) or writes it to `<dir>/.cursor/rules/trusty-memory.mdc`,
/// creating the directory if needed.
/// Test: Behavioral — the document builder is unit-tested via `build_document`.
async fn sync_once(
    palace: &str,
    dir: &Path,
    top_k: usize,
    dry_run: bool,
    out: &OutputConfig,
) -> Result<usize> {
    let drawers = load_memories(palace, top_k).await?;
    let synced_at = Utc::now().to_rfc3339();
    let document = build_document(palace, &synced_at, &drawers);

    if dry_run {
        println!("{document}");
        out.print_success(&format!(
            "[dry-run] {} memories (not written)",
            drawers.len()
        ));
        return Ok(drawers.len());
    }

    let path = dir.join(RULES_REL_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create cursor rules directory {}", parent.display()))?;
    }
    std::fs::write(&path, &document).with_context(|| format!("write {}", path.display()))?;
    out.print_success(&format!(
        "synced {} memories → {}",
        drawers.len(),
        path.display()
    ));
    Ok(drawers.len())
}

/// Run the `--watch` poll loop until interrupted.
///
/// Why: Teams want their Cursor rules to stay current as memories accumulate
/// without re-running the command by hand.
/// What: Performs an initial sync, then sleeps `interval` seconds between
/// passes, re-syncing whenever the palace memory count differs from the last
/// pass. Ctrl-C ends the loop cleanly.
/// Test: Behavioral — exercised manually since it depends on wall-clock time.
async fn run_watch(
    palace: &str,
    dir: &Path,
    top_k: usize,
    dry_run: bool,
    interval: u64,
    out: &OutputConfig,
) -> Result<()> {
    println!("watching palace '{palace}' every {interval}s (Ctrl-C to stop)");
    let mut last_count = sync_once(palace, dir, top_k, dry_run, out).await?;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nstopped watching");
                return Ok(());
            }
            _ = tokio::time::sleep(Duration::from_secs(interval)) => {
                let count = current_memory_count(palace, top_k).await?;
                if count != last_count {
                    last_count = sync_once(palace, dir, top_k, dry_run, out).await?;
                } else {
                    tracing::debug!(palace, count, "no change, skipping re-sync");
                }
            }
        }
    }
}

/// Count the memories that would be exported on the next sync.
///
/// Why: The watch loop only re-writes the file when the export set changes;
/// comparing counts is a cheap, reliable change signal.
/// What: Loads the same drawer set `sync_once` would and returns its length.
/// Test: Behavioral — covered indirectly by the watch path.
async fn current_memory_count(palace: &str, top_k: usize) -> Result<usize> {
    Ok(load_memories(palace, top_k).await?.len())
}

/// Load the top-`top_k` memories of a palace, ranked by importance descending.
///
/// Why: `cursor sync` exports the most important memories; Drawer `room_id` is
/// a one-way hash, so the only way to recover a memory's room is to query each
/// stock `RoomType` separately and tag the results.
/// What: Opens (or creates) the palace handle, queries every stock `RoomType`
/// for its drawers, merges the lists, sorts by importance descending, and
/// truncates to `top_k`.
/// Test: Behavioral — the grouping/formatting it feeds is unit-tested.
async fn load_memories(palace: &str, top_k: usize) -> Result<Vec<RoomMemory>> {
    let handle = open_or_create_handle(palace).await?;
    let mut merged: Vec<RoomMemory> = Vec::new();
    for room in stock_room_types() {
        let drawers = handle.list_drawers(Some(room.clone()), None, top_k);
        for drawer in drawers {
            merged.push(RoomMemory {
                room: room.clone(),
                drawer,
            });
        }
    }
    merged.sort_by(|a, b| {
        b.drawer
            .importance
            .partial_cmp(&a.drawer.importance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged.truncate(top_k);
    Ok(merged)
}

// ── `cursor status` ───────────────────────────────────────────────────────────

/// Run `cursor status`.
///
/// Why: Acceptance criteria for issue #65 — report whether a Cursor rules file
/// exists and, if so, summarize the last sync.
/// What: Reads `<dir>/.cursor/rules/trusty-memory.mdc`; when present, parses the
/// metadata comment for palace id, synced timestamp, and memory count and
/// prints them; when absent, prints a clear "not found" message.
/// Test: The metadata parser `parse_metadata` is unit-tested.
fn status(args: StatusArgs, out: &OutputConfig) -> Result<()> {
    let dir = resolve_dir(args.dir.as_deref())?;
    let path = dir.join(RULES_REL_PATH);

    if !path.is_file() {
        if out.json {
            out.print_json(&serde_json::json!({
                "exists": false,
                "path": path.display().to_string(),
            }));
        } else {
            println!("No cursor rules file found at {}", path.display());
        }
        return Ok(());
    }

    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let meta = parse_metadata(&raw);

    if out.json {
        out.print_json(&serde_json::json!({
            "exists": true,
            "path": path.display().to_string(),
            "palace": meta.palace,
            "synced": meta.synced,
            "memories": meta.memories,
        }));
    } else {
        println!("cursor rules file: {}", path.display());
        println!(
            "  palace:   {}",
            meta.palace.as_deref().unwrap_or("unknown")
        );
        println!(
            "  synced:   {}",
            meta.synced.as_deref().unwrap_or("unknown")
        );
        match meta.memories {
            Some(n) => println!("  memories: {n}"),
            None => println!("  memories: unknown"),
        }
    }
    Ok(())
}

// ── Pure helpers (unit-tested) ────────────────────────────────────────────────

/// A memory paired with the room it was found in.
///
/// Why: `Drawer` stores only a hashed `room_id`; to group memories by room in
/// the exported document we must remember which `RoomType` query surfaced each
/// drawer.
/// What: A drawer plus its originating `RoomType`.
/// Test: Used by `build_document` tests.
#[derive(Debug, Clone)]
pub struct RoomMemory {
    /// Room the drawer was retrieved from.
    pub room: RoomType,
    /// The memory itself.
    pub drawer: Drawer,
}

/// Parsed metadata from a generated rules file's comment line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CursorMetadata {
    /// Palace id the file was generated from, if parseable.
    pub palace: Option<String>,
    /// RFC3339 sync timestamp, if parseable.
    pub synced: Option<String>,
    /// Memory count recorded at sync time, if parseable.
    pub memories: Option<usize>,
}

/// Every stock (non-`Custom`) `RoomType`, in display order.
///
/// Why: Drawers carry a hashed `room_id`, so the export must probe each known
/// room explicitly; a single ordered list keeps the query loop and the
/// document section order in agreement.
/// What: The nine stock `RoomType` variants.
/// Test: `stock_room_types_has_nine` asserts the count.
pub fn stock_room_types() -> Vec<RoomType> {
    vec![
        RoomType::Backend,
        RoomType::Frontend,
        RoomType::Testing,
        RoomType::Planning,
        RoomType::Documentation,
        RoomType::Research,
        RoomType::Configuration,
        RoomType::Meetings,
        RoomType::General,
    ]
}

/// Human-readable label for a `RoomType`, used as the `## <heading>` in the
/// exported document.
///
/// Why: `Custom` variants need their inner string surfaced; stock variants
/// need a stable title-case name.
/// What: Returns the variant name, or the inner string for `Custom`.
/// Test: `room_label_*` cover stock and custom variants.
pub fn room_label(room: &RoomType) -> String {
    match room {
        RoomType::Frontend => "Frontend".to_string(),
        RoomType::Backend => "Backend".to_string(),
        RoomType::Testing => "Testing".to_string(),
        RoomType::Planning => "Planning".to_string(),
        RoomType::Documentation => "Documentation".to_string(),
        RoomType::Research => "Research".to_string(),
        RoomType::Configuration => "Configuration".to_string(),
        RoomType::Meetings => "Meetings".to_string(),
        RoomType::General => "General".to_string(),
        RoomType::Custom(name) => name.clone(),
    }
}

/// Truncate a memory's content to fit a single bullet.
///
/// Why: One verbose memory should not bloat the rules file or overwhelm
/// Cursor's context window.
/// What: Returns the content unchanged when within `MAX_BULLET_CHARS`;
/// otherwise truncates on a character boundary and appends an ellipsis.
/// Newlines are collapsed to spaces so each memory stays on one bullet line.
/// Test: `truncate_content_*` cover short, long, and multiline inputs.
pub fn truncate_content(content: &str) -> String {
    let single_line: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= MAX_BULLET_CHARS {
        return single_line;
    }
    let truncated: String = single_line.chars().take(MAX_BULLET_CHARS).collect();
    format!("{truncated}…")
}

/// Build the YAML front-matter block for the rules file.
///
/// Why: Cursor requires `.mdc` files to begin with a front-matter block that
/// declares description, globs, and the `alwaysApply` flag.
/// What: Returns the `---`-delimited front-matter naming the source palace.
/// Test: `front_matter_contains_palace`.
pub fn build_front_matter(palace: &str) -> String {
    format!(
        "---\ndescription: trusty-memory context for {palace}\nglobs: [\"**/*\"]\nalwaysApply: true\n---\n"
    )
}

/// Build the body of the rules file: memories grouped by room.
///
/// Why: Cursor surfaces the rules file as project context; grouping by room
/// keeps related memories together and mirrors the palace's own structure.
/// What: For each stock room (in order) that has memories, emits a `## <Room>`
/// heading followed by one truncated `- <content>` bullet per memory. Rooms
/// with no memories are omitted. Returns an empty-state note when there are
/// no memories at all.
/// Test: `build_body_*` cover grouping, ordering, empty input, and truncation.
pub fn build_body(memories: &[RoomMemory]) -> String {
    if memories.is_empty() {
        return "_No memories stored in this palace yet._\n".to_string();
    }

    let mut body = String::new();
    for room in stock_room_types() {
        let in_room: Vec<&RoomMemory> = memories.iter().filter(|m| m.room == room).collect();
        if in_room.is_empty() {
            continue;
        }
        body.push_str(&format!("## {}\n", room_label(&room)));
        for m in in_room {
            body.push_str(&format!("- {}\n", truncate_content(&m.drawer.content)));
        }
        body.push('\n');
    }

    // Surface any Custom-room memories that slipped through (defensive — the
    // current loader only probes stock rooms, but a future loader may not).
    let custom: Vec<&RoomMemory> = memories
        .iter()
        .filter(|m| matches!(m.room, RoomType::Custom(_)))
        .collect();
    if !custom.is_empty() {
        let mut by_label: std::collections::BTreeMap<String, Vec<&RoomMemory>> =
            std::collections::BTreeMap::new();
        for m in custom {
            by_label.entry(room_label(&m.room)).or_default().push(m);
        }
        for (label, items) in by_label {
            body.push_str(&format!("## {label}\n"));
            for m in items {
                body.push_str(&format!("- {}\n", truncate_content(&m.drawer.content)));
            }
            body.push('\n');
        }
    }

    body
}

/// Assemble the complete `.mdc` document.
///
/// Why: One pure function produces the entire file so `cursor sync` (write and
/// dry-run) and the unit tests all share identical output.
/// What: Concatenates the front-matter, the auto-generated warning + metadata
/// comment, and the room-grouped body.
/// Test: `build_document_*` assert front-matter, metadata, and body presence.
pub fn build_document(palace: &str, synced_at: &str, memories: &[RoomMemory]) -> String {
    let front = build_front_matter(palace);
    let body = build_body(memories);
    format!(
        "{front}\n<!-- Auto-generated by trusty-memory cursor sync. Do not edit manually. -->\n\
         {META_PREFIX} {palace} | Synced: {synced_at} | Memories: {} -->\n\n{body}",
        memories.len()
    )
}

/// Parse the metadata comment from a generated rules file.
///
/// Why: `cursor status` reports the palace id, sync time, and memory count
/// without re-deriving them; they are recorded in the file's comment line.
/// What: Scans for the `<!-- Palace: ... -->` line and extracts the three
/// pipe-separated fields. Missing or malformed fields yield `None` so the
/// status command degrades gracefully on a hand-edited file.
/// Test: `parse_metadata_*` cover well-formed, partial, and absent input.
pub fn parse_metadata(content: &str) -> CursorMetadata {
    let Some(line) = content
        .lines()
        .find(|l| l.trim_start().starts_with(META_PREFIX))
    else {
        return CursorMetadata::default();
    };
    // Strip the comment markers, then split on '|'.
    let inner = line
        .trim()
        .trim_start_matches("<!--")
        .trim_end_matches("-->")
        .trim();

    let mut meta = CursorMetadata::default();
    for field in inner.split('|') {
        let field = field.trim();
        if let Some(v) = field.strip_prefix("Palace:") {
            meta.palace = Some(v.trim().to_string());
        } else if let Some(v) = field.strip_prefix("Synced:") {
            meta.synced = Some(v.trim().to_string());
        } else if let Some(v) = field.strip_prefix("Memories:") {
            meta.memories = v.trim().parse().ok();
        }
    }
    meta
}

/// Resolve the target directory, defaulting to the current working directory.
///
/// Why: Both `sync` and `status` accept an optional `--dir`; centralizing the
/// default keeps the two in sync.
/// What: Returns `dir` when supplied, else `std::env::current_dir()`.
/// Test: Behavioral — trivial passthrough.
fn resolve_dir(dir: Option<&Path>) -> Result<PathBuf> {
    match dir {
        Some(d) => Ok(d.to_path_buf()),
        None => std::env::current_dir().context("resolve current directory"),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn drawer(content: &str, importance: f32) -> Drawer {
        Drawer {
            id: Uuid::new_v4(),
            room_id: Uuid::nil(),
            content: content.to_string(),
            importance,
            source_file: None,
            created_at: Utc::now(),
            tags: Vec::new(),
            last_accessed_at: None,
            access_count: 0,
        }
    }

    fn mem(room: RoomType, content: &str, importance: f32) -> RoomMemory {
        RoomMemory {
            room,
            drawer: drawer(content, importance),
        }
    }

    #[test]
    fn stock_room_types_has_nine() {
        assert_eq!(stock_room_types().len(), 9);
        // No Custom variant leaks into the stock list.
        assert!(!stock_room_types()
            .iter()
            .any(|r| matches!(r, RoomType::Custom(_))));
    }

    #[test]
    fn room_label_stock_and_custom() {
        assert_eq!(room_label(&RoomType::Backend), "Backend");
        assert_eq!(room_label(&RoomType::General), "General");
        assert_eq!(room_label(&RoomType::Custom("Ops".to_string())), "Ops");
    }

    #[test]
    fn truncate_content_short_unchanged() {
        assert_eq!(truncate_content("hello world"), "hello world");
    }

    #[test]
    fn truncate_content_long_is_clipped() {
        let long = "x".repeat(MAX_BULLET_CHARS + 50);
        let out = truncate_content(&long);
        // MAX_BULLET_CHARS chars + the ellipsis.
        assert_eq!(out.chars().count(), MAX_BULLET_CHARS + 1);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_content_collapses_whitespace() {
        assert_eq!(
            truncate_content("line one\n  line two\t  end"),
            "line one line two end"
        );
    }

    #[test]
    fn front_matter_contains_palace() {
        let fm = build_front_matter("my-app");
        assert!(fm.starts_with("---\n"));
        assert!(fm.contains("description: trusty-memory context for my-app"));
        assert!(fm.contains("alwaysApply: true"));
        assert!(fm.contains("globs: [\"**/*\"]"));
    }

    #[test]
    fn build_body_empty_is_placeholder() {
        let body = build_body(&[]);
        assert!(body.contains("No memories stored"));
    }

    #[test]
    fn build_body_groups_by_room_in_order() {
        let memories = vec![
            mem(RoomType::General, "general fact", 0.5),
            mem(RoomType::Backend, "backend fact", 0.9),
            mem(RoomType::Testing, "testing fact", 0.7),
        ];
        let body = build_body(&memories);
        assert!(body.contains("## Backend\n- backend fact"));
        assert!(body.contains("## Testing\n- testing fact"));
        assert!(body.contains("## General\n- general fact"));
        // Backend section precedes Testing precedes General.
        let backend = body.find("## Backend").expect("backend section");
        let testing = body.find("## Testing").expect("testing section");
        let general = body.find("## General").expect("general section");
        assert!(backend < testing && testing < general);
    }

    #[test]
    fn build_body_omits_empty_rooms() {
        let memories = vec![mem(RoomType::Backend, "only backend", 0.5)];
        let body = build_body(&memories);
        assert!(body.contains("## Backend"));
        assert!(!body.contains("## Frontend"));
        assert!(!body.contains("## Testing"));
    }

    #[test]
    fn build_body_surfaces_custom_rooms() {
        let memories = vec![mem(RoomType::Custom("Ops".to_string()), "ops runbook", 0.5)];
        let body = build_body(&memories);
        assert!(body.contains("## Ops\n- ops runbook"));
    }

    #[test]
    fn build_body_truncates_long_memory() {
        let long = "z".repeat(MAX_BULLET_CHARS + 100);
        let memories = vec![mem(RoomType::Backend, &long, 0.5)];
        let body = build_body(&memories);
        assert!(body.contains('…'), "long memory is truncated in the body");
    }

    #[test]
    fn build_document_has_front_matter_metadata_and_body() {
        let memories = vec![
            mem(RoomType::Backend, "backend fact", 0.9),
            mem(RoomType::General, "general fact", 0.5),
        ];
        let doc = build_document("my-app", "2026-05-19T12:00:00+00:00", &memories);
        assert!(doc.starts_with("---\n"));
        assert!(doc.contains("Auto-generated by trusty-memory cursor sync"));
        assert!(doc.contains("Palace: my-app"));
        assert!(doc.contains("Synced: 2026-05-19T12:00:00+00:00"));
        assert!(doc.contains("Memories: 2"));
        assert!(doc.contains("## Backend\n- backend fact"));
    }

    #[test]
    fn build_document_empty_palace() {
        let doc = build_document("empty", "2026-05-19T00:00:00+00:00", &[]);
        assert!(doc.contains("Memories: 0"));
        assert!(doc.contains("No memories stored"));
    }

    #[test]
    fn parse_metadata_well_formed() {
        let doc = build_document(
            "trusty-memory",
            "2026-05-19T12:00:00+00:00",
            &[mem(RoomType::Backend, "x", 0.5)],
        );
        let meta = parse_metadata(&doc);
        assert_eq!(meta.palace.as_deref(), Some("trusty-memory"));
        assert_eq!(meta.synced.as_deref(), Some("2026-05-19T12:00:00+00:00"));
        assert_eq!(meta.memories, Some(1));
    }

    #[test]
    fn parse_metadata_absent_yields_default() {
        let meta = parse_metadata("just some text\nno metadata here\n");
        assert_eq!(meta, CursorMetadata::default());
        assert!(meta.palace.is_none());
        assert!(meta.memories.is_none());
    }

    #[test]
    fn parse_metadata_malformed_count_is_none() {
        let line = "<!-- Palace: app | Synced: 2026-05-19 | Memories: not-a-number -->";
        let meta = parse_metadata(line);
        assert_eq!(meta.palace.as_deref(), Some("app"));
        assert_eq!(meta.synced.as_deref(), Some("2026-05-19"));
        assert_eq!(meta.memories, None);
    }

    #[test]
    fn parse_metadata_roundtrips_document() {
        let memories = vec![
            mem(RoomType::Backend, "a", 0.9),
            mem(RoomType::Testing, "b", 0.7),
            mem(RoomType::General, "c", 0.5),
        ];
        let doc = build_document("round-trip", "2026-01-01T00:00:00+00:00", &memories);
        let meta = parse_metadata(&doc);
        assert_eq!(meta.palace.as_deref(), Some("round-trip"));
        assert_eq!(meta.memories, Some(3));
    }
}
