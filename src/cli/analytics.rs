//! `analytics` subcommand handler — recall hit/miss tracking.
//!
//! Why: Track which drawers actually get recalled so importance can adapt.
//! What: Reads from the active palace's `recall_log` (when present) to print
//! top-recalled drawers, missed-query stats, and miss-rate summaries.
//! Test: Behavior covered by core `analytics::tests`; CLI parse covered by
//! `cli_help_exits_zero`.

use crate::cli::memory::open_or_create_handle;
use crate::cli::output::OutputConfig;
use crate::cli::AnalyticsCommands;
use anyhow::Result;

pub async fn handle(cmd: AnalyticsCommands, palace: &str, out: &OutputConfig) -> Result<()> {
    let handle = open_or_create_handle(palace).await?;
    let Some(log) = handle.recall_log.clone() else {
        out.print_header(palace, "analytics");
        println!("Analytics not configured for this palace.");
        println!("(Recall log wiring required — palaces must be opened with `with_recall_log`.)");
        return Ok(());
    };

    match cmd {
        AnalyticsCommands::Show { days } => {
            out.print_header(palace, "analytics/show");
            let rate = log.miss_rate(palace, days).await?;
            println!("Recall miss rate (last {days}d): {:.1}%", rate * 100.0);
            let missed = log.missed_queries(palace, 10).await?;
            if missed.is_empty() {
                println!("No missed queries in window.");
            } else {
                println!("Top missed queries:");
                for (hash, count) in missed {
                    println!("  query_hash={hash:016x}  misses={count}");
                }
            }
        }
        AnalyticsCommands::Top { limit } => {
            out.print_header(palace, "analytics/top");
            let top = log.top_drawers(palace, limit).await?;
            if top.is_empty() {
                println!("(no recall events yet)");
            } else {
                for (id, hits) in top {
                    println!("  {id}  hits={hits}");
                }
            }
        }
    }
    Ok(())
}
