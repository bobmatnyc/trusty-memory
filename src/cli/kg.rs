//! `kg` subcommand handler — temporal knowledge graph operations.
//!
//! Why: Surface `assert` / `query` / `history` for the SQLite KG.
//! What: Wires the active palace's `KnowledgeGraph` handle to the CLI verbs.
//! Test: Behavior covered by core `kg::tests`; CLI parse covered by
//! `cli_help_exits_zero`.

use crate::cli::memory::open_or_create_handle;
use crate::cli::output::OutputConfig;
use crate::cli::KgCommands;
use anyhow::Result;
use chrono::Utc;
use trusty_memory_core::store::kg::Triple;

pub async fn handle(cmd: KgCommands, palace: &str, out: &OutputConfig) -> Result<()> {
    match cmd {
        KgCommands::Assert {
            subject,
            predicate,
            object,
            confidence,
            provenance,
        } => {
            out.print_header(palace, "kg/assert");
            let handle = open_or_create_handle(palace).await?;
            let triple = Triple {
                subject: subject.clone(),
                predicate: predicate.clone(),
                object: object.clone(),
                valid_from: Utc::now(),
                valid_to: None,
                confidence,
                provenance: provenance.clone(),
            };
            handle.kg.assert(triple).await?;
            println!("({subject}) -[{predicate}]-> ({object})");
            println!("  confidence: {confidence}");
            if let Some(p) = provenance {
                println!("  provenance: {p}");
            }
            out.print_success("asserted");
        }
        KgCommands::Query { subject } => {
            out.print_header(palace, "kg/query");
            let handle = open_or_create_handle(palace).await?;
            let triples = handle.kg.query_active(&subject).await?;
            if triples.is_empty() {
                println!("(no active triples for '{subject}')");
            } else {
                for t in &triples {
                    println!(
                        "{} -[{}]-> {} (conf={:.2})",
                        t.subject, t.predicate, t.object, t.confidence
                    );
                }
                out.print_footer(triples.len(), "kg", 0);
            }
        }
        KgCommands::History { subject } => {
            // History is the same as query_active for now; full interval
            // history requires an additional store call we haven't added yet.
            out.print_header(palace, "kg/history");
            let handle = open_or_create_handle(palace).await?;
            let triples = handle.kg.query_active(&subject).await?;
            for t in &triples {
                let until = match &t.valid_to {
                    Some(t) => t.to_rfc3339(),
                    None => "now".to_string(),
                };
                println!(
                    "[{} → {}] {} -[{}]-> {} (conf={:.2})",
                    t.valid_from.to_rfc3339(),
                    until,
                    t.subject,
                    t.predicate,
                    t.object,
                    t.confidence,
                );
            }
        }
    }
    Ok(())
}
