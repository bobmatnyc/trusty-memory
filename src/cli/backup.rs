//! `backup` and `restore` subcommand handlers.
//!
//! Why: Operators need to export, transfer, and recover palace data — losing
//! a palace dir means losing all memories for that namespace, so a one-shot
//! archive format is table stakes for adoption.
//! What: Implements `BackupArgs` / `RestoreArgs` plus their handlers.
//! `backup` shells out to `tar -czf` to produce a `.tar.gz` containing the
//! palace data directory plus a top-level `manifest.json`. `restore` extracts
//! the archive into the registry data root, supporting palace renaming
//! (`--palace`) and merge-on-conflict (`--merge`).
//! Test: Unit tests cover filename + manifest helpers; integration test
//! `tests/integration/backup_restore_test.rs` round-trips backup -> delete ->
//! restore -> recall.

use crate::cli::output::OutputConfig;
use crate::cli::palace::data_root;
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use clap::Args;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Manifest schema version embedded in every archive.
const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Args, Debug, Clone)]
pub struct BackupArgs {
    /// Palace to back up (omit when using --all).
    pub palace: Option<String>,

    /// Back up every palace under the data root.
    #[arg(long)]
    pub all: bool,

    /// Output archive path (default: ./<palace>-<YYYYMMDD>.tar.gz).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
pub struct RestoreArgs {
    /// Path to the .tar.gz archive.
    pub archive: PathBuf,

    /// Override palace name on restore (defaults to the manifest's palace_id).
    #[arg(long)]
    pub palace: Option<String>,

    /// Merge into an existing palace instead of erroring.
    #[arg(long)]
    pub merge: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub trusty_memory_version: String,
    pub palace_id: String,
    pub palace_name: String,
    pub backed_up_at: String,
    pub drawer_count: usize,
    pub schema_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PalaceMetaSlim {
    id: String,
    name: String,
}

// ── backup ──────────────────────────────────────────────────────────────────

pub async fn handle_backup(args: BackupArgs, out: &OutputConfig) -> Result<()> {
    let root = data_root()?;

    let palace_ids: Vec<String> = if args.all {
        list_palace_ids(&root)?
    } else {
        let name = args
            .palace
            .clone()
            .ok_or_else(|| anyhow!("missing palace name (or use --all)"))?;
        vec![name]
    };

    if palace_ids.is_empty() {
        out.print_error("no palaces to back up");
        return Ok(());
    }

    for (i, pid) in palace_ids.iter().enumerate() {
        let output_path = if args.all {
            // With --all we always derive a per-palace filename, ignoring -o
            // unless it is provided (then we use it for the *first* palace and
            // synthesize the rest under its parent directory).
            match args.output.as_ref() {
                Some(p) if i == 0 => p.clone(),
                Some(p) => {
                    let parent = p.parent().unwrap_or_else(|| Path::new("."));
                    parent.join(default_output_filename(pid, &today_yyyymmdd()))
                }
                None => PathBuf::from(default_output_filename(pid, &today_yyyymmdd())),
            }
        } else {
            args.output
                .clone()
                .unwrap_or_else(|| PathBuf::from(default_output_filename(pid, &today_yyyymmdd())))
        };

        backup_one(&root, pid, &output_path, out)?;
    }

    Ok(())
}

pub fn backup_one(
    data_root: &Path,
    palace_id: &str,
    output_path: &Path,
    out: &OutputConfig,
) -> Result<()> {
    let palace_dir = data_root.join(palace_id);
    if !palace_dir.exists() {
        bail!(
            "palace '{}' not found at {}",
            palace_id,
            palace_dir.display()
        );
    }

    // Read palace.json for the human-readable name.
    let meta_path = palace_dir.join("palace.json");
    let palace_name = if meta_path.exists() {
        let raw = std::fs::read_to_string(&meta_path)
            .with_context(|| format!("read {}", meta_path.display()))?;
        serde_json::from_str::<PalaceMetaSlim>(&raw)
            .map(|m| m.name)
            .unwrap_or_else(|_| palace_id.to_string())
    } else {
        palace_id.to_string()
    };

    let drawer_count = count_drawers(&palace_dir).unwrap_or(0);

    // Write manifest into a sibling temp dir so tar can pick it up at the
    // archive root.
    let manifest_tmp = tempfile::tempdir().context("create manifest tempdir")?;
    let manifest = Manifest {
        trusty_memory_version: env!("CARGO_PKG_VERSION").to_string(),
        palace_id: palace_id.to_string(),
        palace_name: palace_name.clone(),
        backed_up_at: Utc::now().to_rfc3339(),
        drawer_count,
        schema_version: MANIFEST_SCHEMA_VERSION,
    };
    let manifest_path = manifest_tmp.path().join("manifest.json");
    std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
        .with_context(|| format!("write {}", manifest_path.display()))?;

    // Make sure the output path's parent exists.
    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create output dir {}", parent.display()))?;
        }
    }

