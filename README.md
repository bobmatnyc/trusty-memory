# trusty-memory

**Machine-wide AI memory daemon** built in Rust, using the Memory Palace architecture.

> One install per machine. Multiple named palaces. Sub-5 ms baseline retrieval.

[![crates.io](https://img.shields.io/crates/v/trusty-memory)](https://crates.io/crates/trusty-memory)
[![License: ELv2](https://img.shields.io/badge/license-ELv2-blue)](./LICENSE)

---

## Installation

### From crates.io (recommended)

```sh
cargo install trusty-memory
```

### Via cargo-binstall (fast, no compilation)

```sh
cargo binstall trusty-memory
```

### Build from source

```sh
git clone https://github.com/bobmatnyc/trusty-memory
cd trusty-memory
cargo build --release
cargo install --path .
```

---

## Quick start

```sh
# 1. Install
cargo install trusty-memory

# 2. Create a palace for your project
trusty-memory palace new my-project

# 3. Store a memory
trusty-memory remember my-project "The API gateway validates JWT tokens using RS256."

# 4. Recall memories
trusty-memory recall my-project "how does token validation work?"

# 5. Start the MCP server (for Claude Code integration)
trusty-memory serve --palace my-project
```

---

## Claude Code integration

This is the primary use case. Add trusty-memory as an MCP server so Claude Code
can store and retrieve memories in your project palace automatically.

### Project-local config (`.mcp.json`)

Place this file in your project root:

```json
{
  "mcpServers": {
    "trusty-memory": {
      "command": "trusty-memory",
      "args": ["serve", "--palace", "my-project"]
    }
  }
}
```

### Global config (`~/.claude/mcp.json`)

```json
{
  "mcpServers": {
    "trusty-memory": {
      "command": "trusty-memory",
      "args": ["serve", "--palace", "my-project"]
    }
  }
}
```

The `--palace` flag sets a default palace for all tool calls in the session.
The palace is created automatically on first use if it does not exist.

Once configured, Claude Code has access to 10 memory tools — see
[`docs/mcp-stdio.md`](./docs/mcp-stdio.md) for the full tool reference.

---

## Chat

trusty-memory includes a conversational interface that retrieves palace context
and sends it to a local model.

```sh
trusty-memory chat my-project
```

### Local model configuration

Edit `~/.config/trusty-memory/config.toml` or use `trusty-memory config set`:

```toml
[local_model]
enabled   = true
base_url  = "http://localhost:11434"   # Ollama default
model     = "qwen3:30b"               # default model
```

Ollama and LM Studio are both supported. Any OpenAI-compatible API endpoint works.

---

## Architecture

### 4-layer progressive retrieval

| Layer | Source | Token budget | When |
|------:|:-------|-------------:|:-----|
| **L0** | `identity.txt` | ~100 | always loaded |
| **L1** | top-15 drawers by importance | ~800 | always loaded |
| **L2** | metadata-filtered HNSW vector search | variable | topic match in query |
| **L3** | full HNSW search across the palace | variable | explicit deep query |

L0 and L1 are pre-cached in memory — reads never touch disk. L2 and L3 take a
read lock on the vector index; many concurrent searches never block each other.

### Dual store

- **Vector index** — usearch HNSW (all-MiniLM-L6-v2, 384-d, local ONNX). Handles
  semantic similarity search.
- **Temporal knowledge graph** — SQLite WAL. Stores subject-predicate-object triples
  with `valid_from` / `valid_to` intervals. Asserting a new fact automatically closes
  the prior active interval.

### Palace hierarchy

```
Palace  (one per project or domain)
  └── Wing    (top-level domain: project area or agent persona)
        └── Room    (topic: Frontend / Backend / Testing / Planning / ...)
              └── Closet  (pre-computed pointer index: topic|entities → drawer_ids)
                    └── Drawer  (atomic memory unit: text + importance + tags)
```

### Background tasks

On startup, dreamer tasks run for each palace: memory consolidation, importance
decay, and deduplication. They shut down cleanly within 2 seconds of SIGTERM.

---

## CLI reference

| Command | Description |
|---------|-------------|
| `trusty-memory serve [--http <addr>] [--palace <name>]` | Start MCP stdio server; optionally bind HTTP/SSE companion |
| `trusty-memory palace new <name>` | Create a new palace |
| `trusty-memory palace list` | List all palaces on this machine |
| `trusty-memory remember <palace> <text> [--room <name>]` | Store a memory |
| `trusty-memory recall <palace> <query> [--top-k N]` | Recall memories (L0+L1+L2) |
| `trusty-memory status` | Daemon health and palace summary |
| `trusty-memory chat <palace>` | Start a chat session with palace context |
| `trusty-memory config set <key> <value>` | Set a config value |

---

## MCP tools

The MCP server exposes 10 tools:

| Tool | Required args | Returns |
|------|---------------|---------|
| `memory_remember` | `palace`, `text` | `drawer_id` |
| `memory_recall` | `palace`, `query` | results (L0+L1+L2) |
| `memory_recall_deep` | `palace`, `query` | results (L3 full search) |
| `memory_list` | `palace` | drawer list, filterable by room/tag |
| `memory_forget` | `palace`, `drawer_id` | deletion confirmation |
| `palace_create` | `name` | `palace_id` |
| `palace_list` | — | list of palace IDs |
| `palace_info` | `palace` | metadata and drawer count |
| `kg_assert` | `palace`, `subject`, `predicate`, `object` | confirmation |
| `kg_query` | `palace`, `subject` | active triples |

Full JSON-RPC protocol, request/response examples, and error codes are in
[`docs/mcp-stdio.md`](./docs/mcp-stdio.md).

---

## Performance targets

| Operation | Target |
|-----------|--------|
| L0 + L1 retrieval | sub-5 ms (in-memory) |
| L2 HNSW search (top-10) | sub-50 ms |
| L3 deep search (top-50) | sub-150 ms |
| Palace cold start | under 200 ms |

---

## License

[Elastic License 2.0](./LICENSE) (ELv2).
