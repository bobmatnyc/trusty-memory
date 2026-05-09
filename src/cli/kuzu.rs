//! `kuzu` subcommand handler — kuzu-memory compatibility layer.
//!
//! Why: claude-mpm projects already store facts in KuzuDB; this lets users
//! discover and migrate that data into trusty-memory palaces.
//! What: Wires `KuzuSource::discover` and `KuzuSource::recall` for the CLI.
//! Test: `cli_help_exits_zero` covers parse; behavior tested in core crate.

use crate::cli::output::OutputConfig;
use crate::cli::KuzuCommands;
use anyhow::Result;
use trusty_memory_core::store::kuzu::KuzuSource;

pub async fn handle(cmd: KuzuCommands, _palace: &str, out: &OutputConfig) -> Result<()> {
    match cmd {
        KuzuCommands::Discover => {
            let roots = KuzuSource::default_roots();
            let found = KuzuSource::discover(&roots);
            if found.is_empty() {
                println!("No kuzu-memory databases found.");
                println!("Searched:");
                for root in &roots {
                    println!("  {}", root.display());
                }
            } else {
                out.print_header("kuzu", "discover");
                println!("Found {} kuzu database(s):", found.len());
                for db in &found {
                    println!("  {} -> {}", db.name, db.path.display());
                }
            }
        }
        KuzuCommands::Recall { query, db } => {
            let path = match db {
                Some(p) => p,
                None => {
                    let found = KuzuSource::discover(&KuzuSource::default_roots());
                    match found.into_iter().next() {
                        Some(d) => d.path,
                        None => {
                            out.print_error("no kuzu database found; pass --db <path>");
                            return Ok(());
                        }
                    }
                }
            };
            let src = KuzuSource::open(path)?;
            let results = src.recall(&query, 10)?;
            if results.is_empty() {
                println!("(no results — kuzu bindings not yet wired)");
            } else {
                for r in results {
                    println!("- {r}");
                }
            }
        }
        KuzuCommands::Migrate { db_path } => {
            println!("kuzu migrate: {db_path:?} (full impl in #14)");
        }
    }
    Ok(())
}
