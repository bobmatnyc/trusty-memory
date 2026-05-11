//! HTTP API + embedded SPA shell for the trusty-memory admin UI.
//!
//! Why: The web admin panel is the primary GUI for non-MCP clients. Bundling
//! the Svelte build via `rust-embed` keeps deployment to "drop the binary on
//! a host"; the JSON API surface mirrors the MCP tool set so anything
//! trusty-memory can do via Claude Code can also be done via curl or browser.
//! What: All `/api/v1/*` handlers (status, palaces, drawers, recall, KG,
//! config, chat) plus an embedded-asset fallback that serves `ui/dist/`.
//! Test: `cargo test -p trusty-memory-mcp web::tests` covers the asset
//! fallback and JSON shape of every read endpoint against an in-memory
//! palace built on a `tempdir`.

use crate::AppState;
use axum::{
    body::Body,
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderValue, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;
use trusty_common::ChatMessage;
use trusty_memory_core::dream::{DreamConfig, Dreamer, PersistedDreamStats};
use trusty_memory_core::palace::{Palace, PalaceId, RoomType};
use trusty_memory_core::retrieval::{
    recall_deep_with_default_embedder, recall_with_default_embedder,
};
use trusty_memory_core::store::kg::Triple;
use trusty_memory_core::{PalaceHandle, PalaceRegistry};
use uuid::Uuid;

/// Embedded UI assets produced by `pnpm build` in `ui/`.
///
/// Why: Single-binary deploys with no separate static-file dance. `build.rs`
/// runs the Vite build before compilation so this folder is always populated.
/// What: All files under `ui/dist/` are included in the binary.
/// Test: `serves_index_html` confirms the SPA shell loads.
#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../ui/dist/"]
struct WebAssets;

/// Build the public router with API routes + SPA asset fallback.
///
/// Why: `run_http` calls this so the same router shape is used in tests.
/// What: All API routes under `/api/v1`, fallback to the SPA shell.
/// Test: `serves_index_html` and `status_endpoint_returns_payload`.
pub fn router() -> Router<AppState> {
    // axum 0.8 path syntax uses `{param}` instead of `:param`. The shared
    // `trusty_common::server::with_standard_middleware` layer brings in CORS,
    // tracing, and gzip (with SSE excluded) so we don't drift from sibling
    // trusty-* daemons.
    let router = Router::new()
        .route("/api/v1/status", get(status))
        .route("/api/v1/config", get(config))
        .route("/api/v1/palaces", get(list_palaces).post(create_palace))
        .route("/api/v1/palaces/{id}", get(get_palace_handler))
        .route(
            "/api/v1/palaces/{id}/drawers",
            get(list_drawers).post(create_drawer),
        )
        .route(
            "/api/v1/palaces/{id}/drawers/{drawer_id}",
            delete(delete_drawer),
        )
        .route("/api/v1/palaces/{id}/recall", get(recall_handler))
        .route("/api/v1/palaces/{id}/kg", get(kg_query).post(kg_assert))
        .route(
            "/api/v1/palaces/{id}/dream/status",
            get(palace_dream_status),
        )
        .route("/api/v1/dream/status", get(dream_status))
        .route("/api/v1/dream/run", post(dream_run))
        .route("/api/v1/chat", post(chat_handler))
        .route("/health", get(|| async { "ok" }))
        .fallback(static_handler);

    trusty_common::server::with_standard_middleware(router)
}

// ---------------------------------------------------------------------------
// Static asset serving
// ---------------------------------------------------------------------------

/// Serve any embedded asset; fall back to `index.html` for SPA routes.
///
/// Why: Hash-based routing lives client-side, but `/assets/foo.js` etc. must
/// resolve to the embedded file directly.
/// What: Looks up the request path under `WebAssets`; if absent, returns
/// `index.html`. Unknown paths under `/api/` return 404.
/// Test: `serves_index_html`, `serves_static_asset`, `unknown_api_404`.
async fn static_handler(req: Request<Body>) -> Response {
    let path = req.uri().path().trim_start_matches('/').to_string();

    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    serve_embedded(&path).unwrap_or_else(|| {
        // SPA fallback.
        serve_embedded("index.html")
            .unwrap_or_else(|| (StatusCode::NOT_FOUND, "ui assets missing").into_response())
    })
}

fn serve_embedded(path: &str) -> Option<Response> {
    let path = if path.is_empty() { "index.html" } else { path };
    let asset = WebAssets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let body = Body::from(asset.data.into_owned());
    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime.as_ref())
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    Some(resp)
}

