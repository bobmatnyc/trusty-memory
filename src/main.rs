//! trusty-memory CLI entry point.
//!
//! Why: One binary covers serve, palace admin, and ad-hoc remember/recall —
//! mirroring how `kuzu-memory` is used so existing muscle memory transfers.
//! What: clap subcommands dispatched to either the MCP server or core APIs.
//! Test: `trusty-memory --help` prints all subcommands; `trusty-memory status`
//! exits 0 with a status line.

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "trusty-memory",
    version,
    about = "Machine-wide AI memory service with Memory Palace architecture"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start MCP stdio server (and optionally HTTP/SSE).
    Serve {
        /// Also bind HTTP/SSE on this address (e.g. `127.0.0.1:3031`).
        #[arg(long)]
        http: Option<String>,
    },
    /// Manage palaces.
    Palace {
        #[command(subcommand)]
        action: PalaceAction,
    },
    /// Store a memory in a palace.
    Remember {
        palace: String,
        text: String,
        #[arg(long)]
        room: Option<String>,
    },
    /// Retrieve memories from a palace.
    Recall {
        palace: String,
        query: String,
        #[arg(long, default_value = "5")]
        top_k: usize,
    },
    /// Show service status.
    Status,
}

#[derive(Subcommand)]
enum PalaceAction {
    /// Create a new palace.
    New { name: String },
    /// List all known palaces on this machine.
    List,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::from_filename(".env.local").ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { http } => {
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
        Commands::Palace { action } => match action {
            PalaceAction::New { name } => println!("Creating palace: {name}"),
            PalaceAction::List => println!("Listing palaces..."),
        },
        Commands::Remember { palace, text, room } => {
            println!("Storing in palace '{palace}' (room={room:?}): {text}");
        }
        Commands::Recall {
            palace,
            query,
            top_k,
        } => {
            println!("Recalling from palace '{palace}': {query} (top_k={top_k})");
        }
        Commands::Status => {
            println!("trusty-memory status: scaffold (not yet implemented)");
        }
    }

    Ok(())
}