    // tar -czf <output> -C <data_root> <palace-id> -C <manifest_tmp> manifest.json
    let status = Command::new("tar")
        .arg("-czf")
        .arg(output_path)
        .arg("-C")
        .arg(data_root)
        .arg(palace_id)
        .arg("-C")
        .arg(manifest_tmp.path())
        .arg("manifest.json")
        .status()
        .context("invoke tar")?;
    if !status.success() {
        bail!("tar exited with status {status}");
    }

    if !out.quiet {
        println!(
            "✓ Backed up palace '{}' ({} drawers) → {}",
            palace_name,
            drawer_count,
            output_path.display()
        );
    }
    Ok(())
}

// ── restore ─────────────────────────────────────────────────────────────────

pub async fn handle_restore(args: RestoreArgs, out: &OutputConfig) -> Result<()> {
    let root = data_root()?;
    restore_to_root(&root, args, out)
}

pub fn restore_to_root(root: &Path, args: RestoreArgs, out: &OutputConfig) -> Result<()> {
    validate_archive_extension(&args.archive.to_string_lossy())?;
    if !args.archive.exists() {
        bail!("archive not found: {}", args.archive.display());
    }

    std::fs::create_dir_all(root)
        .with_context(|| format!("create data root {}", root.display()))?;

    // Extract archive into a temp dir.
    let extract_tmp = tempfile::tempdir().context("create extract tempdir")?;
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&args.archive)
        .arg("-C")
        .arg(extract_tmp.path())
        .status()
        .context("invoke tar -xzf")?;
    if !status.success() {
        bail!("tar exited with status {status}");
    }

    let manifest_path = extract_tmp.path().join("manifest.json");
    if !manifest_path.exists() {
        bail!(
            "archive missing manifest.json at {}",
            manifest_path.display()
        );
    }
    let manifest = parse_manifest(
        &std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?,
    )?;

    if manifest.schema_version > MANIFEST_SCHEMA_VERSION {
        bail!(
            "archive schema_version {} is newer than supported {}",
            manifest.schema_version,
            MANIFEST_SCHEMA_VERSION
        );
    }

    // Source dir inside the extracted archive (named after the *original*
    // palace_id from the manifest).
    let src_dir = extract_tmp.path().join(&manifest.palace_id);
    if !src_dir.exists() {
        bail!(
            "archive missing palace dir '{}' (expected {})",
            manifest.palace_id,
            src_dir.display()
        );
    }

    let target_id = args
        .palace
        .clone()
        .unwrap_or_else(|| manifest.palace_id.clone());
    let target_dir = root.join(&target_id);

    if target_dir.exists() {
        if !args.merge {
            bail!(
                "palace '{}' already exists at {}; pass --merge to combine, or --palace <new-name> to rename",
                target_id,
                target_dir.display()
            );
        }
        merge_restore(&src_dir, &target_dir, &target_id)?;
    } else {
        copy_dir_recursive(&src_dir, &target_dir).with_context(|| {
            format!(
                "copy palace from {} to {}",
                src_dir.display(),
                target_dir.display()
            )
        })?;
        // If the caller renamed the palace, rewrite palace.json so the
        // metadata id matches the directory name.
        if target_id != manifest.palace_id {
            rewrite_palace_meta(&target_dir, &target_id)?;
        }
    }

    if !out.quiet {
        println!(
            "✓ Restored palace '{}' ({} drawers) from {}",
            target_id,
            manifest.drawer_count,
            args.archive.display()
        );
    }
    Ok(())
}