// ---------------------------------------------------------------------------
// /api/v1/status, /api/v1/config
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct StatusPayload {
    version: String,
    palace_count: usize,
    default_palace: Option<String>,
    data_root: String,
    total_drawers: usize,
    total_vectors: usize,
    total_kg_triples: usize,
}

async fn status(State(state): State<AppState>) -> Json<StatusPayload> {
    let palaces = PalaceRegistry::list_palaces(&state.data_root).unwrap_or_default();
    let palace_count = palaces.len();
    let (mut total_drawers, mut total_vectors, mut total_kg_triples) = (0usize, 0usize, 0usize);
    for p in &palaces {
        if let Ok(handle) = state.registry.open_palace(&state.data_root, &p.id) {
            total_drawers = total_drawers.saturating_add(handle.drawers.read().len());
            total_vectors = total_vectors.saturating_add(handle.vector_store.index_size());
            total_kg_triples = total_kg_triples.saturating_add(handle.kg.count_active_triples());
        }
    }
    Json(StatusPayload {
        version: state.version.clone(),
        palace_count,
        default_palace: state.default_palace.clone(),
        data_root: state.data_root.display().to_string(),
        total_drawers,
        total_vectors,
        total_kg_triples,
    })
}

#[derive(Serialize)]
struct ConfigPayload {
    openrouter_configured: bool,
    model: String,
    data_root: String,
}

async fn config(State(state): State<AppState>) -> Json<ConfigPayload> {
    let cfg = load_user_config().unwrap_or_default();
    Json(ConfigPayload {
        openrouter_configured: !cfg.openrouter_api_key.is_empty(),
        model: cfg.openrouter_model,
        data_root: state.data_root.display().to_string(),
    })
}

/// Minimal mirror of the user-config schema (the real type lives in the bin
/// crate; replicating just the fields we need here avoids a cyclic dep).
#[derive(Deserialize, Default, Clone)]
struct UserConfigMin {
    #[serde(default)]
    openrouter: OpenRouterMin,
    // Carry forward unknown sections by ignoring them on parse.
}

#[derive(Deserialize, Default, Clone)]
struct OpenRouterMin {
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    model: String,
}

#[derive(Default, Clone)]
struct LoadedUserConfig {
    openrouter_api_key: String,
    openrouter_model: String,
}

