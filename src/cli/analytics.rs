//! `analytics` subcommand handler — recall hit/miss tracking.
//!
//! Why: Track which drawers actually get recalled so importance can adapt.
//! What: Stub for #13 wiring; prints intent.
//! Test: `cli_help_exits_zero`.

use crate::cli::output::OutputConfig;
use crate::cli::AnalyticsCommands;
use anyhow::Result;

pub async fn handle(cmd: AnalyticsCommands, palace: &str, out: &OutputConfig) -> Result<()> {
    match cmd {
        AnalyticsCommands::Show { days } => {
            out.print_header(palace, "analytics/show");
            println!("Recall summary for last {days} day(s): (analytics wiring pending — #13)");
        }
        AnalyticsCommands::Top { limit } => {
            out.print_header(palace, "analytics/top");
            println!("Top {limit} drawers by recall count: (analytics wiring pending — #13)");
        }
    }
    Ok(())
}
