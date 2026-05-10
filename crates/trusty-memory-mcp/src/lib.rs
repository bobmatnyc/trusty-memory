//! MCP server (stdio + HTTP/SSE) for trusty-memory.
//!
//! Why: Claude Code and other MCP-aware clients integrate with trusty-memory
//! through the standardized Model Context Protocol; we expose memory + KG
//! tools so they can be called by name.
//! What: Provides `run_stdio` (JSON-RPC 2.0 over stdin/stdout) and `run_http`
//! (axum HTTP/SSE stub), plus an `AppState` that carries the shared
//! `PalaceRegistry`, on-disk data root, and a lazily-initialized embedder.
//! Test: `cargo test -p trusty-memory-mcp` validates handshake + dispatch.

use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::OnceCell;
use tracing::{info, warn};
use trusty_memory_core::embed::FastEmbedder;
use trusty_memory_core::PalaceRegistry;

pub mod tools;

pub use tools::MemoryMcpServer;

/// Shared application state passed to every request handler.
///
/// Why: The stdio loop and HTTP server need the same handles to the registry,
/// data root, and embedder so MCP tools can perform real reads/writes against
/// the live trusty-memory core. The embedder is heavy (loads ONNX weights) so
/// we hold it behind a `OnceCell` and initialize lazily on first use.
/// What: `Clone`-able via `Arc` fields. The registry / data root are eager;
/// `embedder` is `Arc<OnceCell<Arc<FastEmbedder>>>` so concurrent first-use
/// races resolve to a single shared instance.
/// Test: `app_state_default_constructs` confirms construction without panic.
#[derive(Clone)]
pub struct AppState {
    pub version: String,
    pub registry: Arc<PalaceRegistry>,
    pub data_root: PathBuf,
    pub embedder: Arc<OnceCell<Arc<FastEmbedder>>>,
}

impl AppState {
    /// Construct an `AppState` rooted at the given on-disk data directory.
    ///
    /// Why: The CLI (`serve`) and integration tests need to point the MCP
    /// server at different roots — production at `dirs::data_dir`, tests at a
    /// `tempfile::tempdir()`.
    /// What: Builds an empty `PalaceRegistry`, captures the version, and
    /// allocates an empty `OnceCell` for the embedder.
    /// Test: `tools::tests::dispatch_palace_create_persists` constructs an
    /// AppState pointed at a tempdir and round-trips a palace through it.
    pub fn new(data_root: PathBuf) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            registry: Arc::new(PalaceRegistry::new()),
            data_root,
            embedder: Arc::new(OnceCell::new()),
        }
    }

    /// Resolve (or initialize) the shared embedder.
    ///
    /// Why: FastEmbedder load is expensive — we share one instance across all
    /// tool calls; the `OnceCell` ensures concurrent first-use races collapse
    /// to a single load.
    /// What: Returns `Arc<FastEmbedder>` on success. Errors propagate from the
    /// underlying ONNX load.
    /// Test: Indirectly via `dispatch_remember_then_recall`.
    pub async fn embedder(&self) -> Result<Arc<FastEmbedder>> {
        let cell = self.embedder.clone();
        let embedder = cell
            .get_or_try_init(|| async {
                let e = FastEmbedder::new().await?;
                Ok::<Arc<FastEmbedder>, anyhow::Error>(Arc::new(e))
            })
            .await?
            .clone();
        Ok(embedder)
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("version", &self.version)
            .field("data_root", &self.data_root)
            .field("registry_len", &self.registry.len())
            .finish()
    }
}

/// Handle a single MCP JSON-RPC message and produce its response.
///
/// Why: Pulled out of the stdio loop so unit tests can drive every method
/// without touching real stdin/stdout.
/// What: Routes `initialize`, `tools/list`, `tools/call`, `ping`, and the
/// `notifications/initialized` notification (which returns `Value::Null`).
/// Test: See unit tests below — initialize/list/call all return expected
/// JSON-RPC envelopes; notifications return `Null` (no response written).
pub async fn handle_message(state: &AppState, msg: Value) -> Value {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

    match method {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "trusty-memory", "version": state.version}
            }
        }),
        // Notifications must NOT receive a response.
        "notifications/initialized" | "notifications/cancelled" => Value::Null,
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": tools::tool_definitions()
        }),
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or_default();
            let tool_name = params
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let args = params.get("arguments").cloned().unwrap_or_default();
            match tools::dispatch_tool(state, &tool_name, args).await {
                Ok(content) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": content.to_string()}]
                    }
                }),
                Err(e) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32603, "message": e.to_string()}
                }),
            }
        }
        "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("Method not found: {method}")
            }
        }),
    }
}