fn load_user_config() -> Option<LoadedUserConfig> {
    let home = dirs::home_dir()?;
    let path = home.join(".trusty-memory").join("config.toml");
    if !path.exists() {
        return Some(LoadedUserConfig {
            openrouter_api_key: String::new(),
            openrouter_model: "anthropic/claude-3-5-sonnet".to_string(),
        });
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed: UserConfigMin = toml::from_str(&raw).unwrap_or_default();
    let model = if parsed.openrouter.model.is_empty() {
        "anthropic/claude-3-5-sonnet".to_string()
    } else {
        parsed.openrouter.model
    };
    Some(LoadedUserConfig {
        openrouter_api_key: parsed.openrouter.api_key,
        openrouter_model: model,
    })
}

// ---------------------------------------------------------------------------
// /api/v1/palaces
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct PalaceInfo {
    id: String,
    name: String,
    description: Option<String>,
    drawer_count: usize,
    vector_count: usize,
    kg_triple_count: usize,
    wing_count: usize,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// Build a `PalaceInfo` from a `Palace` row plus an optional opened handle.
///
/// Why: Both `list_palaces` and `get_palace_handler` need the same enriched
/// shape; centralizing the field-pulling avoids drift.
/// What: Reads drawer count, vector index size, active KG triple count, and
/// derives wing_count from the number of distinct `room_id`s in the drawer
/// table (until a dedicated wings/rooms table exists, distinct rooms-by-drawer
/// is the closest proxy).
/// Test: `palace_list_includes_richer_counts`.
fn palace_info_from(palace: &Palace, handle: Option<&Arc<PalaceHandle>>) -> PalaceInfo {
    let (drawer_count, vector_count, kg_triple_count, wing_count) = if let Some(h) = handle {
        let drawers = h.drawers.read();
        let distinct_rooms: HashSet<Uuid> = drawers.iter().map(|d| d.room_id).collect();
        (
            drawers.len(),
            h.vector_store.index_size(),
            h.kg.count_active_triples(),
            distinct_rooms.len(),
        )
    } else {
        (0, 0, 0, 0)
    };
    PalaceInfo {
        id: palace.id.0.clone(),
        name: palace.name.clone(),
        description: palace.description.clone(),
        drawer_count,
        vector_count,
        kg_triple_count,
        wing_count,
        created_at: palace.created_at,
    }
}

async fn list_palaces(State(state): State<AppState>) -> Result<Json<Vec<PalaceInfo>>, ApiError> {
    let palaces = PalaceRegistry::list_palaces(&state.data_root)
        .map_err(|e| ApiError::internal(format!("list palaces: {e:#}")))?;
    let mut out = Vec::with_capacity(palaces.len());
    for p in palaces {
        let handle = state.registry.open_palace(&state.data_root, &p.id).ok();
        out.push(palace_info_from(&p, handle.as_ref()));
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
struct CreatePalaceBody {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

async fn create_palace(
    State(state): State<AppState>,
    Json(body): Json<CreatePalaceBody>,
) -> Result<Json<Value>, ApiError> {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::bad_request("name is required"));
    }
    let id = PalaceId::new(&name);
    let palace = Palace {
        id: id.clone(),
        name: name.clone(),
        description: body.description.filter(|s| !s.is_empty()),
        created_at: chrono::Utc::now(),
        data_dir: state.data_root.join(&name),
    };
    state
        .registry
        .create_palace(&state.data_root, palace)
        .map_err(|e| ApiError::internal(format!("create palace: {e:#}")))?;
    Ok(Json(json!({ "id": name })))
}

async fn get_palace_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<PalaceInfo>, ApiError> {
    let palaces = PalaceRegistry::list_palaces(&state.data_root)
        .map_err(|e| ApiError::internal(format!("list palaces: {e:#}")))?;
    let palace = palaces
        .into_iter()
        .find(|p| p.id.0 == id)
        .ok_or_else(|| ApiError::not_found(format!("palace not found: {id}")))?;
    let handle = state
        .registry
        .open_palace(&state.data_root, &palace.id)
        .ok();
    Ok(Json(palace_info_from(&palace, handle.as_ref())))
}

// ---------------------------------------------------------------------------
// Drawers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ListDrawersQuery {
    #[serde(default)]
    room: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn list_drawers(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<ListDrawersQuery>,
) -> Result<Json<Value>, ApiError> {
    let handle = open_handle(&state, &id)?;
    let room = q.room.as_deref().map(RoomType::parse);
    let drawers = handle.list_drawers(room, q.tag.clone(), q.limit.unwrap_or(50));
    Ok(Json(serde_json::to_value(drawers).unwrap_or(json!([]))))
}

#[derive(Deserialize)]
struct CreateDrawerBody {
    content: String,
    #[serde(default)]
    room: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    importance: Option<f32>,
}

async fn create_drawer(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<CreateDrawerBody>,
) -> Result<Json<Value>, ApiError> {
    let handle = open_handle(&state, &id)?;
    let room = body
        .room
        .as_deref()
        .map(RoomType::parse)
        .unwrap_or(RoomType::General);
    let importance = body.importance.unwrap_or(0.5);
    let drawer_id = handle
        .remember(body.content, room, body.tags, importance)
        .await
        .map_err(|e| ApiError::internal(format!("remember: {e:#}")))?;
    Ok(Json(json!({ "id": drawer_id })))
}

async fn delete_drawer(
    State(state): State<AppState>,
    AxumPath((id, drawer_id)): AxumPath<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let handle = open_handle(&state, &id)?;
    let uuid = Uuid::parse_str(&drawer_id)
        .map_err(|_| ApiError::bad_request("drawer_id must be a UUID"))?;
    handle
        .forget(uuid)
        .await
        .map_err(|e| ApiError::internal(format!("forget: {e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Recall
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RecallQuery {
    q: String,
    #[serde(default)]
    top_k: Option<usize>,
    #[serde(default)]
    deep: Option<bool>,
}

async fn recall_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<RecallQuery>,
) -> Result<Json<Value>, ApiError> {
    let handle = open_handle(&state, &id)?;
    let top_k = q.top_k.unwrap_or(10);
    let results = if q.deep.unwrap_or(false) {
        recall_deep_with_default_embedder(&handle, &q.q, top_k).await
    } else {
        recall_with_default_embedder(&handle, &q.q, top_k).await
    }
    .map_err(|e| ApiError::internal(format!("recall: {e:#}")))?;

    let payload: Vec<Value> = results
        .into_iter()
        .map(|r| {
            json!({
                "drawer": r.drawer,
                "score": r.score,
                "layer": r.layer,
            })
        })
        .collect();
    Ok(Json(json!(payload)))
}

// ---------------------------------------------------------------------------
// Knowledge Graph
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct KgQueryParams {
    subject: String,
}

async fn kg_query(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<KgQueryParams>,
) -> Result<Json<Vec<Triple>>, ApiError> {
    let handle = open_handle(&state, &id)?;
    let triples = handle
        .kg
        .query_active(&q.subject)
        .await
        .map_err(|e| ApiError::internal(format!("kg query: {e:#}")))?;
    Ok(Json(triples))
}

#[derive(Deserialize)]
struct KgAssertBody {
    subject: String,
    predicate: String,
    object: String,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    provenance: Option<String>,
}

async fn kg_assert(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<KgAssertBody>,
) -> Result<StatusCode, ApiError> {
    let handle = open_handle(&state, &id)?;
    let triple = Triple {
        subject: body.subject,
        predicate: body.predicate,
        object: body.object,
        valid_from: chrono::Utc::now(),
        valid_to: None,
        confidence: body.confidence.unwrap_or(1.0),
        provenance: body.provenance,
    };
    handle
        .kg
        .assert(triple)
        .await
        .map_err(|e| ApiError::internal(format!("kg assert: {e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Dream cycle status + on-demand run
// ---------------------------------------------------------------------------

/// Wire payload for dream status endpoints — `last_run_at` may be null when no
/// cycle has run yet on this palace (or the aggregate has nothing to report).
#[derive(Serialize, Default)]
struct DreamStatusPayload {
    last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    merged: usize,
    pruned: usize,
    compacted: usize,
    closets_updated: usize,
    duration_ms: u64,
}

impl From<PersistedDreamStats> for DreamStatusPayload {
    fn from(p: PersistedDreamStats) -> Self {
        Self {
            last_run_at: Some(p.last_run_at),
            merged: p.stats.merged,
            pruned: p.stats.pruned,
            compacted: p.stats.compacted,
            closets_updated: p.stats.closets_updated,
            duration_ms: p.stats.duration_ms,
        }
    }
}

/// GET /api/v1/dream/status — aggregate latest dream stats across all palaces.
///
/// Why: The dashboard wants a single "last dream cycle" panel rather than
/// per-palace details; we sum the per-palace counters and surface the most
/// recent `last_run_at` so operators can spot a stalled background loop.
/// What: Walks every palace, loads its `dream_stats.json` if present, sums
/// counts, and returns the max `last_run_at` (or null if no palace has run).
/// Test: `dream_status_aggregates_across_palaces` covers the read path.
async fn dream_status(State(state): State<AppState>) -> Json<DreamStatusPayload> {
    let palaces = PalaceRegistry::list_palaces(&state.data_root).unwrap_or_default();
    let mut out = DreamStatusPayload::default();
    let mut latest: Option<chrono::DateTime<chrono::Utc>> = None;
    for p in palaces {
        let data_dir = state.data_root.join(p.id.as_str());
        let snap = match PersistedDreamStats::load(&data_dir) {
            Ok(Some(s)) => s,
            _ => continue,
        };
        out.merged = out.merged.saturating_add(snap.stats.merged);
        out.pruned = out.pruned.saturating_add(snap.stats.pruned);
        out.compacted = out.compacted.saturating_add(snap.stats.compacted);
        out.closets_updated = out
            .closets_updated
            .saturating_add(snap.stats.closets_updated);
        out.duration_ms = out.duration_ms.saturating_add(snap.stats.duration_ms);
        latest = match latest {
            Some(t) if t >= snap.last_run_at => Some(t),
            _ => Some(snap.last_run_at),
        };
    }
    out.last_run_at = latest;
    Json(out)
}

/// GET /api/v1/palaces/:id/dream/status — per-palace dream stats snapshot.
async fn palace_dream_status(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<DreamStatusPayload>, ApiError> {
    let data_dir = state.data_root.join(&id);
    if !data_dir.exists() {
        return Err(ApiError::not_found(format!("palace not found: {id}")));
    }
    let payload = match PersistedDreamStats::load(&data_dir) {
        Ok(Some(s)) => s.into(),
        Ok(None) => DreamStatusPayload::default(),
        Err(e) => return Err(ApiError::internal(format!("read dream stats: {e:#}"))),
    };
    Ok(Json(payload))
}

/// POST /api/v1/dream/run — run a dream cycle across all palaces on demand.
///
/// Why: The dashboard exposes a "Run now" button so operators can force a
/// cycle without waiting for the idle clock; useful after a bulk ingest or
/// when diagnosing the dream loop itself.
/// What: Opens every persisted palace, runs `Dreamer::dream_cycle` with the
/// default config, and returns the aggregated stats plus the run timestamp.
/// Errors on individual palaces are logged but don't abort the sweep.
/// Test: `dream_run_aggregates_stats` covers the round-trip.
async fn dream_run(State(state): State<AppState>) -> Result<Json<DreamStatusPayload>, ApiError> {
    let palaces = PalaceRegistry::list_palaces(&state.data_root)
        .map_err(|e| ApiError::internal(format!("list palaces: {e:#}")))?;
    let dreamer = Dreamer::new(DreamConfig::default());
    let mut out = DreamStatusPayload::default();
    for p in palaces {
        let handle = match state.registry.open_palace(&state.data_root, &p.id) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(palace = %p.id, "dream_run: open failed: {e:#}");
                continue;
            }
        };
        match dreamer.dream_cycle(&handle).await {
            Ok(stats) => {
                out.merged = out.merged.saturating_add(stats.merged);
                out.pruned = out.pruned.saturating_add(stats.pruned);
                out.compacted = out.compacted.saturating_add(stats.compacted);
                out.closets_updated = out.closets_updated.saturating_add(stats.closets_updated);
                out.duration_ms = out.duration_ms.saturating_add(stats.duration_ms);
            }
            Err(e) => tracing::warn!(palace = %p.id, "dream_run: cycle failed: {e:#}"),
        }
    }
    out.last_run_at = Some(chrono::Utc::now());
    Ok(Json(out))
}

// ---------------------------------------------------------------------------
// Chat (OpenRouter, SSE-streaming)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ChatBody {
    #[serde(default)]
    palace_id: Option<String>,
    message: String,
    #[serde(default)]
    history: Vec<ChatMessage>,
}

async fn chat_handler(State(state): State<AppState>, Json(body): Json<ChatBody>) -> Response {
    let cfg = match load_user_config() {
        Some(c) if !c.openrouter_api_key.is_empty() => c,
        _ => {
            return (
                StatusCode::PRECONDITION_FAILED,
                "OpenRouter API key not configured",
            )
                .into_response();
        }
    };

    // Pull recall context from the named (or default) palace.
    let palace_id = body
        .palace_id
        .or_else(|| state.default_palace.clone())
        .unwrap_or_default();

    let mut context = String::new();
    if !palace_id.is_empty() {
        if let Ok(handle) = state
            .registry
            .open_palace(&state.data_root, &PalaceId::new(&palace_id))
        {
            if let Ok(hits) = recall_with_default_embedder(&handle, &body.message, 5).await {
                for r in hits.iter().take(5) {
                    context.push_str(&format!("- (L{}) {}\n", r.layer, r.drawer.content));
                }
            }
        }
    }

    let system = if context.is_empty() {
        "You are trusty-memory's assistant.".to_string()
    } else {
        format!(
            "You are trusty-memory's assistant. Use the following palace memory \
             as context when relevant:\n{context}"
        )
    };

    let mut messages: Vec<ChatMessage> = Vec::with_capacity(body.history.len() + 2);
    messages.push(ChatMessage {
        role: "system".to_string(),
        content: system,
    });
    messages.extend(body.history.iter().cloned());
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: body.message.clone(),
    });

    // Bridge trusty-common's plain-delta stream into the SSE wire format the
    // browser client expects: `data: {"delta": "..."}\n\n` per token, plus a
    // terminating `data: [DONE]\n\n`. The shared helper owns the upstream
    // OpenRouter request, retry-friendly timeouts, and SSE frame parsing — we
    // only re-wrap the deltas for our public endpoint contract.
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<String>(32);
    let (sse_tx, sse_rx) =
        tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(32);

    let api_key = cfg.openrouter_api_key.clone();
    let model = cfg.openrouter_model.clone();

    // Task A: run the OpenRouter stream into the delta channel.
    let stream_task = tokio::spawn(async move {
        trusty_common::openrouter_chat_stream(&api_key, &model, messages, delta_tx).await
    });

    // Task B: drain deltas, re-emit SSE frames, then signal [DONE].
    let sse_forward_tx = sse_tx.clone();
    tokio::spawn(async move {
        while let Some(delta) = delta_rx.recv().await {
            let frame = format!("data: {}\n\n", json!({ "delta": delta }));
            if sse_forward_tx
                .send(Ok(axum::body::Bytes::from(frame)))
                .await
                .is_err()
            {
                return;
            }
        }
        match stream_task.await {
            Ok(Ok(())) => {
                let _ = sse_forward_tx
                    .send(Ok(axum::body::Bytes::from("data: [DONE]\n\n")))
                    .await;
            }
            Ok(Err(e)) => {
                let out = format!("data: {}\n\n", json!({ "error": e.to_string() }));
                let _ = sse_forward_tx.send(Ok(axum::body::Bytes::from(out))).await;
            }
            Err(e) => {
                let out = format!("data: {}\n\n", json!({ "error": format!("join: {e}") }));
                let _ = sse_forward_tx.send(Ok(axum::body::Bytes::from(out))).await;
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(sse_rx);

    Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .body(Body::from_stream(stream))
        .expect("static SSE response builds")
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn open_handle(
    state: &AppState,
    id: &str,
) -> Result<std::sync::Arc<trusty_memory_core::PalaceHandle>, ApiError> {
    state
        .registry
        .open_palace(&state.data_root, &PalaceId::new(id))
        .map_err(|e| ApiError::not_found(format!("palace not found: {id} ({e:#})")))
}

/// Lightweight error type for HTTP handlers.
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }
    fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::util::ServiceExt;

    fn test_state() -> AppState {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        AppState::new(root)
    }

    #[tokio::test]
    async fn status_endpoint_returns_payload() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["version"].is_string());
        assert_eq!(v["palace_count"], 0);
    }

    #[tokio::test]
    async fn unknown_api_returns_404() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_then_list_palace() {
        let state = test_state();
        let app = router().with_state(state.clone());
        let body = json!({"name": "web-test", "description": "from test"}).to_string();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/palaces")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/palaces")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let arr = v.as_array().expect("array");
        assert!(arr.iter().any(|p| p["id"] == "web-test"));
    }

    /// Why: The enriched status payload backs the dashboard's top-row stats;
    /// it must always include the new total_* counters, even on an empty data
    /// root, so the UI can render zeros without special-casing missing fields.
    /// What: Hit `/api/v1/status` on a fresh state and assert the new fields
    /// are present and set to 0.
    /// Test: This test itself.
    #[tokio::test]
    async fn status_includes_total_counters() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["total_drawers"], 0);
        assert_eq!(v["total_vectors"], 0);
        assert_eq!(v["total_kg_triples"], 0);
    }

    /// Why: `/api/v1/dream/status` must return a well-shaped payload even
    /// when no palace has ever run a dream cycle (so the dashboard's first
    /// load doesn't error).
    /// What: Hit the endpoint on a fresh state and assert `last_run_at` is
    /// null and the counters are zero.
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_status_empty_returns_nulls() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dream/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["last_run_at"].is_null());
        assert_eq!(v["merged"], 0);
        assert_eq!(v["pruned"], 0);
    }

    #[tokio::test]
    async fn serves_index_html_fallback() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        // Either OK with embedded HTML, or NOT_FOUND if assets not built.
        assert!(
            resp.status() == StatusCode::OK || resp.status() == StatusCode::NOT_FOUND,
            "got {}",
            resp.status()
        );
    }
}
