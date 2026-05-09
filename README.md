# trusty-memory

**Machine-wide, blazingly fast AI memory service** built in Rust, using the
Memory Palace (mempalace) architecture.

> One install per machine. Multiple named palaces. Sub-5ms baseline retrieval.

## What it is

`trusty-memory` is a long-running service that gives LLM agents persistent
memory with surgical retrieval cost. Each project (or domain, or persona) gets
its own *palace* — a 5-level spatial namespace
(`Palace -> Wing -> Room -> Closet -> Drawer`). Drawers are atomic memory units;
they live in both a vector index (usearch HNSW) and a temporal knowledge graph
(SQLite WAL).

Retrieval is layered:

| Layer | Source | Token budget | When |
|------:|:-------|-------------:|:-----|
| **L0** | `identity.txt` | ~100  | always loaded |
| **L1** | top-15 drawers by importance | ~800 | always loaded |
| **L2** | metadata-filtered vector search | variable | topic match |
| **L3** | full deep semantic search | variable | explicit query |

## Why it exists

Per-project memory tools (one DB per repo) re-pay startup cost on every shell.
`trusty-memory` is a single daemon with a `DashMap<PalaceId, Arc<PalaceHandle>>`
registry — palaces stay hot, concurrent reads never block, and `cargo install
trusty-memory` works standalone with zero dependency on other tools.

## Status

**Scaffold.** Types compile, tests pass, MCP tool surface is sketched. Storage
backends and MCP wire protocol are next. See [`CLAUDE.md`](./CLAUDE.md) for the
full architecture and implementation plan.

## Quick look

```bash
cargo build
cargo test

# CLI surface (subcommands stubbed)
cargo run -- --help
cargo run -- status
cargo run -- palace new my-project
cargo run -- remember my-project "trusty-memory uses HNSW + SQLite WAL"
cargo run -- recall my-project "what storage does trusty-memory use?"

# MCP server (stubbed)
RUST_LOG=info cargo run -- serve --http 127.0.0.1:3031
```

## License

MIT.
