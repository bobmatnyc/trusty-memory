//! CLI module: clap derive structures and subcommand handlers.
//!
//! Why: Centralizes the project-aware command surface so `main.rs` only
//! parses + dispatches. Subcommand handlers live in their own files for
//! readability and to keep file sizes well under 800 lines.
//! What: Exports `Cli` (top-level parser), `Commands` enum (subcommands),
//! per-group enums (`PalaceCommands`, `KgCommands`, …), and handler modules.
//! Test: `cargo test --test integration_tests` plus `cargo build` covers
//! the parse surface; per-handler unit tests live in their own modules.

use clap::{Parser, Subcommand, ValueEnum};
use std::net::SocketAddr;
use std::path::PathBuf;

pub mod analytics;
pub mod backup;
pub mod bench;
pub mod chat;
pub mod config;
pub mod convert;
pub mod decay;
pub mod dream;
pub mod git;
pub mod hooks;
pub mod kg;
pub mod kuzu;
pub mod memory;
pub mod output;

pub use memory::{handle_forget, handle_list, handle_recall, handle_remember};
pub mod palace;
pub mod palace_resolver;
pub mod service;
pub mod setup;

#[derive(Parser)]
#[command(
    name = "trusty-memory",
    about = "Machine-wide AI memory service with Memory Palace architecture",
    version,
    propagate_version = true,
    after_help = "Examples:\n  trusty-memory remember \"Rust uses HNSW for vector search\"\n  trusty-memory recall \"vector store\"\n  trusty-memory palace list\n  trusty-memory git ingest\n  trusty-memory status"
)]
pub struct Cli {
    /// Active palace [default: inferred from current directory]
    #[arg(short = 'p', long, global = true, env = "TRUSTY_PALACE")]
    pub palace: Option<String>,

    /// Output as JSON
    #[arg(short = 'j', long, global = true)]
    pub json: bool,

    /// Disable ANSI color
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Suppress headers and decoration
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,

