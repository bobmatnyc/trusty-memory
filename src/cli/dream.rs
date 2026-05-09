//! `dream` subcommand handler — background memory consolidation.
//!
//! Why: Surface the dream loop to operators: trigger a one-shot consolidation
//! pass, or inspect the current idle clock + dream config.
//! What: `Run` runs `Dreamer::dream_cycle` synchronously and prints
//! `DreamStats`; `Status` prints last-activity / is-idle / config.
//! Test: `dream::tests` cover behavior; CLI parse covered by
//! `cli_help_exits_zero`.

use crate::cli::memory::open_or_create_handle;
use crate::cli::output::OutputConfig;
use crate::cli::DreamCommands;
use anyhow::Result;
use serde_json::json;
use std::sync::Arc;
use trusty_memory_core::dream::{DreamConfig, Dreamer};

pub async fn handle(cmd: DreamCommands, palace: &str, out: &OutputConfig) -> Result<()> {
    match cmd {
        DreamCommands::Run => {
            out.print_header(palace, "dream/run");
            let handle = open_or_create_handle(palace).await?;
            let dreamer = Dreamer::new(DreamConfig::default());
            let stats = dreamer.dream_cycle(&handle).await?;
            if out.json {
                out.print_json(&json!({
                    "merged": stats.merged,
                    "pruned": stats.pruned,
                    "closets_updated": stats.closets_updated,
                    "duration_ms": stats.duration_ms,
                }));
            } else {
                println!("merged:          {}", stats.merged);
                println!("pruned:          {}", stats.pruned);
                println!("closets_updated: {}", stats.closets_updated);
                println!("duration_ms:     {}", stats.duration_ms);
            }
        }
        DreamCommands::Status => {
            out.print_header(palace, "dream/status");
            let _handle: Arc<_> = open_or_create_handle(palace).await?;
            let dreamer = Dreamer::new(DreamConfig::default());
            let cfg = &dreamer.config;
            if out.json {
                out.print_json(&json!({
                    "is_idle": dreamer.is_idle(),
                    "config": {
                        "idle_secs": cfg.idle_secs,
                        "dedup_threshold": cfg.dedup_threshold,
                        "prune_importance": cfg.prune_importance,
                        "max_cycle_ms": cfg.max_cycle_ms,
                    }
                }));
            } else {
                println!("is_idle:           {}", dreamer.is_idle());
                println!("idle_secs:         {}", cfg.idle_secs);
                println!("dedup_threshold:   {}", cfg.dedup_threshold);
                println!("prune_importance:  {}", cfg.prune_importance);
                println!("max_cycle_ms:      {}", cfg.max_cycle_ms);
            }
        }
    }
    Ok(())
}
