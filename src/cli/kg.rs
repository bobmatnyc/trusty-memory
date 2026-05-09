//! `kg` subcommand handler — temporal knowledge graph operations.
//!
//! Why: Surface `assert` / `query` / `history` for the SQLite KG.
//! What: Stub that prints inputs; full impl wires to KG store in follow-up.
//! Test: `cli_help_exits_zero` covers the parse surface.

use crate::cli::output::OutputConfig;
use crate::cli::KgCommands;
use anyhow::Result;

pub async fn handle(cmd: KgCommands, palace: &str, out: &OutputConfig) -> Result<()> {
    match cmd {
        KgCommands::Assert {
            subject,
            predicate,
            object,
            confidence,
            provenance,
        } => {
            out.print_header(palace, "kg/assert");
            println!("({subject}) -[{predicate}]-> ({object})");
            println!("  confidence: {confidence}");
            if let Some(p) = provenance {
                println!("  provenance: {p}");
            }
            out.print_success("asserted (KG wiring pending)");
        }
        KgCommands::Query { subject } => {
            out.print_header(palace, "kg/query");
            println!("Active triples for '{subject}': (none — KG wiring pending)");
        }
        KgCommands::History { subject } => {
            out.print_header(palace, "kg/history");
            println!("History for '{subject}': (none — KG wiring pending)");
        }
    }
    Ok(())
}
