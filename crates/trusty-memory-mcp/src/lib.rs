//! MCP server (stdio + HTTP/SSE) for trusty-memory.
//!
//! Why: Claude Code and other MCP-aware clients integrate with trusty-memory
//! through the standardized Model Context Protocol; we expose memory + KG
//! tools so they can be called by name.
//! What: Re-exports the server type and tool registration entry points.
//! Test: `cargo test -p trusty-memory-mcp` (currently builds-only).

pub mod tools;

pub use tools::MemoryMcpServer;