/// Merge restore: copy missing files from `src_dir` into `target_dir` and
/// dedupe-merge the SQLite KG by id.
fn merge_restore(src_dir: &Path, target_dir: &Path, _target_id: &str) -> Result<()> {
    // Copy any file from src that doesn't exist in target.
    for entry in std::fs::read_dir(src_dir)
        .with_context(|| format!("read source palace dir {}", src_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let dest = target_dir.join(&name);
        let src = entry.path();

        if name == "kg.db" && dest.exists() {
            // Merge KG rows by id.
            merge_kg(&src, &dest)?;
            continue;
        }
        if name == "l1_cache.json" && dest.exists() {
            merge_l1_cache(&src, &dest)?;
            continue;
        }
        if !dest.exists() {
            if entry.file_type()?.is_dir() {
                copy_dir_recursive(&src, &dest)?;
            } else {
                std::fs::copy(&src, &dest)
                    .with_context(|| format!("copy {} -> {}", src.display(), dest.display()))?;
            }
        }
        // Otherwise leave the existing target file alone (palace.json,
        // identity.txt, index.usearch are preserved as-is).
    }
    Ok(())
}

/// Merge entities + triples from `src` into `dest` using `INSERT OR IGNORE`
/// so existing rows survive. Other tables are skipped.
fn merge_kg(src: &Path, dest: &Path) -> Result<()> {
    use rusqlite::Connection;

    // Use ATTACH so we can run INSERT OR IGNORE in one statement.
    let conn =
        Connection::open(dest).with_context(|| format!("open dest kg {}", dest.display()))?;
    conn.execute_batch(&format!(
        "ATTACH DATABASE '{}' AS src;",
        src.to_string_lossy().replace('\'', "''")
    ))
    .context("attach source kg")?;

    // Discover tables present in source.
    let mut stmt = conn
        .prepare("SELECT name FROM src.sqlite_master WHERE type='table'")
        .context("list src tables")?;
    let tables: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .filter(|t| !t.starts_with("sqlite_"))
        .collect();
    drop(stmt);

    for table in tables {
        // Best-effort INSERT OR IGNORE INTO main.<table> SELECT * FROM src.<table>.
        // If schema diverges, swallow the error and continue — backup is a
        // best-effort merge, never a fatal operation.
        let _ = conn.execute_batch(&format!(
            "INSERT OR IGNORE INTO main.\"{table}\" SELECT * FROM src.\"{table}\";"
        ));
    }

    let _ = conn.execute_batch("DETACH DATABASE src;");
    Ok(())
}

/// Merge two l1_cache.json arrays, dedup by drawer id.
fn merge_l1_cache(src: &Path, dest: &Path) -> Result<()> {
    let src_raw = std::fs::read_to_string(src).unwrap_or_default();
    let dest_raw = std::fs::read_to_string(dest).unwrap_or_default();

    let src_arr: serde_json::Value =
        serde_json::from_str(&src_raw).unwrap_or(serde_json::json!([]));
    let mut dest_arr: serde_json::Value =
        serde_json::from_str(&dest_raw).unwrap_or(serde_json::json!([]));

    if let (Some(src_items), Some(dest_items)) = (src_arr.as_array(), dest_arr.as_array_mut()) {
        let mut seen: std::collections::HashSet<String> = dest_items
            .iter()
            .filter_map(|v| v.get("id").and_then(|id| id.as_str()).map(String::from))
            .collect();
        for item in src_items {
            let id = item
                .get("id")
                .and_then(|id| id.as_str())
                .map(String::from)
                .unwrap_or_default();
            if id.is_empty() || !seen.contains(&id) {
                if !id.is_empty() {
                    seen.insert(id);
                }
                dest_items.push(item.clone());
            }
        }
    }

    std::fs::write(dest, serde_json::to_vec_pretty(&dest_arr)?)
        .with_context(|| format!("write merged l1 cache to {}", dest.display()))?;
    Ok(())
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn list_palace_ids(data_root: &Path) -> Result<Vec<String>> {
    if !data_root.exists() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(data_root)
        .with_context(|| format!("read data root {}", data_root.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("palace.json").exists() {
            if let Some(name) = entry.file_name().to_str() {
                ids.push(name.to_string());
            }
        }
    }
    ids.sort();
    Ok(ids)
}

/// Count drawers in a palace by reading `l1_cache.json` (best-effort).
///
/// Why: The L1 cache is the only persistent drawer index today; it caps at
/// ~15 entries but it's a stable proxy for "how many drawers does this palace
/// know about" for manifest reporting.
/// What: Reads the file, parses as a JSON array, returns its length. Returns
/// `None` if the file is missing or malformed.
/// Test: Unit test creates a fake l1_cache.json and asserts the count.
fn count_drawers(palace_dir: &Path) -> Option<usize> {
    let path = palace_dir.join("l1_cache.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.as_array().map(|a| a.len())
}

/// Build the default archive filename: `<palace-id>-YYYYMMDD.tar.gz`.
pub fn default_output_filename(palace_id: &str, date: &str) -> String {
    format!("{palace_id}-{date}.tar.gz")
}

fn today_yyyymmdd() -> String {
    Utc::now().format("%Y%m%d").to_string()
}

/// Validate that the archive path ends in `.tar.gz` or `.tgz`.
pub fn validate_archive_extension(name: &str) -> Result<()> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        Ok(())
    } else {
        Err(anyhow!(
            "archive must end in .tar.gz or .tgz (got '{name}')"
        ))
    }
}

/// Parse a manifest JSON string into a `Manifest`.
pub fn parse_manifest(raw: &str) -> Result<Manifest> {
    serde_json::from_str(raw).context("parse manifest.json")
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("create {}", dest.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let s = entry.path();
        let d = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&s, &d)?;
        } else {
            std::fs::copy(&s, &d)
                .with_context(|| format!("copy {} -> {}", s.display(), d.display()))?;
        }
    }
    Ok(())
}

