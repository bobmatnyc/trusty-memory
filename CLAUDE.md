# trusty-memory

Machine-wide, blazingly fast AI memory service built in Rust using the Memory
Palace (mempalace) architecture. Single install per machine, multiple named
palaces, MCP integration for Claude Code.

> **Coordination:** Shared library patterns, consistent conventions, and CI/CD configuration for this project are managed by [trusty-common](../trusty-common). See that repo's CLAUDE.md for cross-project guidelines.

## Project Goals

- **Single install per machine.** `cargo install trusty-memory` once;
  every project on the host shares the same long-running daemon.
- **Many palaces.** One palace per project, agent persona, or knowledge
  domain. Each palace is isolated (own vector index, own SQLite DB, own
  identity.txt) but lives in the same registry.
- **Memory Palace techniques.** A 5-level spatial hierarchy
  (`Palace -> Wing -> Room -> Closet -> Drawer`) gives every memory a stable
  *location* — Closets store pre-computed `topic|entities -> drawer_ids`
  pointer indexes that let us answer most queries without touching the vector
  store.
- **Dual-store retrieval.** Dense vectors (usearch HNSW) handle semantic
  similarity; SQLite WAL holds a temporal knowledge graph for relational and
  time-bounded facts (`valid_from` / `valid_to`).
- **4-layer progressive retrieval.** L0 identity + L1 essential drawers are
  always loaded (~900 tokens). L2 fires on topic match. L3 is deep semantic
  search, opt-in.
- **MCP integration.** Exposes a stdio MCP server (and optional HTTP/SSE) so
  Claude Code can call memory tools directly.
- **Standalone.** Zero dependency on `trusty-search` or any other Trusty tool.

## Architecture

```
Machine-wide service (single install)
  └── Registry: DashMap<PalaceId, Arc<PalaceHandle>>
        └── Palace (namespace, e.g. one per project or domain)
              └── Wing (top-level domain: project area or agent persona)
                    └── Room (topic: Frontend/Backend/Testing/Planning/Research/General/...)
                          └── Closet (pre-computed "topic|entities|→drawer_ids" index)
                                └── Drawer (atomic memory unit: verbatim text + metadata)
```

### Progressive retrieval

| Layer | Source                                         | Tokens   | When                  |
|------:|:-----------------------------------------------|---------:|:----------------------|
| **L0** | `<palace>/identity.txt`                       |   ~100   | always loaded         |
| **L1** | top-15 drawers by importance (pre-cached)     |   ~800   | always loaded         |
| **L2** | metadata-filtered HNSW search                 | variable | topic match in query  |
| **L3** | full HNSW search across the palace            | variable | explicit deep query   |

L0 and L1 are pre-cached on the `PalaceHandle` at palace open; reads never
touch disk. L2/L3 take an `Arc<RwLock<usearch::Index>>` read lock — many
concurrent searches never block each other.

### Temporal knowledge graph (SQLite WAL)

```sql
entities(id, name, entity_type, properties JSON);

triples(id, subject, predicate, object,
        valid_from, valid_to, confidence, provenance);

-- valid_to IS NULL  =>  fact currently active
-- A new contradicting assert closes the prior interval
-- (sets valid_to = now()) and inserts a new active row.
```

WAL mode allows concurrent readers + a single writer, matching our
many-readers / few-writers profile.

## Stack

- **Language:** Rust 2021
- **Async runtime:** `tokio` (`full` features)
- **HTTP / SSE:** `axum 0.7` + `tower-http` (CORS, trace)
- **Vector index:** `usearch 2.25` (HNSW, ANN)
- **Embeddings:** `fastembed 5` (local ONNX, all-MiniLM-L6-v2, 384-d)
- **SQLite:** `rusqlite` (bundled, chrono) + `r2d2` + `r2d2_sqlite`
- **Concurrency:** `dashmap`, `parking_lot`
- **Serialization:** `serde` + `serde_json`
- **Errors:** `anyhow` (binary), `thiserror` (libraries)
- **Logging:** `tracing` + `tracing-subscriber` (env filter)
- **CLI:** `clap 4` (derive)
- **Token counting:** `tiktoken-rs`
- **Cache:** `lru`

## Key conventions

### Palace naming

- Lowercase, kebab-case (e.g. `trusty-memory`, `client-acme`, `agent-pm`).
- One palace per project or major domain.
- Palace ids are stable and used as directory names under the data root.

### Room types

`RoomType` is a closed enum with a `Custom(String)` escape hatch:
`Frontend | Backend | Testing | Planning | Documentation | Research |
Configuration | Meetings | General | Custom(String)`.
Use a stock variant whenever possible — closets keyed by stock RoomType cluster
better.

### Drawer importance

`importance: f32` in `[0.0, 1.0]`. Default `0.5`. Drives L1 selection (top-15)
and breaks ties in L2/L3 ranking. Bump to `>= 0.8` for facts that should always
be in baseline context.

### Temporal KG schema

- Every `assert(s, p, o)` opens a new active interval and closes any prior
  active interval with the same `(s, p)`.
- `query_active(subject)` = `WHERE subject = ? AND valid_to IS NULL`.
- Provenance is free-form (drawer id, URL, agent name) — keep it short.

### Concurrency rules

- **Registry:** `DashMap` — concurrent inserts and lookups, never blocks.
- **`PalaceHandle`:** `Arc<PalaceHandle>` — cheap clone, safe across tasks.
- **Vector store:** `Arc<RwLock<usearch::Index>>` — read locks for `search`,
  write lock only for `upsert` / `remove`.
- **KG:** `r2d2` connection pool over SQLite WAL; readers and writer don't
  block each other.

## Data model (Rust)

