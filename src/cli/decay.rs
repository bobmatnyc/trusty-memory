//! `decay` subcommand handler — temporal decay management.
//!
//! Why: Surface decay configuration and reports for tuning.
//! What: `report` shows the largest gap between base and effective importance
//! across the palace's drawers; `config` prints the current `DecayConfig`.
//! Test: Behavior covered by `decay::tests`; CLI parse covered by
//! `cli_help_exits_zero`.

use crate::cli::memory::open_or_create_handle;
use crate::cli::output::OutputConfig;
use crate::cli::DecayCommands;
use anyhow::Result;
use trusty_memory_core::decay::DecayConfig;

pub async fn handle(cmd: DecayCommands, palace: &str, out: &OutputConfig) -> Result<()> {
    match cmd {
        DecayCommands::Report => {
            out.print_header(palace, "decay/report");
            let handle = open_or_create_handle(palace).await?;
            let cfg = &handle.decay_config;
            let snapshot = handle.drawers.read().clone();
            if snapshot.is_empty() {
                println!("(no drawers yet)");
                return Ok(());
            }
            let mut rows: Vec<(uuid::Uuid, f32, f32, f32)> = snapshot
                .iter()
                .map(|d| {
                    let age = DecayConfig::age_days(d.created_at);
                    let boost = d.accumulated_boost(cfg);
                    let eff = cfg.effective_importance(d.importance, age, boost);
                    (d.id, d.importance, eff, age)
                })
                .collect();
            // Largest gap (base - eff) first.
            rows.sort_by(|a, b| {
                (b.1 - b.2)
                    .partial_cmp(&(a.1 - a.2))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            println!("drawer_id                            base    eff    age_d");
            for (id, base, eff, age) in rows.iter().take(20) {
                println!("{id}  {base:>5.2}  {eff:>5.2}  {age:>6.1}");
            }
        }
        DecayCommands::Config => {
            out.print_header(palace, "decay/config");
            let handle = open_or_create_handle(palace).await?;
            let cfg = &handle.decay_config;
            println!("half_life_days:    {}", cfg.half_life_days);
            println!("floor:             {}", cfg.floor);
            println!("access_boost:      {}", cfg.access_boost);
            println!("access_boost_cap:  {}", cfg.access_boost_cap);
        }
    }
    Ok(())
}
