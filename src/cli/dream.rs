//! `dream` subcommand handler — background memory consolidation.
//!
//! Why: Trigger and inspect the consolidation pass.
//! What: Stub for #10 wiring.
//! Test: `cli_help_exits_zero`.

use crate::cli::output::OutputConfig;
use crate::cli::DreamCommands;
use anyhow::Result;

pub async fn handle(cmd: DreamCommands, palace: &str, out: &OutputConfig) -> Result<()> {
    match cmd {
        DreamCommands::Run => {
            out.print_header(palace, "dream/run");
            println!("Consolidation pass: (wiring pending — #10)");
        }
        DreamCommands::Status => {
            out.print_header(palace, "dream/status");
            println!("Last dream run: (wiring pending — #10)");
        }
    }
    Ok(())
}
