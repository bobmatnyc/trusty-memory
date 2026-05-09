//! `decay` subcommand handler — temporal decay management.
//!
//! Why: Surface decay configuration and reports for tuning.
//! What: Stub for #11 wiring.
//! Test: `cli_help_exits_zero`.

use crate::cli::output::OutputConfig;
use crate::cli::DecayCommands;
use anyhow::Result;

pub async fn handle(cmd: DecayCommands, palace: &str, out: &OutputConfig) -> Result<()> {
    match cmd {
        DecayCommands::Report => {
            out.print_header(palace, "decay/report");
            println!("Top decayed drawers: (decay wiring pending — #11)");
        }
        DecayCommands::Config => {
            out.print_header(palace, "decay/config");
            println!("Decay config: (wiring pending — #11)");
        }
    }
    Ok(())
}
