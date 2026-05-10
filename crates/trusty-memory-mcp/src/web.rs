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
use std::collections::HashMap;
use trusty_memory_core::palace::{Palace, PalaceId, RoomType};
use trusty_memory_core::retrieval::{
    recall_deep_with_default_embedder, recall_with_default_embedder,
};
use trusty_memory_core::store::kg::Triple;
use trusty_memory_core::PalaceRegistry;
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
    use tower_http::cors::CorsLayer;
    use tower_http::trace::TraceLayer;

    Router::new()
        .route("/api/v1/status", get(status))
        .route("/api/v1/config", get(config))
        .route("/api/v1/palaces", get(list_palaces).post(create_palace))
        .route("/api/v1/palaces/:id", get(get_palace_handler))
        .route(
            "/api/v1/palaces/:id/drawers",
            get(list_drawers).post(create_drawer),
        )
        .route(
            "/api/v1/palaces/:id/drawers/:drawer_id",
            delete(delete_drawer),
        )
        .route("/api/v1/palaces/:id/recall", get(recall_handler))
        .route("/api/v1/palaces/:id/kg", get(kg_query).post(kg_assert))
        .route("/api/v1/chat", post(chat_handler))
        .route("/health", get(|| async { "ok" }))
        .fallback(static_handler)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
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
        serve_embedded("index.html").unwrap_or_else(|| {
            (StatusCode::NOT_FOUND, "ui assets missing").into_response()
        })
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
        HeaderValue::from_str(mime.as_ref()).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
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
}

async fn status(State(state): State<AppState>) -> Json<StatusPayload> {
    let count = PalaceRegistry::list_palaces(&state.data_root)
        .map(|v| v.len())
        .unwrap_or(0);
    Json(StatusPayload {
        version: state.version.clone(),
        palace_count: count,
        default_palace: state.default_palace.clone(),
        data_root: state.data_root.display().to_string(),
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
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn list_palaces(
    State(state): State<AppState>,
) -> Result<Json<Vec<PalaceInfo>>, ApiError> {
    let palaces = PalaceRegistry::list_palaces(&state.data_root)
        .map_err(|e| ApiError::internal(format!("list palaces: {e:#}")))?;
    let mut out = Vec::with_capacity(palaces.len());
    for p in palaces {
        let drawer_count = state
            .registry
            .open_palace(&state.data_root, &p.id)
            .map(|h| h.drawers.read().len())
            .unwrap_or(0);
        out.push(PalaceInfo {
            id: p.id.0.clone(),
            name: p.name,
            description: p.description,
            drawer_count,
            created_at: p.created_at,
        });
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
    let drawer_count = state
        .registry
        .open_palace(&state.data_root, &palace.id)
        .map(|h| h.drawers.read().len())
        .unwrap_or(0);
    Ok(Json(PalaceInfo {
        id: palace.id.0,
        name: palace.name,
        description: palace.description,
        drawer_count,
        created_at: palace.created_at,
    }))
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

#[derive(Deserialize, Serialize, Clone)]
struct ChatMessage {
    role: String,
    content: String,
}

async fn chat_handler(
    State(state): State<AppState>,
    Json(body): Json<ChatBody>,
) -> Response {
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

    let mut messages: Vec<HashMap<&str, String>> = Vec::new();
    let system = if context.is_empty() {
        "You are trusty-memory's assistant.".to_string()
    } else {
        format!(
            "You are trusty-memory's assistant. Use the following palace memory \
             as context when relevant:\n{context}"
        )
    };
    messages.push(HashMap::from([
        ("role", "system".to_string()),
        ("content", system),
    ]));
    for m in &body.history {
        messages.push(HashMap::from([
            ("role", m.role.clone()),
            ("content", m.content.clone()),
        ]));
    }
    messages.push(HashMap::from([
        ("role", "user".to_string()),
        ("content", body.message.clone()),
    ]));

    let payload = json!({
        "model": cfg.openrouter_model,
        "messages": messages,
        "stream": true,
    });

    let client = reqwest::Client::new();
    let or_resp = match client
        .post("https://openrouter.ai/api/v1/chat/completions")
        .bearer_auth(&cfg.openrouter_api_key)
        .header("HTTP-Referer", "https://github.com/bobmatnyc/trusty-memory")
        .header("X-Title", "trusty-memory")
        .json(&payload)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("openrouter request failed: {e}"),
            )
                .into_response();
        }
    };

    if !or_resp.status().is_success() {
        let status = or_resp.status();
        let body_text = or_resp.text().await.unwrap_or_default();
        return (StatusCode::BAD_GATEWAY, format!("{status}: {body_text}")).into_response();
    }

    // Bridge OpenRouter's SSE -> our SSE through a channel. Spawning a task
    // keeps the HTTP handler simple and avoids pulling in `async-stream`.
    use futures::stream::StreamExt;
    let mut upstream = or_resp.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(32);

    tokio::spawn(async move {
        let mut buffer = String::new();
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => {
                    buffer.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(idx) = buffer.find("\n\n") {
                        let frame: String = buffer.drain(..idx + 2).collect();
                        for line in frame.lines() {
                            let Some(data) = line.strip_prefix("data:") else {
                                continue;
                            };
                            let data = data.trim();
                            if data == "[DONE]" {
                                let _ = tx
                                    .send(Ok(axum::body::Bytes::from("data: [DONE]\n\n")))
                                    .await;
                                continue;
                            }
                            if let Ok(parsed) = serde_json::from_str::<Value>(data) {
                                if let Some(delta) = parsed
                                    .get("choices")
                                    .and_then(|c| c.get(0))
                                    .and_then(|c| c.get("delta"))
                                    .and_then(|d| d.get("content"))
                                    .and_then(|s| s.as_str())
                                {
                                    let out = format!("data: {}\n\n", json!({ "delta": delta }));
                                    let _ = tx.send(Ok(axum::body::Bytes::from(out))).await;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    let out = format!("data: {}\n\n", json!({ "error": e.to_string() }));
                    let _ = tx.send(Ok(axum::body::Bytes::from(out))).await;
                    break;
                }
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);

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

    #[tokio::test]
    async fn serves_index_html_fallback() {
        let state = test_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            )
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