/// Run the MCP stdio JSON-RPC 2.0 server loop.
///
/// Why: Claude Code launches MCP servers as child processes and speaks
/// JSON-RPC over stdin/stdout — this is the primary integration path.
/// What: Reads one JSON message per line, dispatches via `handle_message`,
/// writes non-null responses to stdout (notifications produce no output).
/// Test: Exercised end-to-end by piping JSON to a spawned `serve` process;
/// `handle_message` covers protocol behaviour in unit tests.
pub async fn run_stdio(state: AppState) -> Result<()> {
    info!("trusty-memory MCP stdio server starting");
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle_message(&state, msg).await,
            Err(e) => {
                warn!("invalid JSON: {e}");
                json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {"code": -32700, "message": "Parse error"}
                })
            }
        };

        // Notifications return Value::Null — skip writing any response.
        if !response.is_null() {
            let mut out = stdout.lock();
            writeln!(out, "{response}")?;
            out.flush()?;
        }
    }
    Ok(())
}

/// Run the optional HTTP/SSE companion server.
///
/// Why: A long-running daemon mode lets non-stdio clients (browsers, curl,
/// future remote agents) hit `/health` and stream events from `/sse`.
/// What: axum router exposing `/health` and a stub `/sse` endpoint.
/// Test: `curl http://127.0.0.1:<port>/health` returns `ok` (manual).
pub async fn run_http(state: AppState, addr: std::net::SocketAddr) -> Result<()> {
    use axum::{routing::get, Router};
    use tower_http::trace::TraceLayer;

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/sse", get(sse_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    info!("HTTP server listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Stub SSE handler returning a single connected event.
///
/// Why: Provides a real `text/event-stream` response so clients can validate
/// the endpoint shape ahead of full streaming support.
/// What: Returns one `data: {"type":"connected"}` SSE frame.
/// Test: `curl -N http://.../sse` shows the connected frame (manual).
async fn sse_handler(
    axum::extract::State(_state): axum::extract::State<AppState>,
) -> impl axum::response::IntoResponse {
    axum::response::Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .body(axum::body::Body::from("data: {\"type\":\"connected\"}\n\n"))
        .expect("static SSE response builds")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> AppState {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        // Leak the tempdir so it lives for the test process; tests are short.
        std::mem::forget(tmp);
        AppState::new(root)
    }

    #[tokio::test]
    async fn initialize_returns_protocol_version_and_capabilities() {
        let state = test_state();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"}
            }
        });
        let resp = handle_message(&state, req).await;
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert_eq!(resp["result"]["serverInfo"]["name"], "trusty-memory");
    }

    #[tokio::test]
    async fn initialized_notification_returns_null() {
        let state = test_state();
        let req = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        let resp = handle_message(&state, req).await;
        assert!(resp.is_null());
    }

    #[tokio::test]
    async fn tools_list_returns_seven_tools() {
        let state = test_state();
        let req = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let resp = handle_message(&state, req).await;
        let tools = resp["result"]["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 7);
    }

    #[tokio::test]
    async fn unknown_method_returns_error() {
        let state = test_state();
        let req = json!({"jsonrpc": "2.0", "id": 4, "method": "wat"});
        let resp = handle_message(&state, req).await;
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn ping_returns_empty_result() {
        let state = test_state();
        let req = json!({"jsonrpc": "2.0", "id": 5, "method": "ping"});
        let resp = handle_message(&state, req).await;
        assert!(resp["result"].is_object());
    }

    #[tokio::test]
    async fn app_state_default_constructs() {
        let s = test_state();
        assert!(!s.version.is_empty());
        assert!(s.registry.is_empty());
    }
}
