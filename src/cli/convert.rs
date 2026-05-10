//! `convert` subcommand handler — migrate kuzu-memory + mempalace into palaces.
//!
//! Why: Users adopting trusty-memory typically have prior history in
//! kuzu-memory's per-project KuzuDB (`<repo>/.kuzu-memory/memories.db`) and/or
//! mempalace's machine-wide WAL (`~/.mempalace/wal/write_log.jsonl`). The
//! `convert` command pulls those memories into trusty-memory palaces so no
//! knowledge is lost when switching tools.
//! What: Reads kuzu memories by shelling out to the `kuzu` CLI (no C++ dep)
//! and parses the mempalace WAL JSONL directly. Maps source-specific types
//! onto `RoomType`, deduplicates by content hash, and writes drawers via
//! `PalaceHandle::remember`. Supports per-project and machine-wide scopes
//! plus a `--dry-run` mode.
//! Test: Unit tests cover the pure mapping/parsing helpers (kuzu room-type
//! mapping, WAL line parsing, palace-name derivation). End-to-end migration
//! is exercised manually because it depends on external tooling and data.

use crate::cli::memory::open_or_create_handle;
use crate::cli::{ConvertArgs, ConvertScope, ConvertSource};
use anyhow::{Context, Result};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use trusty_memory_core::RoomType;

/// A normalized memory record collected from any source before write.
///
/// Why: Decouples the source-specific parsing (kuzu CSV, mempalace JSONL)
/// from the palace write path so dedup and dry-run logic operate on a single
/// uniform shape.
/// What: Holds the verbatim content, importance in [0,1], target room, and
/// a free-form source label for reporting.
/// Test: Constructed in `parse_wal_line` and `read_kuzu_memories` paths.
#[derive(Debug, Clone)]
pub struct RawMemory {
    pub content: String,
    pub importance: f32,
    pub room: RoomType,
    pub source: String,
}

/// Top-level dispatcher for `trusty-memory convert ...`.
pub async fn handle_convert(args: ConvertArgs) -> Result<()> {
    match args.scope {
        ConvertScope::Project => convert_project(&args).await,
        ConvertScope::All => convert_all(&args).await,
    }
}

/// Convert memories for the current project (or `--palace` override) into one
/// palace.
async fn convert_project(args: &ConvertArgs) -> Result<()> {
    let palace_name = match &args.palace {
        Some(p) => p.clone(),
        None => {
            let cwd = std::env::current_dir().context("get current dir")?;
            derive_palace_name(&cwd)
        }
    };

    let project_root = std::env::current_dir().context("get current dir")?;
    let memories = collect_memories(&args.source, Some(&project_root))?;

    print_plan(&palace_name, &memories, args.dry_run);

    if !args.dry_run && !memories.is_empty() {
        write_to_palace(&palace_name, &memories).await?;
        println!(
            "✓ Converted {} memories → palace '{}'",
            memories.len(),
            palace_name
        );
    } else if memories.is_empty() {
        println!("(nothing to convert)");
    }
    Ok(())
}

/// Convert memories across every discoverable project on this machine.
async fn convert_all(args: &ConvertArgs) -> Result<()> {
    // Mempalace is machine-wide → import once into a single palace.
    if args.source == ConvertSource::All || args.source == ConvertSource::Mempalace {
        match read_mempalace_memories() {
            Ok(mems) if !mems.is_empty() => {
                let palace = "mempalace-archive".to_string();
                print_plan(&palace, &mems, args.dry_run);
                if !args.dry_run {
                    write_to_palace(&palace, &mems).await?;
                    println!("✓ Converted {} memories → palace '{}'", mems.len(), palace);
                }
            }
            Ok(_) => {}
            Err(e) => eprintln!("warning: mempalace read failed: {e:#}"),
        }
    }

    // Kuzu is per-project → discover every project under common roots.
    if args.source == ConvertSource::All || args.source == ConvertSource::Kuzu {
        let projects = discover_kuzu_projects();
        if projects.is_empty() {
            println!("(no kuzu-memory projects found under standard project roots)");
        }
        for proj in projects {
            let palace_name = derive_palace_name(&proj);
            let db_path = proj.join(".kuzu-memory").join("memories.db");
            let mems = match read_kuzu_memories(&db_path) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("warning: kuzu read failed for {}: {e:#}", proj.display());
                    continue;
                }
            };
            if mems.is_empty() {
                continue;
            }
            print_plan(&palace_name, &mems, args.dry_run);
            if !args.dry_run {
                write_to_palace(&palace_name, &mems).await?;
                println!(
                    "✓ Converted {} memories → palace '{}'",
                    mems.len(),
                    palace_name
                );
            }
        }
    }
    Ok(())
}

