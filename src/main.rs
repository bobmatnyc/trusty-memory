//! trusty-memory CLI entry point.
//!
//! Why: One binary covers serve, palace admin, git ingest, kuzu compatibility,
//! and ad-hoc remember/recall — mirroring how `kuzu-memory` is used so existing
//! muscle memory transfers.
//! What: Parses the top-level `Cli` and dispatches to per-subcommand handlers
//! in the `cli/` module tree.
//! Test: `cargo test --test integration_tests` plus `--help` and `status`
//! integration tests in `tests/integration/cli_test.rs`.

mod cli;

use anyhow::Result;
use clap::{CommandFactory, Parser};
use clap_complete::generate;
use cli::output::OutputConfig;
use cli::palace_resolver::resolve_palace;
use cli::{Cli, Commands};
use std::io;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::from_filename(".env.local").ok();

    let cli = Cli::parse();

    // Init tracing based on verbosity, deferring to RUST_LOG when set.
    let default_filter = match cli.verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    if cli.no_color {
        colored::control::set_override(false);
    }

    let palace = resolve_palace(cli.palace.as_deref());

    let out = OutputConfig {
        json: cli.json,
        quiet: cli.quiet,
        no_color: cli.no_color,
    };

    match cli.command {
        Commands::Remember {
            text,
            room,
            tags,
            importance,
        } => {
            cli::handle_remember(&palace, text, room, tags, importance, &out).await?;
        }

        Commands::Recall {
            query,
            top_k,
            room,
            deep,
            decay: _,
        } => {
            cli::handle_recall(&palace, query, top_k, room, deep, &out).await?;
        }

        Commands::Forget { id } => {
            cli::handle_forget(&palace, &id, &out).await?;
        }

        Commands::List { limit, room, sort } => {
            cli::handle_list(&palace, limit, room, sort, &out).await?;
        }

        Commands::Palace(sub) => cli::palace::handle(sub, &palace, &out).await?,
        Commands::Kg(sub) => cli::kg::handle(sub, &palace, &out).await?,
        Commands::Git(sub) => cli::git::handle(sub, &palace, &out).await?,
        Commands::Kuzu(sub) => cli::kuzu::handle(sub, &palace, &out).await?,
        Commands::Analytics(sub) => cli::analytics::handle(sub, &palace, &out).await?,
        Commands::Decay(sub) => cli::decay::handle(sub, &palace, &out).await?,
        Commands::Dream(sub) => cli::dream::handle(sub, &palace, &out).await?,

        Commands::Serve {
            http,
            mcp: _,
            palace: default_palace,
        } => {
            tracing::info!(?http, ?default_palace, "Starting trusty-memory MCP server");
            let data_root_for_state = cli::palace::data_root()?;

            // Auto-create the default palace if --palace was supplied and the
            // palace doesn't yet exist on disk.
            //
            // Why: Issue #26 — open-mpm spawns one trusty-memory process per
            // project and binds it to a project-scoped palace; requiring a
            // separate `palace new` step would fight that integration.
            // What: If `default_palace` is set and not already persisted under
            // `data_root`, build a `Palace` record and call `create_palace`
            // (which writes metadata + opens a handle).
            // Test: covered by integration via `default_palace_used_when_arg_omitted`.
            if let Some(name) = default_palace.as_deref() {
                let pid = trusty_memory_core::PalaceId::new(name);
                let palace_dir = data_root_for_state.join(pid.as_str());
                if !palace_dir.join("palace.json").exists() {
                    tracing::info!(palace = %name, "auto-creating default palace");
                    let registry = trusty_memory_core::PalaceRegistry::new();
                    let palace = trusty_memory_core::Palace {
                        id: pid,
                        name: name.to_string(),
                        description: Some(
                            "Auto-created by `trusty-memory serve --palace`".to_string(),
                        ),
                        created_at: chrono::Utc::now(),
                        data_dir: palace_dir,
                    };
                    if let Err(e) = registry.create_palace(&data_root_for_state, palace) {
                        tracing::warn!(palace = %name, "failed to auto-create palace: {e:#}");
                    }
                }
            }

            let state = trusty_memory_mcp::AppState::new(data_root_for_state)
                .with_default_palace(default_palace);

            // Auto-start the Dreamer for every persisted palace so background
            // consolidation runs while the daemon is alive.
            //
            // Why: Issue #21 — operators expect background dedup/prune/closet
            // refresh without having to invoke a separate command. We discover
            // every palace under data_root, open it, and spawn a per-palace
            // dream loop with a shared shutdown signal so all loops terminate
            // cleanly when the daemon stops.
            // What: A `tokio::sync::watch::Sender<bool>` fans out shutdown to
            // every spawned dream task; the join handles are kept so we can
            // await them on exit (best-effort).
            // Test: `dreamer_shutdown_terminates_loop` covers cancellation;
            // discovery is exercised manually via `trusty-memory serve`.
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let mut dream_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
            match cli::palace::data_root() {
                Ok(root) => match trusty_memory_core::PalaceRegistry::list_palaces(&root) {
                    Ok(palaces) => {
                        let registry = trusty_memory_core::PalaceRegistry::new();
                        for p in palaces {
                            match registry.open_palace(&root, &p.id) {
                                Ok(handle) => {
                                    let dreamer = std::sync::Arc::new(
                                        trusty_memory_core::dream::Dreamer::new(
                                            trusty_memory_core::dream::DreamConfig::default(),
                                        ),
                                    );
                                    dream_handles.push(
                                        dreamer.start_with_shutdown(handle, shutdown_rx.clone()),
                                    );
                                }
                                Err(e) => tracing::warn!(
                                    palace = %p.id,
                                    "failed to open palace for dreamer: {e:#}"
                                ),
                            }
                        }
                        tracing::info!(
                            count = dream_handles.len(),
                            "Dreamer started — background consolidation active"
                        );
                    }
                    Err(e) => tracing::warn!("failed to enumerate palaces for Dreamer: {e:#}"),
                },
                Err(e) => tracing::warn!("failed to resolve data root for Dreamer: {e:#}"),
            }

            let serve_result = if let Some(addr) = http {
                tokio::select! {
                    r = trusty_memory_mcp::run_stdio(state.clone()) => r,
                    r = trusty_memory_mcp::run_http(state, addr) => r,
                    _ = tokio::signal::ctrl_c() => {
                        tracing::info!("ctrl-c received, shutting down");
                        Ok(())
                    }
                }
            } else {
                tokio::select! {
                    r = trusty_memory_mcp::run_stdio(state) => r,
                    _ = tokio::signal::ctrl_c() => {
                        tracing::info!("ctrl-c received, shutting down");
                        Ok(())
                    }
                }
            };

            // Signal every dream task to exit and wait briefly for cleanup.
            let _ = shutdown_tx.send(true);
            for jh in dream_handles {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), jh).await;
            }
            serve_result?;
        }

        Commands::Setup {
            non_interactive,
            skip_migration,
            migrate_only,
        } => {
            let opts = cli::setup::SetupOpts {
                non_interactive,
                skip_migration,
                migrate_only,
            };
            cli::setup::handle_setup(opts, &out).await?;
        }

        Commands::Chat {
            message,
            top_k,
            remember,
        } => {
            let opts = cli::chat::ChatOpts {
                message,
                remember,
                top_k,
            };
            cli::chat::handle_chat(&palace, opts, &out).await?;
        }

        Commands::Config(sub) => match sub {
            cli::ConfigCommands::Show => {
                let cfg = cli::config::UserConfig::load()?;
                let path = cli::config::default_config_path()?;
                println!("config: {}", path.display());
                println!("{}", toml::to_string_pretty(&cfg)?);
            }
            cli::ConfigCommands::Set { key, value } => {
                let mut cfg = cli::config::UserConfig::load().unwrap_or_default();
                cfg.set_dotted(&key, &value)?;
                cfg.save()?;
                let path = cli::config::default_config_path()?;
                println!("set {key} in {}", path.display());
            }
        },

        Commands::Convert(args) => {
            cli::convert::handle_convert(args).await?;
        }

        Commands::Bench(sub) => match sub {
            cli::BenchCommands::Compare(args) => {
                let opts = cli::bench::BenchCompareOpts {
                    corpus: args.corpus,
                    top_k: args.top_k,
                    mempalace: args.mempalace,
                    kuzu: args.kuzu,
                    json: args.json,
                };
                cli::bench::handle_bench_compare(opts).await?;
            }
        },

        Commands::Hooks(args) => {
            cli::hooks::handle(args, &palace, &out).await?;
        }

        Commands::Status => {
            let binary = std::env::current_exe()?;
            let root = cli::palace::data_root()?;
            let root_clone = root.clone();
            let palaces = tokio::task::spawn_blocking(move || {
                trusty_memory_core::PalaceRegistry::list_palaces(&root_clone)
            })
            .await??;
            println!("trusty-memory v{}", env!("CARGO_PKG_VERSION"));
            println!("binary: {}", binary.display());
            println!("data_root: {}", root.display());
            println!("palaces: {}", palaces.len());
            println!("active palace: {palace}");
            println!("daemon: not running (serve not started)");
        }

        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            generate(shell, &mut cmd, name, &mut io::stdout());
        }
    }

    Ok(())
}