    /// Verbose logging (-v debug, -vv trace)
    #[arg(short = 'v', long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Store a memory in the active palace.
    #[command(
        after_help = "Examples:\n  trusty-memory remember \"Rust uses HNSW for vectors\"\n  trusty-memory remember \"API uses JWT\" --room backend --importance 0.8"
    )]
    Remember {
        /// Memory content to store
        text: String,
        /// Room type
        #[arg(short, long, default_value = "general")]
        room: String,
        /// Comma-separated tags
        #[arg(short, long, value_delimiter = ',')]
        tags: Vec<String>,
        /// Importance 0.0 - 1.0
        #[arg(short, long, default_value = "0.5")]
        importance: f32,
    },

    /// Recall memories matching a query (L0+L1+L2).
    #[command(
        after_help = "Examples:\n  trusty-memory recall \"vector store\"\n  trusty-memory recall \"auth\" --room backend --top-k 5\n  trusty-memory recall \"deployment\" --deep --json"
    )]
    Recall {
        /// Search query
        query: String,
        /// Max results
        #[arg(short = 'k', long, default_value = "10")]
        top_k: usize,
        /// Filter by room type
        #[arg(short, long)]
        room: Option<String>,
        /// Use L3 full-index deep search
        #[arg(short, long)]
        deep: bool,
        /// Apply temporal decay to scores
        #[arg(long, default_value = "true")]
        decay: bool,
    },

    /// Remove a memory by ID.
    Forget {
        /// Drawer UUID to remove
        id: String,
    },

    /// List recent memories in the active palace.
    #[command(
        after_help = "Examples:\n  trusty-memory list\n  trusty-memory list --room backend --limit 5 --sort importance"
    )]
    List {
        #[arg(short = 'k', long, default_value = "20")]
        limit: usize,
        #[arg(short, long)]
        room: Option<String>,
        /// Sort field: importance | created | accessed
        #[arg(long, default_value = "created")]
        sort: String,
    },

    /// Manage memory palaces.
    #[command(
        subcommand,
        after_help = "Examples:\n  trusty-memory palace list\n  trusty-memory palace new my-project\n  trusty-memory palace info"
    )]
    Palace(PalaceCommands),

    /// Knowledge graph operations.
    #[command(subcommand)]
    Kg(KgCommands),

    /// Git history integration (NLP-based, no inference).
    #[command(subcommand)]
    Git(GitCommands),

    /// kuzu-memory compatibility layer.
    #[command(subcommand)]
    Kuzu(KuzuCommands),

    /// Usage analytics (hit/miss tracking).
    #[command(subcommand)]
    Analytics(AnalyticsCommands),

    /// Temporal decay management.
    #[command(subcommand)]
    Decay(DecayCommands),

    /// Memory consolidation (background dreaming).
    #[command(subcommand)]
    Dream(DreamCommands),

    /// Start the MCP server.
    ///
    /// By default binds the HTTP+SSE server on an OS-chosen free port *and*
    /// serves the stdio MCP transport concurrently. The bound address is
    /// printed on startup and written to `~/.trusty-memory/http_addr` so
    /// other commands and scripts can discover the running daemon without a
    /// hard-coded port. Use `--no-http` to disable the HTTP listener (e.g.
    /// when invoked by a Claude Code stdio hook).
    #[command(
        after_help = "Examples:\n  trusty-memory serve                       # OS picks a free port; address printed and written to ~/.trusty-memory/http_addr\n  trusty-memory serve --http 127.0.0.1:3031 # pin a specific port\n  trusty-memory serve --no-http             # stdio MCP only"
    )]
    Serve {
        /// HTTP bind address. Port 0 = OS picks a free port (default).
        /// Override with e.g. --http 127.0.0.1:3031
        #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:0")]
        http: SocketAddr,
        /// Disable HTTP server (MCP stdio only, for Claude Code integration).
        #[arg(long)]
        no_http: bool,
        /// stdio MCP mode (used by Claude Code hook)
        #[arg(long)]
        mcp: bool,
        /// Default palace for MCP tool calls when `palace` arg is omitted.
        ///
        /// Auto-created on startup if it does not exist. Returned in the
        /// `initialize` response under `serverInfo.default_palace` so MCP
        /// clients can verify the namespace they're bound to.
        #[arg(long, value_name = "NAME")]
        palace: Option<String>,
    },

    /// Manage the trusty-memory background service (macOS launchd).
    ///
    /// Installs a LaunchAgent plist so `trusty-memory serve` runs at login and
    /// is restarted on crash. No daemon wrapper — launchd owns the process
    /// lifecycle directly.
    #[command(
        subcommand,
        after_help = "Examples:\n  trusty-memory service install\n  trusty-memory service status\n  trusty-memory service logs\n  trusty-memory service uninstall"
    )]
    Service(ServiceCommands),

    /// Chat with an OpenRouter model using palace memory as context.
    #[command(
        after_help = "Examples:\n  trusty-memory chat \"what do you know about the auth system?\"\n  trusty-memory chat \"summarize today\" --remember\n  trusty-memory chat \"deploy steps\" --palace ops"
    )]
    Chat {
        /// Message to send to the model.
        message: String,
        /// Top-k drawers to include in the memory context.
        #[arg(short = 'k', long, default_value = "10")]
        top_k: usize,
        /// Persist the assistant response back into the palace.
        #[arg(long)]
        remember: bool,
    },

    /// Manage user configuration in `~/.trusty-memory/config.toml`.
    #[command(
        subcommand,
        after_help = "Examples:\n  trusty-memory config show\n  trusty-memory config set openrouter.api_key sk-or-..."
    )]
    Config(ConfigCommands),

    /// Machine setup wizard.
    Setup {
        #[arg(long)]
        non_interactive: bool,
        #[arg(long)]
        skip_migration: bool,
        #[arg(long)]
        migrate_only: bool,
    },

    /// Competitive retrieval benchmarking.
    #[command(subcommand)]
    Bench(BenchCommands),

    /// Convert memories from external sources (kuzu-memory, mempalace) into palaces.
    #[command(
        after_help = "Examples:\n  trusty-memory convert project\n  trusty-memory convert all --dry-run\n  trusty-memory convert project --source kuzu --palace my-app"
    )]
    Convert(ConvertArgs),

    /// Install/manage event hooks (git, Claude Code) for passive memory ingest.
    #[command(
        after_help = "Examples:\n  trusty-memory hooks install --git\n  trusty-memory hooks install --claude-code\n  trusty-memory hooks install --all\n  trusty-memory hooks list\n  trusty-memory hooks status"
    )]
    Hooks(hooks::HooksArgs),

    /// Back up a palace (or all palaces) to a `.tar.gz` archive.
    #[command(
        after_help = "Examples:\n  trusty-memory backup my-project\n  trusty-memory backup --all\n  trusty-memory backup my-project -o /tmp/my-project.tar.gz"
    )]
    Backup(backup::BackupArgs),

    /// Restore a palace from a `.tar.gz` archive.
    #[command(
        after_help = "Examples:\n  trusty-memory restore ./my-project-20260509.tar.gz\n  trusty-memory restore archive.tar.gz --palace renamed\n  trusty-memory restore archive.tar.gz --merge"
    )]
    Restore(backup::RestoreArgs),

    /// Show daemon health and palace summary.
    Status,

    /// Generate shell completions.
    #[command(
        after_help = "Examples:\n  trusty-memory completions zsh > ~/.zsh/completions/_trusty-memory\n  trusty-memory completions bash > /etc/bash_completion.d/trusty-memory"
    )]
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

// ── Subcommand groups ────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum PalaceCommands {
    /// List all palaces.
    List,
    /// Create a new palace.
    New {
        name: String,
        #[arg(long)]
        description: Option<String>,
    },
    /// Show palace details.
    Info { id: Option<String> },
    /// Delete a palace.
    Delete { name: String },
    /// Rename a palace.
    Rename { old: String, new: String },
}