/// Pull memories from the requested sources for a single project root.
fn collect_memories(source: &ConvertSource, project_root: Option<&Path>) -> Result<Vec<RawMemory>> {
    let mut collected: Vec<RawMemory> = Vec::new();
    let mut seen: HashSet<u64> = HashSet::new();

    let want_kuzu = matches!(source, ConvertSource::Kuzu | ConvertSource::All);
    let want_mempalace = matches!(source, ConvertSource::Mempalace | ConvertSource::All);

    if want_kuzu {
        if let Some(root) = project_root {
            let db = root.join(".kuzu-memory").join("memories.db");
            if db.exists() {
                match read_kuzu_memories(&db) {
                    Ok(mems) => extend_dedup(&mut collected, &mut seen, mems),
                    Err(e) => eprintln!("warning: kuzu read failed: {e:#}"),
                }
            }
        }
    }

    if want_mempalace {
        match read_mempalace_memories() {
            Ok(mems) => extend_dedup(&mut collected, &mut seen, mems),
            Err(e) => eprintln!("warning: mempalace read failed: {e:#}"),
        }
    }

    Ok(collected)
}

fn extend_dedup(target: &mut Vec<RawMemory>, seen: &mut HashSet<u64>, src: Vec<RawMemory>) {
    for m in src {
        let h = content_hash(&m.content);
        if seen.insert(h) {
            target.push(m);
        }
    }
}

fn content_hash(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

fn print_plan(palace: &str, mems: &[RawMemory], dry_run: bool) {
    let prefix = if dry_run { "[dry-run] " } else { "" };
    println!(
        "{prefix}Plan: {} memories → palace '{}'",
        mems.len(),
        palace
    );
    let mut by_source = std::collections::BTreeMap::<String, usize>::new();
    for m in mems {
        *by_source.entry(m.source.clone()).or_insert(0) += 1;
    }
    for (src, n) in by_source {
        println!("  - {src}: {n}");
    }
}

async fn write_to_palace(palace: &str, mems: &[RawMemory]) -> Result<()> {
    let handle = open_or_create_handle(palace).await?;
    for m in mems {
        let tags = vec![format!("source:{}", m.source)];
        if let Err(e) = handle
            .remember(m.content.clone(), m.room.clone(), tags, m.importance)
            .await
        {
            eprintln!("warning: failed to store memory: {e:#}");
        }
    }
    Ok(())
}

// ── Source readers ───────────────────────────────────────────────────────────

/// Read memories from a kuzu-memory `memories.db` by shelling out to the
/// `kuzu` CLI.
///
/// Why: Adding the `kuzu` C++ library as a Rust dependency is heavyweight;
/// the CLI gets us 95% of the benefit at 0% of the build cost.
/// What: Runs a Cypher query in CSV mode and parses the rows. If the CLI is
/// missing, returns Ok(empty) with a warning so machine-wide conversion
/// degrades gracefully.
/// Test: Behavior covered manually since it requires the kuzu binary; pure
/// helpers (room mapping) are unit-tested.
pub fn read_kuzu_memories(db_path: &Path) -> Result<Vec<RawMemory>> {
    if !db_path.exists() {
        return Ok(Vec::new());
    }
    let output = match std::process::Command::new("kuzu")
        .arg(db_path)
        .args(["--mode", "csv", "--no-progress"])
        .args(["-c", "MATCH (m:Memory) RETURN m.id, m.content, m.importance, m.memory_type, m.created_at LIMIT 10000"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!(
                "warning: `kuzu` CLI not found ({e}); skipping kuzu source. \
                 Install with `pip install kuzu` or via your package manager."
            );
            return Ok(Vec::new());
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "warning: kuzu query failed for {}: {stderr}",
            db_path.display()
        );
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    // First non-empty line is typically the header; skip it.
    let mut lines = stdout.lines().filter(|l| !l.trim().is_empty());
    let _header = lines.next();
    for line in lines {
        if let Some(mem) = parse_kuzu_csv_row(line) {
            out.push(mem);
        }
    }
    Ok(out)
}

/// Parse one CSV row from `kuzu` CLI output into a `RawMemory`.
fn parse_kuzu_csv_row(line: &str) -> Option<RawMemory> {
    let fields = parse_csv_line(line);
    // Expect: id, content, importance, memory_type, created_at
    if fields.len() < 4 {
        return None;
    }
    let content = fields.get(1)?.clone();
    if content.trim().is_empty() {
        return None;
    }
    let importance = fields
        .get(2)
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.5) as f32;
    let importance = importance.clamp(0.0, 1.0);
    let memory_type = fields.get(3).cloned().unwrap_or_default();
    let room = map_kuzu_room_type(&memory_type);

    Some(RawMemory {
        content,
        importance,
        room,
        source: "kuzu".to_string(),
    })
}

/// Minimal CSV row parser handling double-quoted fields with `""` escapes.
fn parse_csv_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Map a kuzu `memory_type` string to a `RoomType`.
///
/// Why: kuzu-memory's free-form types aren't useful in trusty-memory's spatial
/// model; collapsing them onto stock rooms keeps closet clustering tidy.
/// What: SEMANTIC→Research, PROCEDURAL→Backend, EPISODIC/WORKING/SENSORY→General.
/// Test: `map_kuzu_room_type_*` unit tests cover every documented variant.
pub fn map_kuzu_room_type(kind: &str) -> RoomType {
    match kind.trim().to_uppercase().as_str() {
        "SEMANTIC" => RoomType::Research,
        "PROCEDURAL" => RoomType::Backend,
        "EPISODIC" | "WORKING" | "SENSORY" => RoomType::General,
        _ => RoomType::General,
    }
}

/// Read memories from the mempalace WAL (`~/.mempalace/wal/write_log.jsonl`).
///
/// Why: mempalace is a machine-wide store; the WAL is the durable source of
/// truth and is line-delimited JSON, which is easy to parse without pulling
/// in mempalace as a dependency.
/// What: Iterates lines, filters `add_drawer` ops, and maps each into a
/// `RawMemory` using the stored content_preview (truncated but better than
/// nothing).
/// Test: `parse_wal_line_*` unit tests cover the supported and ignored shapes.
pub fn read_mempalace_memories() -> Result<Vec<RawMemory>> {
    let wal_path = match dirs::home_dir() {
        Some(h) => h.join(".mempalace").join("wal").join("write_log.jsonl"),
        None => return Ok(Vec::new()),
    };
    if !wal_path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&wal_path)
        .with_context(|| format!("read mempalace WAL at {}", wal_path.display()))?;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(mem) = parse_wal_line(line) {
            out.push(mem);
        }
    }
    Ok(out)
}

