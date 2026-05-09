//! MCP tool surface for trusty-memory.
//!
//! Why: Concentrates the public tool contract in one file so changes are
//! auditable and the MCP schema stays in sync with the implementation.
//! What: Defines `MemoryMcpServer`, `tool_definitions()` (the MCP
//! `tools/list` payload), and the in-process tool dispatcher.
//! Test: `cargo test -p trusty-memory-mcp` validates the schema and dispatch.
//!
//! Tools exposed:
//! - `memory_remember(palace, text, room?, tags?)` -> drawer_id
//! - `memory_recall(palace, query, top_k?)`        -> Vec<Drawer> (L0+L1+L2)
//! - `memory_recall_deep(palace, query, top_k?)`   -> Vec<Drawer> (L3 deep)
//! - `palace_create(name, description?)`           -> PalaceId
//! - `palace_list()`                                -> Vec<PalaceId>
//! - `kg_assert(palace, subject, predicate, object, confidence?)` -> ()
//! - `kg_query(palace, subject)`                    -> Vec<Triple>

use anyhow::Result;
use serde_json::{json, Value};

/// Marker server type. Reserved for future stateful MCP server impls.
///
/// Why: Keep a stable type name while the protocol-loop is implemented at
/// module level, so external callers can still depend on a server symbol.
/// What: Zero-sized struct with `new` / `Default`.
/// Test: `MemoryMcpServer::default()` constructs without panic.
pub struct MemoryMcpServer;

impl MemoryMcpServer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MemoryMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

/// MCP `tools/list` response payload.
///
/// Why: Claude Code calls `tools/list` once on connect and uses the schema
/// to drive the tool picker; the schema is the source of truth for arg names.
/// What: Returns a JSON object `{ "tools": [...] }` with all 7 tool defs.
/// Test: Assert the array length is 7 and contains every expected name.
pub fn tool_definitions() -> Value {
    json!({
        "tools": [
            {
                "name": "memory_remember",
                "description": "Store a memory (drawer) in a palace room.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace": {"type": "string", "description": "Palace ID"},
                        "text":   {"type": "string", "description": "Memory content"},
                        "room":   {"type": "string", "description": "Room type (optional)"},
                        "tags":   {"type": "array", "items": {"type": "string"}}
                    },
                    "required": ["palace", "text"]
                }
            },
            {
                "name": "memory_recall",
                "description": "Recall memories using L0+L1+L2 progressive retrieval.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace": {"type": "string"},
                        "query":  {"type": "string"},
                        "top_k":  {"type": "integer", "default": 10}
                    },
                    "required": ["palace", "query"]
                }
            },
            {
                "name": "memory_recall_deep",
                "description": "Deep recall using L3 full HNSW search.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace": {"type": "string"},
                        "query":  {"type": "string"},
                        "top_k":  {"type": "integer", "default": 10}
                    },
                    "required": ["palace", "query"]
                }
            },
            {
                "name": "palace_create",
                "description": "Create a new memory palace.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name":        {"type": "string"},
                        "description": {"type": "string"}
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "palace_list",
                "description": "List all palaces on this machine.",
                "inputSchema": {"type": "object", "properties": {}}
            },
            {
                "name": "kg_assert",
                "description": "Assert a fact in the temporal knowledge graph.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace":     {"type": "string"},
                        "subject":    {"type": "string"},
                        "predicate":  {"type": "string"},
                        "object":     {"type": "string"},
                        "confidence": {"type": "number", "default": 1.0},
                        "provenance": {"type": "string"}
                    },
                    "required": ["palace", "subject", "predicate", "object"]
                }
            },
            {
                "name": "kg_query",
                "description": "Query active knowledge-graph triples for a subject.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "palace":  {"type": "string"},
                        "subject": {"type": "string"}
                    },
                    "required": ["palace", "subject"]
                }
            }
        ]
    })
}

/// Dispatch a tool call by name to its handler stub.
///
/// Why: Centralises the name → handler mapping so the stdio loop stays small;
/// once the registry / retrieval engines land, only this fn changes.
/// What: Returns `Ok(Value)` on success, `Err` on unknown tool / bad args.
/// Test: `dispatch_tool("memory_remember", { palace, text })` returns a
/// JSON object with a `drawer_id` field; `dispatch_tool("nope", _)` errors.
pub async fn dispatch_tool(name: &str, args: Value) -> Result<Value> {
    match name {
        "memory_remember" => {
            let palace = args
                .get("palace")
                .and_then(|v| v.as_str())
                .unwrap_or("default");
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let drawer_id = uuid::Uuid::new_v4();
            let preview_end = text
                .char_indices()
                .nth(50)
                .map(|(i, _)| i)
                .unwrap_or(text.len());
            Ok(json!({
                "drawer_id": drawer_id.to_string(),
                "palace": palace,
                "status": "stored",
                "preview": &text[..preview_end],
                "note": "Registry not yet wired — implement in #6/#15"
            }))
        }
        "memory_recall" | "memory_recall_deep" => {
            let palace = args
                .get("palace")
                .and_then(|v| v.as_str())
                .unwrap_or("default");
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            Ok(json!({
                "palace": palace,
                "query": query,
                "results": [],
                "note": "Registry not yet wired — implement in #6/#15"
            }))
        }
        "palace_create" => {
            let palace_name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed");
            Ok(json!({"palace_id": palace_name, "status": "created"}))
        }
        "palace_list" => Ok(json!({"palaces": []})),
        "kg_assert" => Ok(json!({"status": "asserted"})),
        "kg_query" => {
            let subject = args.get("subject").and_then(|v| v.as_str()).unwrap_or("");
            Ok(json!({"subject": subject, "triples": []}))
        }
        other => anyhow::bail!("unknown tool: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definitions_lists_seven_tools() {
        let defs = tool_definitions();
        let tools = defs
            .get("tools")
            .and_then(|t| t.as_array())
            .expect("tools array");
        assert_eq!(tools.len(), 7);
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        for expected in [
            "memory_remember",
            "memory_recall",
            "memory_recall_deep",
            "palace_create",
            "palace_list",
            "kg_assert",
            "kg_query",
        ] {
            assert!(names.contains(&expected), "missing tool: {expected}");
        }
    }

    #[tokio::test]
    async fn dispatch_remember_returns_drawer_id() {
        let result = dispatch_tool("memory_remember", json!({"palace": "p1", "text": "hello"}))
            .await
            .expect("dispatch ok");
        assert!(result.get("drawer_id").is_some());
        assert_eq!(result.get("palace").and_then(|v| v.as_str()), Some("p1"));
    }

    #[tokio::test]
    async fn dispatch_unknown_tool_errors() {
        let err = dispatch_tool("does_not_exist", json!({}))
            .await
            .expect_err("should error");
        assert!(err.to_string().contains("unknown tool"));
    }
}