#[derive(Subcommand)]
pub enum KgCommands {
    /// Assert a fact (subject predicate object).
    Assert {
        subject: String,
        predicate: String,
        object: String,
        #[arg(long, default_value = "1.0")]
        confidence: f32,
        #[arg(long)]
        provenance: Option<String>,
    },
    /// Query active triples for a subject.
    Query { subject: String },
    /// Show full interval history for a subject.
    History { subject: String },
}

#[derive(Subcommand)]
pub enum GitCommands {
    /// Extract facts from git history.
    #[command(
        after_help = "Examples:\n  trusty-memory git ingest\n  trusty-memory git ingest /path/to/repo --limit 200 --since 2024-01-01"
    )]
    Ingest {
        /// Repository path [default: current directory]
        path: Option<PathBuf>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long, default_value = "100")]
        limit: usize,
        /// Show what would be stored without storing
        #[arg(long)]
        dry_run: bool,
    },
    /// Watch for new commits and ingest automatically.
    Watch {
        path: Option<PathBuf>,
        #[arg(long, default_value = "300")]
        interval: u64,
    },
}

#[derive(Subcommand)]
pub enum KuzuCommands {
    /// Scan for kuzu-memory databases on this machine.
    Discover,
    /// Recall from a kuzu database.
    Recall {
        query: String,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Migrate kuzu databases to palaces.
    Migrate { db_path: Option<PathBuf> },
}

#[derive(Subcommand)]
pub enum AnalyticsCommands {
    /// Show recall summary.
    Show {
        #[arg(long, default_value = "7")]
        days: u32,
    },
    /// Top most-recalled drawers.
    Top {
        #[arg(short = 'k', long, default_value = "10")]
        limit: usize,
    },
}

#[derive(Subcommand)]
pub enum DecayCommands {
    /// Show drawers with largest importance gap.
    Report,
    /// Show/set decay configuration.
    Config,
}

#[derive(Subcommand)]
pub enum DreamCommands {
    /// Trigger a consolidation pass now.
    Run,
    /// Show last dream run stats.
    Status,
}

#[derive(Subcommand)]
pub enum ServiceCommands {
    /// Install and start the launchd service (macOS).
    Install,
    /// Stop and uninstall the launchd service (macOS).
    Uninstall,
    /// Show launchd status (PID, last exit code).
    Status,
    /// Print the last 50 lines of the service log.
    Logs,
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Print the resolved user configuration.
    Show,
    /// Set a dotted config key (e.g. `openrouter.api_key sk-or-...`).
    Set { key: String, value: String },
}

#[derive(Subcommand)]
pub enum BenchCommands {
    /// Compare retrieval quality across systems on a labeled corpus.
    #[command(
        after_help = "Examples:\n  trusty-memory bench compare\n  trusty-memory bench compare --mempalace --kuzu\n  trusty-memory bench compare --mempalace --json\n  trusty-memory bench compare --top-k 10"
    )]
    Compare(BenchCompareArgs),
}

#[derive(clap::Args, Debug, Clone)]
pub struct BenchCompareArgs {
    /// Path to JSONL corpus (documents + labeled queries).
    #[arg(long, default_value = "tests/fixtures/bench_corpus.jsonl")]
    pub corpus: PathBuf,

    /// K for Recall@K.
    #[arg(long, default_value = "5")]
    pub top_k: usize,

    /// Include mempalace (drives `mempalace-mcp` against a fresh palace).
    #[arg(long)]
    pub mempalace: bool,

    /// Include kuzu-memory (drives `kuzu-memory mcp` in a fresh project dir).
    #[arg(long)]
    pub kuzu: bool,

    /// Emit machine-readable JSON metrics instead of the formatted table.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for the `convert` subcommand.
///
/// Why: Migrates pre-existing memories (kuzu-memory DBs, mempalace WAL) into
/// trusty-memory palaces so users don't lose history when adopting the new
/// service.
/// What: Carries the scope (single project vs whole machine), source filter,
/// dry-run flag, and palace-name override.
/// Test: Parse coverage via clap; behavioral tests live in `convert.rs`.
#[derive(clap::Args, Debug, Clone)]
pub struct ConvertArgs {
    /// Scope: "project" (current dir) or "all" (whole machine).
    #[arg(value_enum)]
    pub scope: ConvertScope,

    /// Which source to convert from.
    #[arg(long, value_enum, default_value = "all")]
    pub source: ConvertSource,

    /// Dry run — print what would be imported without writing.
    #[arg(long)]
    pub dry_run: bool,

    /// Override palace name (project mode only).
    #[arg(long)]
    pub palace: Option<String>,
}

#[derive(ValueEnum, Clone, Debug)]
pub enum ConvertScope {
    Project,
    All,
}

#[derive(ValueEnum, Clone, Debug, PartialEq, Eq)]
pub enum ConvertSource {
    Kuzu,
    Mempalace,
    All,
}
