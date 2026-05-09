//! `git` subcommand handler — extract facts from git history.
//!
//! Why: Git history is a rich source of project facts; the `ingest` command
//! drives the NLP-based extractor in `trusty-memory-core::git`.
//! What: Walks git history via `GitExtractor`, emits a `GitFact` per commit,
//! and persists each one as a `Drawer` via `PalaceHandle::remember` (unless
//! `--dry-run`).
//! Test: Run against the trusty-memory repo itself; expect non-empty fact list.

use crate::cli::memory::open_or_create_handle;
use crate::cli::output::OutputConfig;
use crate::cli::GitCommands;
use anyhow::Result;
use trusty_memory_core::git::GitExtractor;
use trusty_memory_core::RoomType;

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

            if dry_run {
                for fact in &facts {
                    let preview_len = fact.narrative.len().min(80);
                    let preview = &fact.narrative[..preview_len];
                    println!("  [{:.2}] would store {preview}", fact.importance);
                }
                out.print_success("dry-run complete");
                return Ok(());
            }

            let handle = open_or_create_handle(palace).await?;
            let mut stored = 0usize;
            for fact in &facts {
                let tags = fact_tags(fact);
                let room = fact
                    .entities
                    .room_types
                    .first()
                    .cloned()
                    .unwrap_or(RoomType::General);
                handle
                    .remember(fact.narrative.clone(), room, tags, fact.importance)
                    .await?;
                stored += 1;
            }
            out.print_success(&format!("ingested {stored} commit(s)"));
        }
        GitCommands::Watch { path: _, interval } => {
            println!("Watching for new commits every {interval}s (full impl in #12 wiring)");
        }
    }
    Ok(())
}

/// Build drawer tags from a `GitFact`.
///
/// Why: Re-uses `GitFact::to_drawer`'s tag schema without going through the
/// drawer conversion (we already build a fresh Drawer inside `remember`).
/// What: Returns `["git:<sha>", "author:<name>", "type:<t>", "scope:<s>",
/// "issue:<n>", ...]`.
/// Test: Indirectly via `git ingest` integration.
fn fact_tags(fact: &trusty_memory_core::git::GitFact) -> Vec<String> {
    let mut tags = vec![
        format!("git:{}", &fact.sha[..fact.sha.len().min(8)]),
        format!("author:{}", fact.author),
    ];
    if !fact.conventional.commit_type.is_empty() {
        tags.push(format!("type:{}", fact.conventional.commit_type));
    }
    if let Some(scope) = &fact.conventional.scope {
        tags.push(format!("scope:{scope}"));
    }
    for issue in &fact.entities.issue_refs {
        tags.push(format!("issue:{issue}"));
    }
    tags
}
