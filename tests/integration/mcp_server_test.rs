//! Why: Lock the MCP tool surface so accidental schema drift breaks CI before
//! a Claude Code client sees it.
//! What: Verifies all 11 tools are advertised and the JSON-RPC handshake +
//! tools/call round-trip return the expected envelope shapes.
//! Test: `cargo test --test mcp_server_test`.

use serde_json::json;
use trusty_memory_mcp::{handle_message, tools::tool_definitions, AppState};

#[test]
fn all_tools_are_advertised() {
    let defs = tool_definitions();
    let tools = defs
        .get("tools")
        .and_then(|t| t.as_array())
        .expect("tools array");
    assert_eq!(tools.len(), 12, "expected exactly 12 MCP tools");

    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .collect();

    for expected in [
        "memory_remember",
        "memory_recall",
        "memory_recall_deep",
        "memory_list",
        "memory_forget",
        "palace_create",
        "palace_list",
        "palace_info",
        "kg_assert",
        "kg_query",
        "palace_compact",
        "memory_recall_all",
    ] {
        assert!(
            names.contains(&expected),
            "missing tool {expected} in {names:?}"
        );
    }
}

#[tokio::test]
async fn initialize_then_list_then_call_handshake() {
    // Why: With the MCP tools wired to the real registry, every memory_*
    // / kg_* call needs an on-disk palace. We point the AppState at a
    // tempdir so this integration test does not collide with the
    // developer's machine-wide palaces directory.
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf());

    // 1) initialize
    let init = handle_message(
        &state,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                       "clientInfo": {"name": "it", "version": "0"}}
        }),
    )
    .await;
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");

    // 2) initialized notification — no response
    let notif = handle_message(
        &state,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    assert!(notif.is_null());

    // 3) tools/list
    let list = handle_message(
        &state,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 12);

    // 4) palace_create — the real registry needs the palace on disk before
    //    memory_remember can target it.
    let created = handle_message(
        &state,
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {
                "name": "palace_create",
                "arguments": {"name": "it-palace"}
            }
        }),
    )
    .await;
    let created_text = created["result"]["content"][0]["text"]
        .as_str()
        .expect("palace_create text");
    assert!(created_text.contains("created"));

    // 5) memory_remember
    let call = handle_message(
        &state,
        json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {
                "name": "memory_remember",
                "arguments": {"palace": "it-palace", "text": "first memory snippet"}
            }
        }),
    )
    .await;
    let content = call["result"]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert!(content[0]["text"].as_str().unwrap().contains("drawer_id"));

    // 6) memory_recall — verifies the real retrieval path round-trips the
    //    text we just stored.
    let recalled = handle_message(
        &state,
        json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": {
                "name": "memory_recall",
                "arguments": {"palace": "it-palace", "query": "first memory snippet", "top_k": 5}
            }
        }),
    )
    .await;
    let recalled_text = recalled["result"]["content"][0]["text"]
        .as_str()
        .expect("recall text");
    assert!(
        recalled_text.contains("first memory snippet"),
        "expected recall to contain `first memory snippet`, got: {recalled_text}"
    );
}