/// Parse a single mempalace WAL JSON line. Returns `None` for non-add_drawer
/// ops or malformed input.
pub fn parse_wal_line(line: &str) -> Option<RawMemory> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let op = v.get("operation")?.as_str()?;
    if op != "add_drawer" {
        return None;
    }
    let params = v.get("params")?;
    let content = params
        .get("content_preview")
        .or_else(|| params.get("content"))
        .and_then(|c| c.as_str())?
        .to_string();
    if content.trim().is_empty() {
        return None;
    }
    let room_str = params
        .get("room")
        .and_then(|r| r.as_str())
        .unwrap_or("general");
    let room = RoomType::parse(room_str);
    let importance = params
        .get("importance")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.5) as f32;
    Some(RawMemory {
        content,
        importance: importance.clamp(0.0, 1.0),
        room,
        source: "mempalace".to_string(),
    })
}

// ── Discovery helpers ────────────────────────────────────────────────────────

/// Derive a palace name (kebab-case lowercase) from a project path.
///
/// Why: Palaces use kebab-case ids; users want their project directory name
/// to be the palace name without manual configuration.
/// What: Takes the final path component, lowercases it, replaces non-alnum
/// characters with `-`, and collapses repeated dashes.
/// Test: `derive_palace_name_*` unit tests assert the canonical shape.
pub fn derive_palace_name(path: &Path) -> String {
    let raw = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string());
    let lower = raw.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut last_dash = false;
    for c in lower.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed
    }
}

