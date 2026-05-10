# trusty-memory MCP stdio integration guide

This document describes how to integrate with `trusty-memory serve` over stdio
using the Model Context Protocol (MCP) JSON-RPC 2.0 transport. The audience is
developers building tools — such as open-mpm agents — that spawn trusty-memory
as a child process and call its memory tools programmatically.

---

## Table of contents

1. [Invocation](#1-invocation)
2. [JSON-RPC 2.0 transport details](#2-json-rpc-20-transport-details)
3. [Handshake sequence](#3-handshake-sequence)
4. [tools/list schema](#4-toolslist-schema)
5. [tools/call examples](#5-toolscall-examples)
6. [Error code mapping](#6-error-code-mapping)
7. [Shutdown behavior](#7-shutdown-behavior)

---

## 1. Invocation

Launch the MCP server with:

```sh
trusty-memory serve
```

### Flags

| Flag | Type | Description |
|------|------|-------------|
| `--http <ADDR>` | `SocketAddr` | Also bind an HTTP+SSE companion server at the given address (e.g. `127.0.0.1:3031`). The stdio JSON-RPC loop runs concurrently regardless. |
| `--palace <NAME>` | `string` | Bind the server to a project-scoped default palace. When set, MCP tool calls may omit the `palace` argument. The palace is auto-created on disk if it does not exist. |

### Environment variables

| Variable | Description |
|----------|-------------|
| `RUST_LOG` | Tracing filter (e.g. `info`, `debug`, `trusty_memory=trace`). Defaults to `warn`. All log output goes to **stderr**. |

### Typical integration pattern

An integrator (e.g. open-mpm) spawns one `trusty-memory serve` process per
project, scoped to a project palace:

```sh
trusty-memory serve --palace my-project
```

This ensures all MCP tool calls within the session target `my-project` without
requiring each call to pass a `palace` argument.

---

## 2. JSON-RPC 2.0 transport details

| Property | Value |
|----------|-------|
| Protocol | JSON-RPC 2.0 |
| Framing | Newline-delimited (`\n`). One JSON object per line. |
| Client → server | Write to **stdin** of the process. |
| Server → client | Read from **stdout** of the process. |
| Log output | Written to **stderr** only. Never parse stderr as JSON. |
| Encoding | UTF-8. |
| Batch requests | Not supported. Send one request per line. |

### Message shape

Request:

```json
{"jsonrpc": "2.0", "id": 1, "method": "METHOD", "params": {}}
```

Notification (no response expected, no `id` field):

```json
{"jsonrpc": "2.0", "method": "notifications/initialized"}
```

Successful response:

```json
{"jsonrpc": "2.0", "id": 1, "result": {}}
```

Error response:

```json
{"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": "Method not found: foo"}}
```

**Key behavior**: the server never writes a response for notifications
(`notifications/initialized`, `notifications/cancelled`). If you send one of
these, do not wait for a response line.

---

## 3. Handshake sequence

MCP requires a three-step handshake before tool calls.

### Step 1 — `initialize` request

Send from client, include your client identity:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": {
    "protocolVersion": "2024-11-05",
    "capabilities": {},
    "clientInfo": {"name": "open-mpm", "version": "1.0.0"}
  }
}
```

### Step 2 — `initialize` response

The server responds with its protocol version, capability set, and server
metadata. When started with `--palace`, `serverInfo.default_palace` is
included:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "protocolVersion": "2024-11-05",
    "capabilities": {
      "tools": {}
    },
    "serverInfo": {
      "name": "trusty-memory",
      "version": "0.1.10",
      "default_palace": "my-project"
    }
  }
}
```

When no `--palace` flag was given, `serverInfo.default_palace` is absent.

### Step 3 — `notifications/initialized` notification

After processing the `initialize` response, send this notification. No response
will be written by the server.

```json
{
  "jsonrpc": "2.0",
  "method": "notifications/initialized",
  "params": {}
}
```

The session is now ready for `tools/list` and `tools/call` requests.

---

## 4. `tools/list` schema

Send a `tools/list` request to retrieve the full schema for all available tools:

```json
{"jsonrpc": "2.0", "id": 2, "method": "tools/list"}
```

The server returns 10 tools. The `palace` argument is marked `required` in each
tool's `inputSchema.required` array **unless** the server was started with
`--palace`, in which case `palace` is omitted from `required` (it remains an
accepted optional field).

### Full `tools/list` response (no `--palace` default)

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "tools": [
      {
        "name": "memory_remember",
        "description": "Store a memory (drawer) in a palace room.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "palace": {
              "type": "string",
              "description": "Palace ID (optional if server started with --palace)"
            },
            "text": {
              "type": "string",
              "description": "Memory content"
            },
            "room": {
              "type": "string",
              "description": "Room type (optional)"
            },
            "tags": {
              "type": "array",
              "items": {"type": "string"}
            }
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
        "inputSchema": {
          "type": "object",
          "properties": {}
        }
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
      },
      {
        "name": "memory_list",
        "description": "List drawers in a palace, optionally filtered by room type or tag.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "palace": {"type": "string"},
            "room":   {"type": "string", "description": "Filter by room type (Frontend, Backend, Testing, Planning, Documentation, Research, Configuration, Meetings, General, or custom)"},
            "tag":    {"type": "string", "description": "Filter by tag"},
            "limit":  {"type": "integer", "description": "Max results (default 50)"}
          },
          "required": ["palace"]
        }
      },
      {
        "name": "memory_forget",
        "description": "Delete a drawer from a palace by its UUID.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "palace":    {"type": "string"},
            "drawer_id": {"type": "string", "description": "UUID of the drawer to delete"}
          },
          "required": ["palace", "drawer_id"]
        }
      },
      {
        "name": "palace_info",
        "description": "Get metadata and stats for a single palace.",
        "inputSchema": {
          "type": "object",
          "properties": {
            "palace": {"type": "string"}
          },
          "required": ["palace"]
        }
      }
    ]
  }
}
```

### Tool reference summary

| Tool | Required args (no default palace) | Returns |
|------|-----------------------------------|---------|
| `memory_remember` | `palace`, `text` | `drawer_id`, `palace`, `status` |
| `memory_recall` | `palace`, `query` | `palace`, `query`, `results[]` |
| `memory_recall_deep` | `palace`, `query` | `palace`, `query`, `results[]` |
| `memory_list` | `palace` | `palace`, `drawers[]` |
| `memory_forget` | `palace`, `drawer_id` | `status`, `drawer_id`, `palace` |
| `palace_create` | `name` | `palace_id`, `status` |
| `palace_list` | (none) | `palaces[]` |
| `palace_info` | `palace` | `id`, `name`, `drawer_count`, `data_dir` |
| `kg_assert` | `palace`, `subject`, `predicate`, `object` | `status` |
| `kg_query` | `palace`, `subject` | `subject`, `triples[]` |

### Room types

The `room` argument on `memory_remember` accepts these string values:

- `Frontend`
- `Backend`
- `Testing`
- `Planning`
- `Documentation`
- `Research`
- `Configuration`
- `Meetings`
- `General` (default when omitted or unrecognized)
- Any other string is stored as `Custom(<value>)`

---

## 5. `tools/call` examples

All tool calls follow the same envelope:

```json
{
  "jsonrpc": "2.0",
  "id": <integer>,
  "method": "tools/call",
  "params": {
    "name": "<tool_name>",
    "arguments": { ... }
  }
}
```

Successful responses wrap the tool result as a JSON string inside
`result.content[0].text`:

```json
{
  "jsonrpc": "2.0",
  "id": <integer>,
  "result": {
    "content": [
      {"type": "text", "text": "{...}"}
    ]
  }
}
```

Parse `result.content[0].text` as JSON to access the tool's return value.

### `memory_remember`

Store a memory in a palace room.

Request:

```json
{
  "jsonrpc": "2.0",
  "id": 10,
  "method": "tools/call",
  "params": {
    "name": "memory_remember",
    "arguments": {
      "palace": "my-project",
      "text": "The API gateway validates JWT tokens using RS256.",
      "room": "Backend",
      "tags": ["auth", "jwt", "api-gateway"]
    }
  }
}
```

Response (`result.content[0].text` parsed):

```json
{
  "drawer_id": "a3f1c2d4-...",
  "palace": "my-project",
  "status": "stored"
}
```

### `memory_recall`

Retrieve memories using L0 (identity) + L1 (top-importance drawers) + L2 (HNSW
semantic search) progressive retrieval. Suitable for most queries.

Request:

```json
{
  "jsonrpc": "2.0",
  "id": 11,
  "method": "tools/call",
  "params": {
    "name": "memory_recall",
    "arguments": {
      "palace": "my-project",
      "query": "How does token validation work?",
      "top_k": 5
    }
  }
}
```

Response (`result.content[0].text` parsed):

```json
{
  "palace": "my-project",
  "query": "How does token validation work?",
  "results": [
    {
      "drawer_id": "a3f1c2d4-...",
      "content": "The API gateway validates JWT tokens using RS256.",
      "score": 0.91,
      "layer": 2,
      "tags": ["auth", "jwt", "api-gateway"],
      "importance": 0.5
    }
  ]
}
```

Result fields:

| Field | Type | Description |
|-------|------|-------------|
| `drawer_id` | string (UUID) | Stable identifier for the drawer |
| `content` | string | The stored memory text |
| `score` | float | Similarity score (higher is more relevant) |
| `layer` | integer | Retrieval layer that surfaced this result (0–3) |
| `tags` | string[] | Tags attached at write time |
| `importance` | float | Importance weight in [0.0, 1.0] |

### `memory_recall_deep`

L3 full-index HNSW search. Use when `memory_recall` returns insufficient
results. More exhaustive but slower (target: sub-150 ms).

Request:

```json
{
  "jsonrpc": "2.0",
  "id": 12,
  "method": "tools/call",
  "params": {
    "name": "memory_recall_deep",
    "arguments": {
      "palace": "my-project",
      "query": "deployment configuration details",
      "top_k": 20
    }
  }
}
```

The response shape is identical to `memory_recall`.

### `palace_create`

Create a new named palace. Idempotent if the palace already exists (returns an
error on conflict — call `palace_list` first if in doubt).

Request:

```json
{
  "jsonrpc": "2.0",
  "id": 20,
  "method": "tools/call",
  "params": {
    "name": "palace_create",
    "arguments": {
      "name": "client-acme",
      "description": "Memories for the Acme client engagement"
    }
  }
}
```

Response (`result.content[0].text` parsed):

```json
{
  "palace_id": "client-acme",
  "status": "created"
}
```

### `palace_list`

List all palaces registered on this machine.

Request:

```json
{
  "jsonrpc": "2.0",
  "id": 21,
  "method": "tools/call",
  "params": {
    "name": "palace_list",
    "arguments": {}
  }
}
```

Response (`result.content[0].text` parsed):

```json
{
  "palaces": ["my-project", "client-acme", "agent-pm"]
}
```

### `kg_assert`

Assert a subject-predicate-object triple into the temporal knowledge graph.
If a prior active triple with the same `(subject, predicate)` exists, it is
closed (its `valid_to` is set to now) before the new triple is inserted.

Request:

```json
{
  "jsonrpc": "2.0",
  "id": 30,
  "method": "tools/call",
  "params": {
    "name": "kg_assert",
    "arguments": {
      "palace": "my-project",
      "subject": "auth-service",
      "predicate": "deployed_version",
      "object": "2.4.1",
      "confidence": 0.95,
      "provenance": "deploy-log-2026-05-10"
    }
  }
}
```

Response (`result.content[0].text` parsed):

```json
{
  "status": "asserted"
}
```

### `kg_query`

Query all currently active triples for a subject (i.e., `valid_to IS NULL`).

Request:

```json
{
  "jsonrpc": "2.0",
  "id": 31,
  "method": "tools/call",
  "params": {
    "name": "kg_query",
    "arguments": {
      "palace": "my-project",
      "subject": "auth-service"
    }
  }
}
```

Response (`result.content[0].text` parsed):

```json
{
  "subject": "auth-service",
  "triples": [
    {
      "subject": "auth-service",
      "predicate": "deployed_version",
      "object": "2.4.1",
      "valid_from": "2026-05-10T14:32:00Z",
      "valid_to": null,
      "confidence": 0.95,
      "provenance": "deploy-log-2026-05-10"
    }
  ]
}
```

Triple fields:

| Field | Type | Description |
|-------|------|-------------|
| `subject` | string | Entity identifier |
| `predicate` | string | Relationship name |
| `object` | string | Value or target entity |
| `valid_from` | RFC 3339 string | When this fact became active |
| `valid_to` | RFC 3339 string or `null` | When this fact was superseded; `null` means currently active |
| `confidence` | float in [0.0, 1.0] | Caller-supplied confidence score |
| `provenance` | string or `null` | Free-form source identifier |

### `memory_list`

List drawers in a palace, optionally filtered by room type or tag. Results are
sorted by importance descending and capped at `limit` (default 50).

Request:

```json
{
  "jsonrpc": "2.0",
  "id": 40,
  "method": "tools/call",
  "params": {
    "name": "memory_list",
    "arguments": {
      "palace": "my-project",
      "room": "Backend",
      "tag": "auth",
      "limit": 20
    }
  }
}
```

Response (`result.content[0].text` parsed):

```json
{
  "palace": "my-project",
  "drawers": [
    {
      "drawer_id": "a3f1c2d4-...",
      "content": "The API gateway validates JWT tokens using RS256.",
      "importance": 0.5,
      "tags": ["auth", "jwt", "api-gateway"],
      "created_at": "2026-05-10T14:32:00Z"
    }
  ]
}
```

### `memory_forget`

Delete a drawer from a palace by its UUID. Removes the vector from the HNSW
index, drops the row from the in-memory drawer table, and persists the L1
snapshot.

Request:

```json
{
  "jsonrpc": "2.0",
  "id": 41,
  "method": "tools/call",
  "params": {
    "name": "memory_forget",
    "arguments": {
      "palace": "my-project",
      "drawer_id": "a3f1c2d4-1234-5678-9abc-def012345678"
    }
  }
}
```

Response (`result.content[0].text` parsed):

```json
{
  "status": "deleted",
  "drawer_id": "a3f1c2d4-1234-5678-9abc-def012345678",
  "palace": "my-project"
}
```

### `palace_info`

Get metadata and stats for a single palace.

Request:

```json
{
  "jsonrpc": "2.0",
  "id": 42,
  "method": "tools/call",
  "params": {
    "name": "palace_info",
    "arguments": {
      "palace": "my-project"
    }
  }
}
```

Response (`result.content[0].text` parsed):

```json
{
  "id": "my-project",
  "name": "my-project",
  "drawer_count": 142,
  "data_dir": "/Users/alice/Library/Application Support/trusty-memory/my-project"
}
```

---

## 6. Error code mapping

All errors follow the JSON-RPC 2.0 error object shape:

```json
{
  "jsonrpc": "2.0",
  "id": <id>,
  "error": {
    "code": <integer>,
    "message": "<human-readable string>"
  }
}
```

| Code | Standard name | trusty-memory failure modes |
|------|---------------|----------------------------|
| `-32700` | Parse error | Malformed JSON on stdin (not valid UTF-8 or not valid JSON). |
| `-32601` | Method not found | Unknown `method` value — only `initialize`, `notifications/initialized`, `notifications/cancelled`, `tools/list`, `tools/call`, and `ping` are recognized. |
| `-32603` | Internal error | Dispatched to a known tool but the tool itself failed. Covers: unknown tool name (`"unknown tool: <name>"`), missing required argument (`"missing 'palace'"`, `"missing 'text'"`, etc.), palace open failure, embedder initialization failure, SQLite write failure, vector index failure. |

The message string is always a human-readable description including the
underlying cause. Parse it for diagnostic display; do not pattern-match it for
control flow.

### `palace` omitted without a default

When `palace` is omitted from a tool call and the server was started without
`--palace`, the server returns `-32603` with a message of the form:

```
memory_recall: missing 'palace' (no --palace default configured)
```

---

## 7. Shutdown behavior

### Graceful shutdown via SIGTERM / SIGINT

When the process receives `SIGTERM` or `SIGINT` (Ctrl-C), the server:

1. Stops reading new messages from stdin.
2. Signals all background dreamer tasks (memory consolidation loops) to exit.
3. Waits up to 2 seconds for each dreamer task to finish cleanly.
4. Exits with code `0`.

In-flight tool calls that have already started dispatching complete normally
before shutdown. There is no partial-write risk to SQLite because the KG store
uses WAL mode with a connection pool.

### EOF on stdin

When stdin is closed (EOF), the stdio read loop returns `Ok(())` and the
process exits with code `0`. This is the expected behavior when the parent
process closes the pipe rather than sending SIGTERM.

### HTTP companion server

If `--http` was given, both the stdio loop and the HTTP server run concurrently
in the same tokio runtime. Either loop terminating (stdin EOF, SIGINT, or HTTP
server error) causes the other to be cancelled. The process exits after both
complete.

### Dreamer tasks

Background consolidation tasks (memory deduplication, closet index refresh, and
importance decay) are spawned on startup for each existing palace. They run
independently of tool calls. On shutdown they receive a cancellation signal via
a `tokio::sync::watch` channel and exit within the 2-second grace window.
