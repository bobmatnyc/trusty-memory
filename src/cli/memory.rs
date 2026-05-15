//! Top-level memory commands: remember / recall / forget / list.
//!
//! Why: These four verbs are the public face of the CLI; centralizing their
//! handlers next to the palace plumbing keeps `main.rs` declarative and lets
//! tests target each handler directly.
//! What: Async handlers that resolve a palace handle, dispatch to the core
//! API, and print results in either human or JSON mode.
//! Test: `cli_remember_and_recall`, `cli_forget_removes_drawer`, and
//! `cli_list_filters_by_room` integration tests.

use crate::cli::output::OutputConfig;
use crate::cli::palace::data_root;
use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use trusty_memory_core::retrieval::{
    recall_across_palaces_with_default_embedder, recall_deep_with_default_embedder,
    recall_with_default_embedder, PalaceHandle, RecallResult,
};
use trusty_memory_core::{Palace, PalaceId, PalaceRegistry, RoomType};
use uuid::Uuid;

/// Resolve a palace handle, auto-creating it on first use.
///
/// Why: Every memory verb needs a hydrated `PalaceHandle`; this collapses the
/// open-or-create bookkeeping into a single helper.
/// What: Looks up the palace via `PalaceRegistry::open_palace`; on miss, falls
/// back to `create_palace` with a default `Palace` row.
/// Test: Exercised end-to-end by `cli_remember_and_recall`.
pub async fn open_or_create_handle(palace_id: &str) -> Result<Arc<PalaceHandle>> {
    let root = data_root()?;
    let id = PalaceId::new(palace_id.to_string());
    let name = palace_id.to_string();
    let root_clone = root.clone();
    let id_clone = id.clone();
    let handle = tokio::task::spawn_blocking(move || -> Result<Arc<PalaceHandle>> {
        let reg = PalaceRegistry::new();
        if let Ok(h) = reg.open_palace(&root_clone, &id_clone) {
            return Ok(h);
        }
        let p = Palace {
            id: id_clone.clone(),
            name,
            description: None,
            created_at: Utc::now(),
            data_dir: root_clone.join(id_clone.as_str()),
        };
        reg.create_palace(&root_clone, p)
            .context("create palace on first use")
    })
    .await
    .context("join open_or_create palace")??;
    Ok(handle)
}

/// `remember` — store a new drawer.
pub async fn handle_remember(
    palace: &str,
    text: String,
    room: String,
    tags: Vec<String>,
    importance: f32,
    out: &OutputConfig,
) -> Result<()> {
    out.print_header(palace, &room);
    let handle = open_or_create_handle(palace).await?;
    let room_type = RoomType::parse(&room);
    let id = handle
        .remember(text, room_type, tags, importance)
        .await
        .context("remember drawer")?;
    if out.json {
        let v = json!({"drawer_id": id.to_string(), "palace": palace});
        out.print_json(&v);
    } else {
        println!("drawer_id: {id}");
        out.print_success("stored");
    }
    Ok(())
}