/// Discover project directories that contain a `.kuzu-memory/memories.db`.
///
/// Why: Machine-wide conversion needs to enumerate every kuzu-memory project
/// without a config file; we walk the common project roots.
/// What: Scans `~/Projects`, `~/src`, `~/dev`, `~/code` (one level deep) and
/// returns directories whose `.kuzu-memory/memories.db` exists.
/// Test: Behavioral; trivially covered by integration runs.
fn discover_kuzu_projects() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        for dir in ["Projects", "src", "dev", "code"] {
            roots.push(home.join(dir));
        }
    }
    let mut out = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() && p.join(".kuzu-memory").join("memories.db").exists() {
                out.push(p);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn map_kuzu_room_type_semantic() {
        assert_eq!(map_kuzu_room_type("SEMANTIC"), RoomType::Research);
        assert_eq!(map_kuzu_room_type("semantic"), RoomType::Research);
    }

    #[test]
    fn map_kuzu_room_type_procedural() {
        assert_eq!(map_kuzu_room_type("PROCEDURAL"), RoomType::Backend);
    }

    #[test]
    fn map_kuzu_room_type_episodic_working_sensory() {
        assert_eq!(map_kuzu_room_type("EPISODIC"), RoomType::General);
        assert_eq!(map_kuzu_room_type("WORKING"), RoomType::General);
        assert_eq!(map_kuzu_room_type("SENSORY"), RoomType::General);
    }

    #[test]
    fn map_kuzu_room_type_unknown_defaults_to_general() {
        assert_eq!(map_kuzu_room_type("WHATEVER"), RoomType::General);
        assert_eq!(map_kuzu_room_type(""), RoomType::General);
    }

    #[test]
    fn parse_wal_line_add_drawer_returns_memory() {
        let line = r#"{"operation":"add_drawer","params":{"drawer_id":"abc","wing":"trusty","room":"backend","content_preview":"Use HNSW index","importance":0.7}}"#;
        let mem = parse_wal_line(line).expect("should parse");
        assert_eq!(mem.content, "Use HNSW index");
        assert_eq!(mem.room, RoomType::Backend);
        assert!((mem.importance - 0.7).abs() < 1e-4);
        assert_eq!(mem.source, "mempalace");
    }

    #[test]
    fn parse_wal_line_non_add_drawer_returns_none() {
        let line = r#"{"operation":"create_palace","params":{"name":"foo"}}"#;
        assert!(parse_wal_line(line).is_none());
    }

    #[test]
    fn parse_wal_line_malformed_returns_none() {
        assert!(parse_wal_line("not json").is_none());
        assert!(parse_wal_line(r#"{"operation":"add_drawer"}"#).is_none());
    }

    #[test]
    fn parse_wal_line_empty_content_returns_none() {
        let line = r#"{"operation":"add_drawer","params":{"content_preview":""}}"#;
        assert!(parse_wal_line(line).is_none());
    }

    #[test]
    fn derive_palace_name_simple_project() {
        let p = PathBuf::from("/Users/x/Projects/trusty-memory");
        assert_eq!(derive_palace_name(&p), "trusty-memory");
    }

    #[test]
    fn derive_palace_name_uppercase_and_spaces() {
        let p = PathBuf::from("/tmp/My Cool Project");
        assert_eq!(derive_palace_name(&p), "my-cool-project");
    }

    #[test]
    fn derive_palace_name_collapses_repeated_separators() {
        let p = PathBuf::from("/tmp/foo___bar..baz");
        assert_eq!(derive_palace_name(&p), "foo-bar-baz");
    }

    #[test]
    fn derive_palace_name_empty_falls_back_to_unknown() {
        let p = PathBuf::from("/");
        assert_eq!(derive_palace_name(&p), "unknown");
    }

    #[test]
    fn parse_csv_line_handles_quoted_commas() {
        let row = r#""id1","hello, world","0.8","SEMANTIC","2024-01-01""#;
        let fields = parse_csv_line(row);
        assert_eq!(fields.len(), 5);
        assert_eq!(fields[1], "hello, world");
        assert_eq!(fields[3], "SEMANTIC");
    }

    #[test]
    fn parse_kuzu_csv_row_builds_memory() {
        let row = r#""id1","Use HNSW","0.9","SEMANTIC","2024-01-01""#;
        let mem = parse_kuzu_csv_row(row).expect("parse");
        assert_eq!(mem.content, "Use HNSW");
        assert_eq!(mem.room, RoomType::Research);
        assert!((mem.importance - 0.9).abs() < 1e-4);
        assert_eq!(mem.source, "kuzu");
    }

    #[test]
    fn parse_kuzu_csv_row_clamps_importance() {
        let row = r#""id1","x","2.5","EPISODIC","t""#;
        let mem = parse_kuzu_csv_row(row).expect("parse");
        assert!((mem.importance - 1.0).abs() < 1e-4);
    }

    #[test]
    fn content_hash_dedup_works() {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let a = RawMemory {
            content: "hello".into(),
            importance: 0.5,
            room: RoomType::General,
            source: "kuzu".into(),
        };
        let b = a.clone();
        extend_dedup(&mut out, &mut seen, vec![a, b]);
        assert_eq!(out.len(), 1);
    }
}