See [`crates/trusty-memory-core/src/palace.rs`](crates/trusty-memory-core/src/palace.rs):

```rust
pub struct PalaceId(pub String);

pub struct Palace {
    pub id: PalaceId,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub data_dir: PathBuf,
}

pub struct Wing { id: Uuid, palace_id: PalaceId, name: String }

pub enum RoomType { Frontend, Backend, Testing, Planning,
                    Documentation, Research, Configuration,
                    Meetings, General, Custom(String) }

pub struct Room { id: Uuid, wing_id: Uuid, room_type: RoomType }

pub struct Drawer {
    id: Uuid,
    room_id: Uuid,
    content: String,
    importance: f32,
    source_file: Option<PathBuf>,
    created_at: DateTime<Utc>,
    tags: Vec<String>,
}

pub struct Triple {
    subject: String,
    predicate: String,
    object: String,
    valid_from: DateTime<Utc>,
    valid_to: Option<DateTime<Utc>>,
    confidence: f32,
    provenance: Option<String>,
}
```

## CLI

```
trusty-memory serve [--http <addr>]   # MCP stdio server (and optional HTTP/SSE)
trusty-memory palace new <name>       # create a palace
trusty-memory palace list             # list palaces on this machine
trusty-memory remember <palace> <text> [--room <name>]
trusty-memory recall <palace> <query> [--top-k N]
trusty-memory status                  # daemon health and palace summary
```

## MCP tools

The MCP server exposes (at minimum) the following tools:

| Tool                  | Args                                                            | Returns        |
|-----------------------|------------------------------------------------------------------|----------------|
| `memory_remember`     | `palace, text, room?, tags?`                                    | `drawer_id`    |
| `memory_recall`       | `palace, query, top_k?`  (L0 + L1 + L2)                          | `Vec<Drawer>`  |
| `memory_recall_deep`  | `palace, query, top_k?`  (L3)                                    | `Vec<Drawer>`  |
| `palace_create`       | `name, description?`                                            | `PalaceId`     |
| `palace_list`         | —                                                                | `Vec<PalaceId>`|
| `kg_assert`           | `palace, subject, predicate, object, confidence?, provenance?`   | `()`           |
| `kg_query`            | `palace, subject`                                                | `Vec<Triple>`  |

## Performance targets

- **L0 + L1 retrieval:** sub-5 ms (in-memory, pre-cached).
- **L2 retrieval (HNSW + metadata filter, top-10):** sub-50 ms.
- **L3 deep search (HNSW, top-50):** sub-150 ms.
- **Concurrent reads:** zero global locks; per-palace `RwLock` read side only.
- **Cold start of an existing palace:** under 200 ms for typical project size
  (a few thousand drawers).

## Multi-request design

- One `tokio::main` runtime; many concurrent requests share it.
- `Arc<AppState>` is cloned into every axum handler.
- `PalaceRegistry` lookups never block (DashMap).
- Vector / KG handles inside a `PalaceHandle` use read-write split:
  reads are concurrent, writes serialize per-palace only.
- Long-running embed jobs run on `tokio::task::spawn_blocking` — never on the
  async reactor.

## Development

```bash
# Build
cargo build
cargo build --release

# Test
cargo test
cargo test -p trusty-memory-core

# Run the daemon
RUST_LOG=info cargo run -- serve
RUST_LOG=debug cargo run -- serve --http 127.0.0.1:3031

# Lint (CI requirement)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

### Coding rules

- **No `unwrap()` in library code.** Return `Result<_, _>`.
- **No `panic!` in library code** unless it represents a true invariant.
- **`thiserror` for crates, `anyhow` for the binary.**
- **Every public function** gets a `Why / What / Test` doc comment.
- **`cargo clippy --deny warnings` must pass.**

### Project layout

```
trusty-memory/
├── Cargo.toml                  # workspace + bin manifest
├── CLAUDE.md                   # this file
├── README.md
├── .gitignore
├── .open-mpm/
│   └── agents/
│       ├── pm.toml             # PM orchestrator agent
│       └── engineer.toml       # Rust engineer agent
├── crates/
│   ├── trusty-memory-core/     # types, storage, retrieval
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── palace.rs       # Palace / Wing / Room / Drawer types
│   │       ├── registry.rs     # DashMap-backed palace registry
│   │       ├── embed.rs        # Embedder trait + FastEmbedder stub
│   │       ├── retrieval.rs    # 4-layer progressive retrieval
│   │       └── store/
│   │           ├── mod.rs
│   │           ├── vector.rs   # VectorStore trait + UsearchStore stub
│   │           └── kg.rs       # SQLite temporal KG stub
│   └── trusty-memory-mcp/      # MCP server (stdio + HTTP/SSE)
│       └── src/
│           ├── lib.rs
│           └── tools.rs        # MCP tool surface
├── src/
│   └── main.rs                 # CLI entry point
└── tests/
    └── integration/
        └── basic_palace_test.rs
```

## Implementation roadmap

1. **Vector store** — wire `usearch::Index` behind `Arc<RwLock<_>>`, persist
   to `<palace>/index.usearch`.
2. **Embedder** — load `fastembed::TextEmbedding` (all-MiniLM-L6-v2) on first
   use; warm a small batch on palace open.
3. **KG** — open SQLite WAL, run migrations, implement `assert` /
   `query_active` with prior-interval close-out.
4. **Retrieval L2/L3** — combine vector hits + closet pointer index +
   importance to produce final ranking.
5. **MCP server** — stdio first, then axum HTTP/SSE; route tool calls to core
   APIs.
6. **CLI** — connect each subcommand to the registry / core APIs.
7. **Persistence** — palace metadata, identity.txt, L1 cache snapshot.
8. **Bench + clippy gate in CI.**
