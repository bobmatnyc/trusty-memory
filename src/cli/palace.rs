//! `palace` subcommand handler.
//!
//! Why: Group all palace-admin operations under one namespace.
//! What: Stub handlers that print intent until the registry is wired in (#15 follow-up).
//! Test: Covered by `cli_help_exits_zero` integration test.

use crate::cli::output::OutputConfig;
use crate::cli::PalaceCommands;
use anyhow::Result;

pub async fn handle(cmd: PalaceCommands, _palace: &str, out: &OutputConfig) -> Result<()> {
    match cmd {
        PalaceCommands::List => {
            out.print_header("palaces", "list");
            println!("(registry wiring pending — no palaces listed)");
        }
        PalaceCommands::New { name, description } => {
            println!("Creating palace '{name}'");
            if let Some(d) = description {
                println!("  description: {d}");
            }
            out.print_success("created (registry wiring pending)");
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
