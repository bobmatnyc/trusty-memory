# trusty-memory

**Machine-wide AI memory daemon** built in Rust, using the Memory Palace architecture.

> One install per machine. Multiple named palaces. Sub-5 ms baseline retrieval.

[![crates.io](https://img.shields.io/crates/v/trusty-memory)](https://crates.io/crates/trusty-memory)
[![License: ELv2](https://img.shields.io/badge/license-ELv2-blue)](./LICENSE)

---

## Getting Started

trusty-memory is a persistent background daemon that exposes memory tools to
Claude Code over MCP. There are two ways to run it.

### Path A: Standalone daemon (recommended)

Install once, run as a launchd service, and point Claude Code at it.

```sh
# 1. Install
cargo install trusty-memory

# 2. Install and start the launchd service (macOS)
trusty-memory service install

# 3. Add to your project's .mcp.json
```

`.mcp.json` in the project root:

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

Palaces are created automatically on first use. No `palace new` step required.

For machine-wide config, use `~/.claude/mcp.json` with the same contents.

### Path B: Direct serve (no service install)

If you don't want a launchd service, just point Claude Code at `serve` directly.
The daemon will auto-start on first MCP call and remain running until shut down.

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

That's it. Claude Code now has 10 memory tools available.

### Verifying

```sh
trusty-memory status
trusty-memory palace list
```

---

## The Daemon Model

trusty-memory runs as a single long-lived process per machine. All projects
share the same daemon; isolation is provided by **palaces** (named namespaces),
not by separate processes.

- **Auto-start.** Every CLI command except `serve`, `service`, `setup`, and
  `hooks` calls `ensure_daemon()` first. If no daemon is alive, one is spawned
  detached and the command waits up to 5 seconds for it to come up.
- **Service management (macOS).** `trusty-memory service install` writes
  `~/Library/LaunchAgents/com.trusty.trusty-memory.plist` and bootstraps it
  into the user's launchd domain. `RunAtLoad: true`, `KeepAlive: true`.
  On non-macOS platforms `service` returns an error pointing to systemd.
- **HTTP port discovery.** The daemon auto-binds starting at `127.0.0.1:3031`
  and walks up to 20 ports if that's taken. The chosen address is written to
  `~/Library/Application Support/trusty-memory/http_addr`, which
  `service status` reads.
- **Data directory.** macOS: `~/Library/Application Support/trusty-memory/palaces/`.
  Other platforms fall back to `~/.trusty-memory/palaces/`.
- **Logs.** `~/.trusty-memory/logs/daemon.log`. `trusty-memory service logs`
  tails the last 50 lines.
- **Stdio-only mode.** Pass `--no-http` to `serve` if you don't want a TCP
  listener. The daemon still writes its PID file but skips `http_addr`.
- **Shutdown.** `trusty-memory stop` reads the PID file and sends SIGTERM.
  Background dreamer tasks get a 2 second grace window before exit.

Full reference: [`docs/daemon.md`](./docs/daemon.md).

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
- **Temporal knowledge graph** — SQLite WAL. Stores subject-predicate-object
  triples with `valid_from` / `valid_to` intervals. Asserting a new fact
  automatically closes the prior active interval.

### Palace hierarchy

```
Palace  (one per project or domain)
  └── Wing    (top-level domain: project area or agent persona)
        └── Room    (topic: Frontend / Backend / Testing / Planning / ...)
              └── Closet  (pre-computed pointer index: topic|entities -> drawer_ids)
                    └── Drawer  (atomic memory unit: text + importance + tags)
```

### Background tasks

On startup, dreamer tasks run for each palace: memory consolidation, importance
decay, and deduplication. They shut down cleanly within 2 seconds of SIGTERM.

---

## CLI Reference

The commands you'll use day-to-day are **bold**.

| Command | Description |
|---------|-------------|
| **`trusty-memory serve [--http <addr>] [--palace <name>] [--no-http]`** | Start MCP stdio server (and optional HTTP/SSE companion) |
| `trusty-memory start` | Start daemon in background (detached) |
| `trusty-memory stop` | Stop running daemon gracefully |
| **`trusty-memory status`** | Daemon health and palace summary |
| `trusty-memory doctor` | Diagnose environment issues |
| **`trusty-memory service install`** | Install and start launchd service (macOS) |
| `trusty-memory service uninstall` | Unload and remove launchd service (macOS) |
| `trusty-memory service status` | Show launchd service status and HTTP address (macOS) |
| `trusty-memory service logs` | Tail daemon logs (macOS) |
| `trusty-memory palace new <name>` | Create a new palace |
| **`trusty-memory palace list`** | List all palaces |
| `trusty-memory palace info <name>` | Metadata and drawer count for a palace |
| `trusty-memory palace compact <name>` | Compact palace storage |
| **`trusty-memory remember <palace> <text> [--room <name>]`** | Store a memory |
| **`trusty-memory recall <palace> <query> [--top-k N] [--deep] [--room <name>]`** | Recall memories |
| `trusty-memory list <palace> [--room <name>] [--tag <tag>]` | List drawers |
| `trusty-memory forget <palace> <drawer_id>` | Delete a drawer |
| `trusty-memory chat <palace>` | Conversational interface with palace context |
| `trusty-memory config set <key> <value>` | Set config value |
| `trusty-memory setup` | Interactive first-run setup |
| `trusty-memory hooks fire <event>` | Fire a Claude Code hook event |
| `trusty-memory backup <palace>` | Backup a palace |
| `trusty-memory restore <palace>` | Restore a palace |
| `trusty-memory completions <shell>` | Generate shell completions |

---

## MCP Tools

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

When the server is launched with `--palace <name>`, the `palace` argument can
be omitted from every tool call. Full JSON-RPC protocol, request/response
examples, and error codes are in [`docs/mcp-stdio.md`](./docs/mcp-stdio.md).

---

## vs. kuzu-memory

[kuzu-memory](https://github.com/zachhandley/kuzu-memory) is a Python-based
memory system built on the Kuzu graph database. Both projects solve overlapping
problems with different trade-offs.

| Feature | trusty-memory | kuzu-memory |
|---------|---------------|-------------|
| Language | Rust | Python |
| Install | `cargo install trusty-memory` | `pip install kuzu-memory` |
| Daemon model | Persistent launchd service | Per-session spawn (no daemon) |
| Cold start | <200 ms | 300–500 ms (Python + sentence-transformers load) |
| Warm retrieval | sub-5 ms (L0+L1 in-memory) | ~3 ms (warm, in-process) |
| Namespaces | Multiple palaces (arbitrary) | One per working directory + user DB |
| Retrieval model | 4-layer progressive (L0/L1/L2/L3) | Multi-strategy (HNSW + TF-IDF + entity graph, optional LLM rerank) |
| Always-loaded baseline | Yes (L0 identity + L1 top-15, ~900 tokens) | No (all retrieval on-demand) |
| Vector store | usearch HNSW (local ONNX) | Kuzu built-in HNSW |
| Knowledge graph | Temporal triples (valid_from/valid_to, interval closure) | TTL-based expiry (30d episodic, 1d working, etc.) |
| Memory hierarchy | Palace → Wing → Room → Closet → Drawer | Flat graph model |
| Memory footprint | ~50 MB daemon + model | ~25 MB process + 80–100 MB sentence-transformers |
| LLM reranking | No (local model only) | Optional (Haiku via API) |

trusty-memory includes a (currently stub) read-only bridge to kuzu-memory
databases for projects that already have one.

### When to use each

- **Use trusty-memory** when you want a persistent, machine-wide daemon, multiple
  project namespaces under a single process, and always-available L0/L1 context
  without hitting a model.
- **Use kuzu-memory** when you want Python-ecosystem integration, per-project
  isolation with minimal setup, optional LLM reranking, and a simpler flat
  graph model.

---

## Performance Targets

| Operation | Target |
|-----------|--------|
| L0 + L1 retrieval | sub-5 ms (in-memory) |
| L2 HNSW search (top-10) | sub-50 ms |
| L3 deep search (top-50) | sub-150 ms |
| Palace cold start | under 200 ms |

---

## License

[Elastic License 2.0](./LICENSE) (ELv2).
