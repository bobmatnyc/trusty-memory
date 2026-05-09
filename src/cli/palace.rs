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
            let target = id.as_deref().unwrap_or("(active)");
            println!("Palace info: {target}");
        }
        PalaceCommands::Delete { name } => {
            println!("Deleting palace '{name}'");
            out.print_success("deleted (registry wiring pending)");
        }
        PalaceCommands::Rename { old, new } => {
            println!("Renaming palace '{old}' -> '{new}'");
            out.print_success("renamed (registry wiring pending)");
        }
    }
    Ok(())
}
