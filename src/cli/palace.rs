//! `palace` subcommand handler.
//!
//! Why: Group all palace-admin operations under one namespace. With #7 wired in,
//! `new` and `list` now persist to and read from the on-disk registry root.
//! What: Routes to `PalaceRegistry` for create/list/info; delete/rename remain
//! stubs until #15 lands the full lifecycle.
//! Test: Covered by `cli_help_exits_zero` integration test plus core registry
//! tests.

use crate::cli::output::OutputConfig;
use crate::cli::PalaceCommands;
use anyhow::{Context, Result};
use chrono::Utc;
use std::path::PathBuf;
use trusty_memory_core::store::kg::KnowledgeGraph;
use trusty_memory_core::store::vector::UsearchStore;
use trusty_memory_core::{Palace, PalaceId, PalaceRegistry};

/// Resolve the machine-wide data root: `<dirs::data_dir>/trusty-memory/palaces/`.
///
/// Why: Single install per machine means a single canonical root; centralizing
/// the path keeps callers from drifting.
/// What: Falls back to `~/.trusty-memory/palaces/` if `dirs::data_dir()` is
/// unavailable.
/// Test: Implicitly covered by CLI integration tests on a tempdir override.
pub fn data_root() -> Result<PathBuf> {
    let base = dirs::data_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".trusty-memory")))
        .context("could not resolve a data dir for trusty-memory")?;
    Ok(base.join("trusty-memory").join("palaces"))
}

pub async fn handle(cmd: PalaceCommands, _palace: &str, out: &OutputConfig) -> Result<()> {
    match cmd {
        PalaceCommands::List => {
            let root = data_root()?;
            out.print_header("palaces", "list");
            let palaces = tokio::task::spawn_blocking(move || PalaceRegistry::list_palaces(&root))
                .await
                .context("join list_palaces")??;
            if palaces.is_empty() {
                println!("(no palaces yet — create one with `trusty-memory palace new <name>`)");
            } else {
                for p in &palaces {
                    println!("  {} — {}", p.id, p.name);
                }
                out.print_footer(palaces.len(), "list", 0);
            }
        }
        PalaceCommands::New { name, description } => {
            let root = data_root()?;
            let palace = Palace {
                id: PalaceId::new(name.clone()),
                name: name.clone(),
                description: description.clone(),
                created_at: Utc::now(),
                data_dir: root.join(&name),
            };
            let root_clone = root.clone();
            tokio::task::spawn_blocking(move || {
                let reg = PalaceRegistry::new();
                reg.create_palace(&root_clone, palace).map(|_| ())
            })
            .await
            .context("join create_palace")??;
            println!("Created palace '{name}'");
            if let Some(d) = description {
                println!("  description: {d}");
            }
            println!("  data_dir: {}", root.join(&name).display());
            out.print_success("created");
        }
        PalaceCommands::Info { id } => {
            let root = data_root()?;
            let target_id = id.unwrap_or_else(|| _palace.to_string());
            let palaces = tokio::task::spawn_blocking({
                let root = root.clone();
                move || PalaceRegistry::list_palaces(&root)
            })
            .await
            .context("join list_palaces")??;
            match palaces.into_iter().find(|p| p.id.as_str() == target_id) {
                Some(p) => {
                    println!("id:          {}", p.id);
                    println!("name:        {}", p.name);
                    if let Some(d) = p.description {
                        println!("description: {d}");
                    }
                    println!("created_at:  {}", p.created_at.to_rfc3339());
                    println!("data_dir:    {}", p.data_dir.display());
                }
                None => {
                    out.print_error(&format!("palace '{target_id}' not found"));
                }
            }
        }
        PalaceCommands::Delete { name } => {
            let root = data_root()?;
            let palace_dir = root.join(&name);
            if !palace_dir.exists() {
                out.print_error(&format!("palace '{name}' not found"));
                return Ok(());
            }
            std::fs::remove_dir_all(&palace_dir)
                .with_context(|| format!("remove palace dir {}", palace_dir.display()))?;
            out.print_success(&format!("deleted palace '{name}'"));
        }
        PalaceCommands::Compact { name } => {
            let root = data_root()?;
            let target = name.unwrap_or_else(|| _palace.to_string());
            let palace_dir = root.join(&target);
            if !palace_dir.exists() {
                out.print_error(&format!("palace '{target}' not found"));
                return Ok(());
            }
            let vector_path = palace_dir.join("index.usearch");
            let kg_path = palace_dir.join("kg.db");
            if !vector_path.exists() || !kg_path.exists() {
                out.print_error(&format!(
                    "palace '{target}' is missing index.usearch or kg.db"
                ));
                return Ok(());
            }

            println!("Compacting palace '{target}'...");
            // Run on a blocking thread — both usearch and SQLite are sync C/C++.
            let stats = tokio::task::spawn_blocking(move || -> Result<_> {
                let store =
                    UsearchStore::new(vector_path, 384).context("open vector store for compact")?;
                let kg = KnowledgeGraph::open(&kg_path).context("open KG for compact")?;
                let valid = kg.load_drawer_ids().context("load_drawer_ids")?;
                let res = store.compact_orphans(&valid).context("compact_orphans")?;
                Ok(res)
            })
            .await
            .context("join compact task")??;

            let pct = if stats.total_checked > 0 {
                (stats.orphans_removed as f64 * 100.0 / stats.total_checked as f64).round() as u64
            } else {
                0
            };
            println!(
                "  checked {} vectors, removed {} orphans ({pct}%)",
                stats.total_checked, stats.orphans_removed
            );
            if stats.index_size_before != stats.total_checked {
                println!(
                    "  note: HNSW index reports {} entries; only {} are tracked by this session's key_map",
                    stats.index_size_before, stats.total_checked
                );
                println!(
                    "  (cold-reload limitation — rerun after some writes, or use the dream loop's rebuild path)"
                );
            }
            println!(
                "  index size: {} -> {}",
                stats.index_size_before, stats.index_size_after
            );
            out.print_success("compacted");
        }
        PalaceCommands::Rename { old, new } => {
            let root = data_root()?;
            let from = root.join(&old);
            let to = root.join(&new);
            if !from.exists() {
                out.print_error(&format!("palace '{old}' not found"));
                return Ok(());
            }
            std::fs::rename(&from, &to)
                .with_context(|| format!("rename {} -> {}", from.display(), to.display()))?;
            out.print_success(&format!("renamed '{old}' -> '{new}'"));
        }
    }
    Ok(())
}