/// Rewrite palace.json in `target_dir` so its `id` and `name` reflect a
/// renamed palace.
fn rewrite_palace_meta(target_dir: &Path, target_id: &str) -> Result<()> {
    let path = target_dir.join("palace.json");
    if !path.exists() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut v: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "id".to_string(),
            serde_json::Value::String(target_id.to_string()),
        );
        obj.insert(
            "name".to_string(),
            serde_json::Value::String(target_id.to_string()),
        );
        if let Some(dd) = obj.get_mut("data_dir") {
            *dd = serde_json::Value::String(target_dir.to_string_lossy().into_owned());
        }
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&v)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_output_filename_format() {
        assert_eq!(
            default_output_filename("my-project", "20260509"),
            "my-project-20260509.tar.gz"
        );
    }

    #[test]
    fn parse_manifest_roundtrip() {
        let raw = r#"{
            "trusty_memory_version": "0.1.9",
            "palace_id": "alpha",
            "palace_name": "Alpha",
            "backed_up_at": "2026-05-09T00:00:00Z",
            "drawer_count": 7,
            "schema_version": 1
        }"#;
        let m = parse_manifest(raw).expect("parse");
        assert_eq!(m.palace_id, "alpha");
        assert_eq!(m.drawer_count, 7);
        assert_eq!(m.schema_version, 1);
    }

    #[test]
    fn validate_archive_extension_accepts_tar_gz() {
        assert!(validate_archive_extension("foo.tar.gz").is_ok());
        assert!(validate_archive_extension("FOO.TGZ").is_ok());
        assert!(validate_archive_extension("path/to/x.tar.gz").is_ok());
    }

    #[test]
    fn validate_archive_extension_rejects_other() {
        assert!(validate_archive_extension("foo.zip").is_err());
        assert!(validate_archive_extension("foo.tar").is_err());
        assert!(validate_archive_extension("foo").is_err());
    }

    #[test]
    fn count_drawers_reads_l1_cache() {
        let dir = tempfile::tempdir().unwrap();
        let payload = serde_json::json!([
            {"id": "a"},
            {"id": "b"},
            {"id": "c"},
        ]);
        std::fs::write(
            dir.path().join("l1_cache.json"),
            serde_json::to_vec(&payload).unwrap(),
        )
        .unwrap();
        assert_eq!(count_drawers(dir.path()), Some(3));
    }

    #[test]
    fn count_drawers_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(count_drawers(dir.path()), None);
    }
}