/// `recall` — search memories.
pub async fn handle_recall(
    palace: &str,
    query: String,
    top_k: usize,
    room: Option<String>,
    deep: bool,
    out: &OutputConfig,
) -> Result<()> {
    out.print_header(palace, room.as_deref().unwrap_or("all"));
    let handle = open_or_create_handle(palace).await?;
    let started = std::time::Instant::now();
    let results: Vec<RecallResult> = if deep {
        recall_deep_with_default_embedder(&handle, &query, top_k).await?
    } else {
        recall_with_default_embedder(&handle, &query, top_k).await?
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;

    if out.json {
        let arr: Vec<_> = results
            .iter()
            .map(|r| {
                json!({
                    "drawer_id": r.drawer.id.to_string(),
                    "score": r.score,
                    "layer": r.layer,
                    "content": r.drawer.content,
                    "importance": r.drawer.importance,
                    "tags": r.drawer.tags,
                })
            })
            .collect();
        out.print_json(&json!({"results": arr}));
    } else {
        for r in &results {
            let preview_len = r.drawer.content.len().min(120);
            let preview = &r.drawer.content[..preview_len];
            let room_label = format!("L{}", r.layer);
            println!("[{:.3}] [{}] {}", r.score, room_label, preview);
        }
        let layer = if deep { "L3" } else { "L2" };
        out.print_footer(results.len(), layer, elapsed_ms);
    }
    Ok(())
}

/// `recall --all-palaces` — fan a recall across every palace on this machine
/// and present a single ranked, deduplicated result list.
///
/// Why: Agents often don't care which palace a fact lives in; they want the
/// most relevant memories regardless of namespace. Forcing the operator to
/// repeat a recall per palace is slow and produces no merged ranking. This
/// helper opens every persisted palace, fans out concurrently, and prints the
/// top-k with a `[palace: <id>]` prefix so the source is still obvious.
/// What: Lists palaces via `PalaceRegistry::list_palaces`, opens a handle for
/// each (skipping failures), and delegates to
/// `recall_across_palaces_with_default_embedder`. Honours both human and JSON
/// output modes.
/// Test: Exercised end-to-end via `cargo run -- recall <q> --all-palaces`;
/// core merge logic is covered by `recall_across_palaces_merges_results`.
pub async fn handle_recall_all(
    query: String,
    top_k: usize,
    deep: bool,
    out: &OutputConfig,
) -> Result<()> {
    out.print_header("(all palaces)", "all");
    let root = data_root()?;

    // List + open palaces on a blocking thread — the registry calls are
    // synchronous and we keep the async reactor free for the embedder load.
    let handles = tokio::task::spawn_blocking(move || -> Result<Vec<Arc<PalaceHandle>>> {
        let palaces = PalaceRegistry::list_palaces(&root).context("list palaces")?;
        if palaces.is_empty() {
            return Ok(Vec::new());
        }
        let reg = PalaceRegistry::new();
        let mut out = Vec::with_capacity(palaces.len());
        for p in &palaces {
            match reg.open_palace(&root, &p.id) {
                Ok(h) => out.push(h),
                Err(e) => tracing::warn!(palace = %p.id, "open failed, skipping: {e:#}"),
            }
        }
        Ok(out)
    })
    .await
    .context("join open_all_palaces")??;

    if handles.is_empty() {
        if out.json {
            let empty: Vec<serde_json::Value> = Vec::new();
            out.print_json(&json!({"results": empty}));
        } else {
            println!("(no palaces on this machine)");
        }
        return Ok(());
    }

    let started = std::time::Instant::now();
    let results = recall_across_palaces_with_default_embedder(&handles, &query, top_k, deep)
        .await
        .context("recall_across_palaces")?;
    let elapsed_ms = started.elapsed().as_millis() as u64;

    if out.json {
        let arr: Vec<_> = results
            .iter()
            .map(|r| {
                json!({
                    "palace_id":  r.palace_id,
                    "drawer_id":  r.result.drawer.id.to_string(),
                    "score":      r.result.score,
                    "layer":      r.result.layer,
                    "content":    r.result.drawer.content,
                    "importance": r.result.drawer.importance,
                    "tags":       r.result.drawer.tags,
                })
            })
            .collect();
        out.print_json(&json!({"results": arr}));
    } else {
        for r in &results {
            let preview_len = r.result.drawer.content.len().min(120);
            let preview = &r.result.drawer.content[..preview_len];
            println!(
                "[{:.3}] [L{}] [palace: {}] {}",
                r.result.score, r.result.layer, r.palace_id, preview
            );
        }
        let layer = if deep { "L3" } else { "L2" };
        out.print_footer(results.len(), layer, elapsed_ms);
    }
    Ok(())
}

/// `forget` — remove a drawer by UUID.
pub async fn handle_forget(palace: &str, id_str: &str, out: &OutputConfig) -> Result<()> {
    let id = Uuid::parse_str(id_str).with_context(|| format!("invalid UUID: {id_str}"))?;
    let handle = open_or_create_handle(palace).await?;
    handle.forget(id).await?;
    if out.json {
        out.print_json(&json!({"drawer_id": id.to_string(), "status": "forgotten"}));
    } else {
        out.print_success(&format!("forgot {id}"));
    }
    Ok(())
}

/// `list` — list drawers with optional filters.
pub async fn handle_list(
    palace: &str,
    limit: usize,
    room: Option<String>,
    _sort: String,
    out: &OutputConfig,
) -> Result<()> {
    out.print_header(palace, room.as_deref().unwrap_or("all"));
    let handle = open_or_create_handle(palace).await?;
    let room_filter = room.as_deref().map(RoomType::parse);
    let drawers = handle.list_drawers(room_filter, None, limit);

    if out.json {
        let arr: Vec<_> = drawers
            .iter()
            .map(|d| {
                json!({
                    "drawer_id": d.id.to_string(),
                    "content": d.content,
                    "importance": d.importance,
                    "tags": d.tags,
                    "created_at": d.created_at.to_rfc3339(),
                })
            })
            .collect();
        out.print_json(&json!({"drawers": arr}));
    } else {
        for d in &drawers {
            let preview_len = d.content.len().min(120);
            let preview = &d.content[..preview_len];
            println!("[{:.2}] {} {}", d.importance, d.id, preview);
        }
        out.print_footer(drawers.len(), "list", 0);
    }
    Ok(())
}
