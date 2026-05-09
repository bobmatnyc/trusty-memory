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

        Commands::Serve { http, mcp: _ } => {
            tracing::info!(?http, "Starting trusty-memory MCP server");
            let state = trusty_memory_mcp::AppState::new();
            if let Some(addr) = http {
                tokio::select! {
                    r = trusty_memory_mcp::run_stdio(state.clone()) => r?,
                    r = trusty_memory_mcp::run_http(state, addr) => r?,
                }
            } else {
                trusty_memory_mcp::run_stdio(state).await?;
            }
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
