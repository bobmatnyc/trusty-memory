//! `git` subcommand handler — extract facts from git history.
//!
//! Why: Git history is a rich source of project facts; the `ingest` command
//! drives the NLP-based extractor in `trusty-memory-core::git`.
//! What: Wires `GitExtractor` to print a preview of extracted facts; persistence
//! to a palace lands once the registry is wired in.
//! Test: Run against the trusty-memory repo itself; expect non-empty fact list.

use crate::cli::output::OutputConfig;
use crate::cli::GitCommands;
use anyhow::Result;
use trusty_memory_core::git::GitExtractor;

pub async fn handle(cmd: GitCommands, palace: &str, out: &OutputConfig) -> Result<()> {
    match cmd {
        GitCommands::Ingest {
            path,
            since: _,
            limit,
            dry_run,
        } => {
            let repo_path = match path {
                Some(p) => p,
                None => std::env::current_dir()?,
            };
            out.print_header(palace, "git/ingest");
            let extractor = GitExtractor::new(repo_path)?;
            let facts = extractor.extract(None, limit)?;
            println!("Extracted {} facts from git history", facts.len());
            for fact in &facts {
                let action = if dry_run { "would store" } else { "storing" };
                let preview_len = fact.narrative.len().min(80);
                let preview = &fact.narrative[..preview_len];
                println!("  [{:.2}] {action} {preview}", fact.importance);
            }
            if !dry_run {
                println!("(registry wiring pending — facts not yet persisted)");
            }
        }
        GitCommands::Watch { path: _, interval } => {
            println!("Watching for new commits every {interval}s (full impl in #12 wiring)");
        }
    }
    Ok(())
}
