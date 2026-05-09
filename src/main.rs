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
            out.print_header(&palace, &room);
            println!("Storing memory in palace '{palace}' room '{room}'");
            println!("  importance: {importance}");
            if !tags.is_empty() {
                println!("  tags: {}", tags.join(", "));
            }
            let preview_len = text.len().min(80);
            println!("  preview: {}", &text[..preview_len]);
            out.print_success("stored (registry wiring pending)");
        }

        Commands::Recall {
            query,
            top_k,
            room,
            deep,
            decay: _,
        } => {
            out.print_header(&palace, room.as_deref().unwrap_or("all rooms"));
            let layer = if deep { "L3" } else { "L2" };
            println!("Recalling '{query}' from '{palace}' (top {top_k}, {layer})");
            out.print_footer(0, layer, 0);
        }

        Commands::Forget { id } => {
            println!("Removing drawer {id} from '{palace}'");
            out.print_success("removed (registry wiring pending)");
        }

        Commands::List { limit, room, sort } => {
            out.print_header(&palace, room.as_deref().unwrap_or("all"));
            println!("Listing up to {limit} memories sorted by {sort}");
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
            if let Some(addr_str) = http {
                let addr: std::net::SocketAddr = addr_str
                    .parse()
                    .map_err(|e| anyhow::anyhow!("invalid --http address {addr_str:?}: {e}"))?;
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
            println!("trusty-memory setup");
            println!("  non_interactive: {non_interactive}");
            println!("  skip_migration: {skip_migration}");
            println!("  migrate_only: {migrate_only}");
            println!("(full implementation in #14)");
        }

        Commands::Status => {
            let binary = std::env::current_exe()?;
            println!("trusty-memory v{}", env!("CARGO_PKG_VERSION"));
            println!("binary: {}", binary.display());
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
