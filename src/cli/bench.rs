//! `bench compare` subcommand: competitive retrieval benchmarking harness.
//!
//! Why: We need an objective, reproducible way to compare trusty-memory's
//! retrieval quality and latency against peer systems (mempalace, kuzu-memory).
//! A labeled fixture corpus + standard IR metrics (Recall@K, MRR) gives us a
//! single number per system to track over time.
//! What: Loads a JSONL corpus of documents and labeled queries, drives every
//! enabled system through a `SystemDriver` trait (in-process for trusty-memory,
//! MCP stdio subprocess for the rest), and prints a metrics table.
//! Test: `bench_corpus_parse` exercises corpus loading; `trusty_driver_recall_at_5`
//! end-to-end-tests the trusty driver against the fixture corpus.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use trusty_memory_core::embed::{Embedder, FastEmbedder};
use trusty_memory_core::retrieval::recall;
use trusty_memory_core::store::VectorStore;
use trusty_memory_core::{Drawer, Palace, PalaceId, PalaceRegistry};
use uuid::Uuid;

// ── Corpus types ────────────────────────────────────────────────────────────

/// One row of the bench corpus JSONL: either a document to insert or a labeled
/// query to evaluate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CorpusRow {
    Document(BenchDoc),
    Query(BenchQuery),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchDoc {
    pub id: String,
    pub content: String,
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchQuery {
    pub id: String,
    pub text: String,
    pub relevant: Vec<String>,
}

/// Parsed corpus.
#[derive(Debug, Clone, Default)]
pub struct BenchCorpus {
    pub docs: Vec<BenchDoc>,
    pub queries: Vec<BenchQuery>,
}

impl BenchCorpus {
    /// Parse a JSONL corpus file from disk.
    ///
    /// Why: Bench fixtures live next to tests as JSONL so they can be edited
    /// without touching code; loading is a tiny pure function we can unit-test.
    /// What: Reads the file line-by-line, ignoring blanks and comments
    /// (lines beginning with `#`), and decodes each line as a `CorpusRow`.
    /// Test: `bench_corpus_parse` loads the fixture and asserts 20 docs + 10
    /// queries.
    pub fn from_path(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read bench corpus {}", path.display()))?;
        let mut corpus = BenchCorpus::default();
        for (lineno, line) in raw.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let row: CorpusRow = serde_json::from_str(trimmed).with_context(|| {
                format!("parse corpus line {} in {}", lineno + 1, path.display())
            })?;
            match row {
                CorpusRow::Document(d) => corpus.docs.push(d),
                CorpusRow::Query(q) => corpus.queries.push(q),
            }
        }
        Ok(corpus)
    }
}

// ── Driver trait ────────────────────────────────────────────────────────────

/// Abstract retrieval system under test.
///
/// Why: We want to swap in mempalace, kuzu-memory, or any future MCP-speaking
/// system without changing the bench loop.
/// What: Two ops — `insert` writes one document, `query` returns ranked doc ids.
/// Test: `TrustyDriver` covers the in-process implementation; MCP drivers are
/// exercised manually against real binaries.
#[async_trait]
pub trait SystemDriver: Send + Sync {
    async fn insert(&self, doc: &BenchDoc) -> Result<()>;
    async fn query(&self, q: &str, top_k: usize) -> Result<Vec<String>>;
    fn name(&self) -> &str;
}

// ── Trusty driver (in-process) ──────────────────────────────────────────────

/// In-process driver that uses trusty-memory's own `PalaceRegistry`.
///
/// Why: The whole point is to measure trusty-memory honestly, on the same
/// embeddings + retrieval path that production uses.
/// What: Owns a `tempfile::TempDir` data root, a fresh palace, a shared
/// `FastEmbedder`, and a doc-id table mapping bench `d1..d20` ids to drawer
/// content (so `query` can recover bench ids from returned drawers).
/// Test: `trusty_driver_recall_at_5` inserts the fixture corpus and asserts
/// Recall@5 >= 0.80.
pub struct TrustyDriver {
    name: String,
    handle: Arc<trusty_memory_core::PalaceHandle>,
    embedder: Arc<FastEmbedder>,
    /// drawer.id (UUID) -> bench doc id (e.g. "d1"). Lets `query` map results
    /// back to the labeled corpus ids.
    id_table: Arc<Mutex<HashMap<Uuid, String>>>,
    // Held to keep the temp data root alive for the driver's lifetime.
    _tmp: Arc<tempfile::TempDir>,
}

impl TrustyDriver {
    pub async fn new() -> Result<Self> {
        let tmp = tempfile::tempdir().context("allocate trusty bench tempdir")?;
        let data_root = tmp.path().to_path_buf();
        let palace_id = PalaceId::new("bench");
        let palace = Palace {
            id: palace_id.clone(),
            name: "bench".to_string(),
            description: Some("competitive bench palace".into()),
            created_at: chrono::Utc::now(),
            data_dir: data_root.join("bench"),
        };

        let registry = PalaceRegistry::new();
        let handle = registry
            .create_palace(&data_root, palace)
            .context("create bench palace")?;

        let embedder = Arc::new(
            FastEmbedder::new()
                .await
                .context("init FastEmbedder for bench")?,
        );

        Ok(Self {
            name: "trusty-memory".into(),
            handle,
            embedder,
            id_table: Arc::new(Mutex::new(HashMap::new())),
            _tmp: Arc::new(tmp),
        })
    }
}

#[async_trait]
impl SystemDriver for TrustyDriver {
    async fn insert(&self, doc: &BenchDoc) -> Result<()> {
        let drawer = Drawer::new(Uuid::nil(), &doc.content);
        let drawer_id = drawer.id;

        let vecs = self
            .embedder
            .embed_batch(std::slice::from_ref(&doc.content))
            .await
            .context("embed bench doc")?;
        let vec0 = vecs
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("embedder returned no vectors"))?;

        self.handle
            .vector_store
            .upsert(drawer_id, vec0)
            .await
            .context("upsert vector for bench doc")?;
        self.handle.add_drawer(drawer);

        let mut tbl = self.id_table.lock().await;
        tbl.insert(drawer_id, doc.id.clone());
        Ok(())
    }

    async fn query(&self, q: &str, top_k: usize) -> Result<Vec<String>> {
        let results = recall(&self.handle, self.embedder.as_ref(), q, top_k)
            .await
            .context("recall bench query")?;
        let tbl = self.id_table.lock().await;
        let mut out: Vec<String> = Vec::new();
        for r in results {
            // Skip the synthetic L0 identity (nil UUID).
            if r.drawer.id.is_nil() {
                continue;
            }
            // Vector hits map back to drawers via 8-byte prefix; the drawer
            // table on the handle holds the canonical UUID, so a direct lookup
            // works. For L1 hits the id matches exactly.
            if let Some(bench_id) = tbl.get(&r.drawer.id) {
                if !out.contains(bench_id) {
                    out.push(bench_id.clone());
                }
            }
            if out.len() >= top_k {
                break;
            }
        }
        Ok(out)
    }

    fn name(&self) -> &str {
        &self.name
    }
}

// ── MCP subprocess driver ───────────────────────────────────────────────────

/// Configuration for an MCP-speaking peer system.
///
/// Why: Different MCP memory servers (mempalace, kuzu-memory) use different
/// tool names and argument shapes. Rather than hard-coding one shape, we
/// parameterize the driver with closures that map bench data into the
/// system's expected JSON.
/// What: Carries the spawn command, env, working dir, and the per-system
/// (insert_tool, search_tool) names and arg/extract functions.
/// Test: `mempalace_config` and `kuzu_config` produce configs used by the
/// integration run; the field set is exercised end-to-end by `handle_bench_compare`.
/// `(result, corpus_pairs) -> matching bench ids`.
pub type ExtractIdsFn = fn(&serde_json::Value, &[(String, String)]) -> Vec<String>;

pub struct McpDriverConfig {
    pub name: String,
    pub cmd: Vec<String>,
    pub env: Vec<(String, String)>,
    pub working_dir: Option<PathBuf>,
    pub insert_tool: String,
    pub insert_args_fn: fn(&BenchDoc) -> serde_json::Value,
    pub search_tool: String,
    pub search_args_fn: fn(&str, usize) -> serde_json::Value,
    pub extract_ids_fn: ExtractIdsFn,
}

/// Drives a peer system over MCP stdio (JSON-RPC 2.0).
///
/// Why: mempalace and kuzu-memory both expose MCP servers; talking to them via
/// their published surface is the most honest comparison we can run.
/// What: Spawns the configured command as a subprocess, performs the
/// `initialize` handshake, then maps `insert` / `query` to `tools/call`
/// using the per-system tool names and arg builders.
/// Test: Manual — run against real `mempalace-mcp` and `kuzu-memory mcp`
/// binaries via `bench compare --mempalace --kuzu`.
pub struct McpDriver {
    name: String,
    cfg: McpDriverConfig,
    /// Mapping of bench doc ids to their content; we scan the recall response
    /// text for these substrings since MCP returns free-text content blocks.
    doc_index: Arc<Mutex<Vec<(String, String)>>>,
    inner: Arc<Mutex<McpInner>>,
}

struct McpInner {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpDriver {
    pub async fn spawn(cfg: McpDriverConfig) -> Result<Self> {
        let program = cfg
            .cmd
            .first()
            .ok_or_else(|| anyhow!("empty mcp command for {}", cfg.name))?
            .clone();
        let args: Vec<String> = cfg.cmd.iter().skip(1).cloned().collect();

        let mut command = Command::new(&program);
        command
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        for (k, v) in &cfg.env {
            command.env(k, v);
        }
        if let Some(dir) = &cfg.working_dir {
            command.current_dir(dir);
        }

        let mut child = command
            .spawn()
            .with_context(|| format!("spawn MCP server: {} {:?}", program, args))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("mcp child has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("mcp child has no stdout"))?;

        let mut inner = McpInner {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        };

        // Initialize handshake — best-effort; some servers ignore this.
        let init_params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "trusty-memory-bench", "version": "0.1"},
        });
        let _ = inner.rpc("initialize", init_params).await;

        let name = cfg.name.clone();
        Ok(Self {
            name,
            cfg,
            doc_index: Arc::new(Mutex::new(Vec::new())),
            inner: Arc::new(Mutex::new(inner)),
        })
    }
}

// ── Per-system MCP configs ──────────────────────────────────────────────────

/// Rank-preserving content matcher for mempalace `mempalace_search` responses.
///
/// Why: `mempalace_search` returns a JSON object (embedded as a string inside
/// `content[0].text`) with a `results` array ordered by relevance score.
/// The previous `extract_by_content_match` iterated the *corpus* in insertion
/// order and emitted every doc whose 40-char prefix appeared anywhere in the
/// blob — causing results to come out in corpus order (d1 before d20) rather
/// than in the rank order mempalace computed. This destroyed R@1 and MRR for
/// any query whose highest-ranked doc was not also the lowest-numbered doc in
/// the corpus.
/// What: Parses the embedded JSON from `content[0].text`, iterates the
/// `results[]` array in the order the server returned them, and maps each
/// `result.text` back to a bench doc id by 40-char prefix comparison against
/// the corpus lookup table. Falls back to the generic blob scan if the JSON
/// does not have a `results` key (e.g. older server versions).
/// Test: Exercised end-to-end via `bench compare --mempalace`.
fn extract_mempalace_search(
    result: &serde_json::Value,
    corpus: &[(String, String)],
) -> Vec<String> {
    // Build a prefix -> bench-id lookup once (O(n) on corpus size).
    let prefix_map: Vec<(String, &str)> = corpus
        .iter()
        .map(|(id, content)| {
            let prefix: String = content.chars().take(40).collect();
            (prefix.trim().to_string(), id.as_str())
        })
        .collect();

    // Extract the raw text from content[0].text (mempalace embeds JSON there).
    let text = result
        .get("content")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Try to parse the structured JSON response and iterate results in rank order.
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(results_arr) = parsed.get("results").and_then(|v| v.as_array()) {
            let mut out: Vec<String> = Vec::new();
            for hit in results_arr {
                let hit_text = hit.get("text").and_then(|v| v.as_str()).unwrap_or("");
                // Match by 40-char prefix against corpus entries.
                for (prefix, bench_id) in &prefix_map {
                    if !prefix.is_empty() && hit_text.starts_with(prefix.as_str()) {
                        let id = bench_id.to_string();
                        if !out.contains(&id) {
                            out.push(id);
                        }
                        break;
                    }
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
    }

    // Fallback: generic blob scan (corpus-order, used if JSON parse fails).
    let blob = if text.is_empty() {
        result.to_string()
    } else {
        text.to_string()
    };
    let mut out: Vec<String> = Vec::new();
    for (prefix, bench_id) in &prefix_map {
        if !prefix.is_empty() && blob.contains(prefix.as_str()) {
            let id = bench_id.to_string();
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// Generic content-prefix matcher for MCP systems that return free-text blobs.
///
/// Why: kuzu_recall and other systems that do not return structured JSON need a
/// fallback that at least finds which docs are present. Rank fidelity is best-
/// effort here since there is no machine-readable rank signal.
/// What: Concatenates `result.content[*].text` (or stringifies the result),
/// then walks `corpus` in insertion order and emits each id whose 40-char
/// content prefix appears.
/// Test: Exercised end-to-end via `bench compare --kuzu`.
fn extract_by_content_match(
    result: &serde_json::Value,
    corpus: &[(String, String)],
) -> Vec<String> {
    let mut blob = String::new();
    if let Some(arr) = result.get("content").and_then(|v| v.as_array()) {
        for c in arr {
            if let Some(t) = c.get("text").and_then(|v| v.as_str()) {
                blob.push_str(t);
                blob.push('\n');
            }
        }
    }
    if blob.is_empty() {
        blob = result.to_string();
    }
    let mut out: Vec<String> = Vec::new();
    for (id, content) in corpus {
        let needle: String = content.chars().take(40).collect();
        let needle = needle.trim();
        if !needle.is_empty() && blob.contains(needle) && !out.contains(id) {
            out.push(id.clone());
        }
    }
    out
}

/// Build an MCP driver config for `mempalace-mcp` v3.3+.
///
/// Why: mempalace exposes `mempalace_add_drawer` and `mempalace_search` with
/// `wing` / `room` / `content` / `query` / `limit` arg shapes that differ
/// from our own surface.
/// What: Returns a config that targets a pre-initialized palace at
/// `palace_path` via `MEMPALACE_PATH`.
/// Test: `bench compare --mempalace` against the bench fixture.
pub fn mempalace_config(palace_path: PathBuf) -> McpDriverConfig {
    McpDriverConfig {
        name: "mempalace".into(),
        cmd: vec![
            "mempalace-mcp".into(),
            "--palace".into(),
            palace_path.display().to_string(),
        ],
        env: vec![],
        working_dir: None,
        insert_tool: "mempalace_add_drawer".into(),
        insert_args_fn: |doc| {
            serde_json::json!({
                "wing": "bench",
                "room": doc.room.clone().unwrap_or_else(|| "General".to_string()),
                "content": doc.content,
            })
        },
        search_tool: "mempalace_search".into(),
        search_args_fn: |q, k| serde_json::json!({"query": q, "limit": k}),
        extract_ids_fn: extract_mempalace_search,
    }
}

/// Build an MCP driver config for `kuzu-memory mcp` v1.12+.
///
/// Why: kuzu-memory uses `kuzu_remember` (sync store) and `kuzu_recall`
/// (semantic retrieval); it stores per-directory state, so we run it inside
/// a freshly initialized temp dir.
/// What: Returns a config with `working_dir` set to the pre-initialized
/// kuzu project directory.
/// Test: `bench compare --kuzu` against the bench fixture.
pub fn kuzu_config(project_dir: PathBuf) -> McpDriverConfig {
    McpDriverConfig {
        name: "kuzu-memory".into(),
        cmd: vec!["kuzu-memory".into(), "mcp".into()],
        env: vec![],
        working_dir: Some(project_dir),
        insert_tool: "kuzu_remember".into(),
        insert_args_fn: |doc| {
            serde_json::json!({
                "content": doc.content,
                "memory_type": "identity",
                "knowledge_type": "note",
                "importance": 0.8,
            })
        },
        search_tool: "kuzu_recall".into(),
        search_args_fn: |q, k| serde_json::json!({"query": q, "limit": k}),
        extract_ids_fn: extract_by_content_match,
    }
}

impl McpInner {
    async fn rpc(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let line = format!("{}\n", serde_json::to_string(&req)?);
        self.stdin
            .write_all(line.as_bytes())
            .await
            .context("write rpc request")?;
        self.stdin.flush().await.ok();

        let mut buf = String::new();
        self.stdout
            .read_line(&mut buf)
            .await
            .context("read rpc response line")?;
        if buf.is_empty() {
            return Err(anyhow!("mcp server closed stdout"));
        }
        let resp: serde_json::Value = serde_json::from_str(buf.trim())
            .with_context(|| format!("parse rpc response: {buf:?}"))?;
        if let Some(err) = resp.get("error") {
            return Err(anyhow!("rpc error: {err}"));
        }
        Ok(resp
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }
}

#[async_trait]
impl SystemDriver for McpDriver {
    async fn insert(&self, doc: &BenchDoc) -> Result<()> {
        {
            let mut idx = self.doc_index.lock().await;
            idx.push((doc.id.clone(), doc.content.clone()));
        }
        let arguments = (self.cfg.insert_args_fn)(doc);
        let params = serde_json::json!({
            "name": self.cfg.insert_tool,
            "arguments": arguments,
        });
        let mut inner = self.inner.lock().await;
        inner.rpc("tools/call", params).await.map(|_| ())
    }

    async fn query(&self, q: &str, top_k: usize) -> Result<Vec<String>> {
        let arguments = (self.cfg.search_args_fn)(q, top_k);
        let params = serde_json::json!({
            "name": self.cfg.search_tool,
            "arguments": arguments,
        });
        let result = {
            let mut inner = self.inner.lock().await;
            inner.rpc("tools/call", params).await?
        };
        let idx = self.doc_index.lock().await;
        let mut out = (self.cfg.extract_ids_fn)(&result, idx.as_slice());
        if out.len() > top_k {
            out.truncate(top_k);
        }
        Ok(out)
    }

    fn name(&self) -> &str {
        &self.name
    }
}

// ── Metrics ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct SystemMetrics {
    pub name: String,
    pub recall_at_1: f32,
    pub recall_at_k: f32,
    pub mrr: f32,
    pub mean_latency_ms: f32,
    #[allow(dead_code)]
    pub queries_run: usize,
}

/// Compute recall/MRR/latency for one system over all queries.
///
/// Why: Pure function over (results, expected) keeps the metrics testable in
/// isolation from the driver layer.
/// What: Recall@1 = fraction of queries whose top-1 hit is relevant; Recall@K
/// = fraction with at least one relevant in top-K; MRR = mean reciprocal rank
/// of the first relevant result (0 if none).
/// Test: Implicitly covered by `trusty_driver_recall_at_5`.
pub fn compute_metrics(
    name: &str,
    per_query: &[(Vec<String>, Vec<String>, f32)],
    top_k: usize,
) -> SystemMetrics {
    if per_query.is_empty() {
        return SystemMetrics {
            name: name.to_string(),
            ..Default::default()
        };
    }
    let n = per_query.len() as f32;
    let mut r1 = 0.0f32;
    let mut rk = 0.0f32;
    let mut mrr = 0.0f32;
    let mut lat_sum = 0.0f32;
    for (results, relevant, lat_ms) in per_query {
        lat_sum += lat_ms;
        let top = results.iter().take(top_k);
        let mut first_rank: Option<usize> = None;
        for (rank, id) in top.enumerate() {
            if relevant.iter().any(|r| r == id) {
                first_rank = Some(rank + 1);
                break;
            }
        }
        if let Some(rank) = first_rank {
            rk += 1.0;
            mrr += 1.0 / rank as f32;
            if rank == 1 {
                r1 += 1.0;
            }
        }
    }
    SystemMetrics {
        name: name.to_string(),
        recall_at_1: r1 / n,
        recall_at_k: rk / n,
        mrr: mrr / n,
        mean_latency_ms: lat_sum / n,
        queries_run: per_query.len(),
    }
}

fn print_table(rows: &[SystemMetrics], top_k: usize) {
    println!();
    println!(
        "{:<14} {:>6} {:>6} {:>6} {:>14}",
        "System",
        "R@1",
        format!("R@{top_k}"),
        "MRR",
        "Latency(ms)"
    );
    println!("{}", "─".repeat(50));
    for r in rows {
        println!(
            "{:<14} {:>6.2} {:>6.2} {:>6.2} {:>14.2}",
            r.name, r.recall_at_1, r.recall_at_k, r.mrr, r.mean_latency_ms
        );
    }
    println!();
}

// ── Driver factory ──────────────────────────────────────────────────────────

/// CLI options for `bench compare`.
#[derive(Debug, Clone)]
pub struct BenchCompareOpts {
    pub corpus: PathBuf,
    pub top_k: usize,
    pub mempalace: bool,
    pub kuzu: bool,
    pub json: bool,
}

/// Initialize a fresh mempalace at `path` via the `mempalace init` CLI.
///
/// Why: `mempalace-mcp` needs an existing palace directory; we create a
/// scratch one per bench run so results are reproducible.
/// What: Runs `mempalace init <path> --yes`, returning the path on success.
/// Test: Exercised by `bench compare --mempalace`.
fn init_mempalace_palace() -> Result<(PathBuf, tempfile::TempDir)> {
    let tmp = tempfile::tempdir().context("allocate mempalace bench tempdir")?;
    let path = tmp.path().to_path_buf();
    let status = std::process::Command::new("mempalace")
        .arg("init")
        .arg(&path)
        .arg("--yes")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("invoke `mempalace init`")?;
    if !status.success() {
        return Err(anyhow!("`mempalace init` exited with status {status:?}"));
    }
    // mempalace requires at least one mine pass to actually materialize the
    // palace database; without it the MCP server reports "No palace found".
    // Drop a tiny seed file and mine the directory.
    let seed = path.join(".bench_seed.txt");
    std::fs::write(&seed, "bench seed file\n").context("write mempalace seed file")?;
    let status = std::process::Command::new("mempalace")
        .arg("--palace")
        .arg(&path)
        .arg("mine")
        .arg(&path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("invoke `mempalace mine`")?;
    if !status.success() {
        return Err(anyhow!("`mempalace mine` exited with status {status:?}"));
    }
    Ok((path, tmp))
}

/// Initialize a fresh kuzu-memory project at `path` via `kuzu-memory init`.
///
/// Why: kuzu-memory keeps state per project directory; the MCP server picks
/// up `./.kuzu-memory/` from its cwd.
/// What: Creates a scratch dir, runs `kuzu-memory init` inside it, and
/// returns the path so the MCP driver can `current_dir(...)` to it.
/// Test: Exercised by `bench compare --kuzu`.
fn init_kuzu_project() -> Result<(PathBuf, tempfile::TempDir)> {
    let tmp = tempfile::tempdir().context("allocate kuzu bench tempdir")?;
    let path = tmp.path().to_path_buf();
    let status = std::process::Command::new("kuzu-memory")
        .arg("init")
        .current_dir(&path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("invoke `kuzu-memory init`")?;
    if !status.success() {
        return Err(anyhow!("`kuzu-memory init` exited with status {status:?}"));
    }
    Ok((path, tmp))
}

/// Holds tempdirs alongside drivers so they live for the bench duration.
pub struct DriverBundle {
    pub drivers: Vec<Box<dyn SystemDriver>>,
    _tempdirs: Vec<tempfile::TempDir>,
}

/// Build the set of drivers requested by the user.
async fn build_drivers(opts: &BenchCompareOpts) -> Result<DriverBundle> {
    let mut drivers: Vec<Box<dyn SystemDriver>> = Vec::new();
    let mut tempdirs: Vec<tempfile::TempDir> = Vec::new();

    // trusty-memory is always included.
    drivers.push(Box::new(TrustyDriver::new().await?));

    if opts.mempalace {
        let (palace_path, tmp) = init_mempalace_palace()?;
        tempdirs.push(tmp);
        let cfg = mempalace_config(palace_path);
        drivers.push(Box::new(McpDriver::spawn(cfg).await?));
    }

    if opts.kuzu {
        let (project_dir, tmp) = init_kuzu_project()?;
        tempdirs.push(tmp);
        let cfg = kuzu_config(project_dir);
        drivers.push(Box::new(McpDriver::spawn(cfg).await?));
    }

    Ok(DriverBundle {
        drivers,
        _tempdirs: tempdirs,
    })
}

/// Top-level `bench compare` entry point.
///
/// Why: Stitches corpus loading, driver setup, the query loop, metric
/// computation, and table rendering into one CLI command.
/// What: Returns Ok on success; surfaces driver/corpus errors via anyhow.
/// Test: Manual — full path is exercised by running the CLI; the unit tests
/// cover parsing and the trusty driver in isolation.
pub async fn handle_bench_compare(opts: BenchCompareOpts) -> Result<()> {
    let corpus = BenchCorpus::from_path(&opts.corpus)?;
    if corpus.queries.is_empty() {
        return Err(anyhow!("corpus has no queries"));
    }
    println!(
        "Loaded {} docs and {} queries from {}",
        corpus.docs.len(),
        corpus.queries.len(),
        opts.corpus.display()
    );

    let bundle = build_drivers(&opts).await?;
    let mut all_metrics: Vec<SystemMetrics> = Vec::with_capacity(bundle.drivers.len());

    for driver in bundle.drivers.iter() {
        println!(
            "\n[{}] inserting {} docs ...",
            driver.name(),
            corpus.docs.len()
        );
        for doc in &corpus.docs {
            driver
                .insert(doc)
                .await
                .with_context(|| format!("[{}] insert {}", driver.name(), doc.id))?;
        }

        let mut per_query: Vec<(Vec<String>, Vec<String>, f32)> =
            Vec::with_capacity(corpus.queries.len());
        for q in &corpus.queries {
            let start = Instant::now();
            let results = driver
                .query(&q.text, opts.top_k)
                .await
                .with_context(|| format!("[{}] query {}", driver.name(), q.id))?;
            let elapsed_ms = start.elapsed().as_secs_f32() * 1000.0;
            per_query.push((results, q.relevant.clone(), elapsed_ms));
        }
        let m = compute_metrics(driver.name(), &per_query, opts.top_k);
        all_metrics.push(m);
    }

    if opts.json {
        let payload: Vec<serde_json::Value> = all_metrics
            .iter()
            .map(|m| {
                serde_json::json!({
                    "system": m.name,
                    "recall_at_1": m.recall_at_1,
                    "recall_at_k": m.recall_at_k,
                    "mrr": m.mrr,
                    "mean_latency_ms": m.mean_latency_ms,
                    "queries_run": m.queries_run,
                    "top_k": opts.top_k,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        print_table(&all_metrics, opts.top_k);
    }
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_path() -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests/fixtures/bench_corpus.jsonl");
        p
    }

    #[test]
    fn bench_corpus_parse() {
        let path = fixture_path();
        let corpus = BenchCorpus::from_path(&path).expect("parse corpus");
        assert_eq!(corpus.docs.len(), 100, "expected 100 documents");
        assert_eq!(corpus.queries.len(), 50, "expected 50 queries");
        assert_eq!(corpus.docs[0].id, "d1");
        assert_eq!(corpus.queries[0].id, "q1");
        assert!(!corpus.queries[0].relevant.is_empty());
    }

    #[test]
    fn metrics_perfect_recall() {
        // Two queries, both with the relevant doc at rank 1 -> R@1 = R@5 = MRR = 1.0
        let per_query = vec![
            (vec!["d1".into(), "d2".into()], vec!["d1".into()], 10.0),
            (vec!["d3".into(), "d4".into()], vec!["d3".into()], 20.0),
        ];
        let m = compute_metrics("test", &per_query, 5);
        assert!((m.recall_at_1 - 1.0).abs() < 1e-6);
        assert!((m.recall_at_k - 1.0).abs() < 1e-6);
        assert!((m.mrr - 1.0).abs() < 1e-6);
        assert!((m.mean_latency_ms - 15.0).abs() < 1e-6);
    }

    #[test]
    fn metrics_no_results() {
        let per_query = vec![(vec![], vec!["d1".into()], 5.0)];
        let m = compute_metrics("test", &per_query, 5);
        assert_eq!(m.recall_at_1, 0.0);
        assert_eq!(m.recall_at_k, 0.0);
        assert_eq!(m.mrr, 0.0);
    }

    /// Verifies that `extract_mempalace_search` preserves the rank order
    /// returned by the server's `results[]` array rather than the corpus
    /// insertion order.
    ///
    /// Why: The previous `extract_by_content_match` iterated the corpus in
    /// insertion order, so a doc ranked #1 by mempalace could appear at
    /// position 4 in the output if three lower-numbered docs also appeared in
    /// the top-K blob. This test pins the fixed behaviour.
    /// What: Constructs a synthetic MCP response where d3 ranks first, then
    /// d1; asserts the extraction returns [d3, d1], not [d1, d3].
    /// Test: This test itself.
    #[test]
    fn extract_mempalace_preserves_rank_order() {
        // Corpus: d1, d2, d3 in insertion order.
        let corpus: Vec<(String, String)> = vec![
            (
                "d1".into(),
                "Alpha beta gamma delta epsilon zeta eta theta iota kappa.".into(),
            ),
            (
                "d2".into(),
                "Lambda mu nu xi omicron pi rho sigma tau upsilon phi chi.".into(),
            ),
            (
                "d3".into(),
                "Omega psi chi phi upsilon tau sigma rho pi omicron xi nu.".into(),
            ),
        ];

        // Synthetic mempalace_search response: d3 ranked first, d1 second, d2 absent.
        let response_text = serde_json::json!({
            "query": "test",
            "results": [
                {"text": "Omega psi chi phi upsilon tau sigma rho pi omicron xi nu.", "similarity": 0.9},
                {"text": "Alpha beta gamma delta epsilon zeta eta theta iota kappa.", "similarity": 0.4},
            ]
        })
        .to_string();

        let mcp_result = serde_json::json!({
            "content": [{"type": "text", "text": response_text}]
        });

        let ids = extract_mempalace_search(&mcp_result, &corpus);
        assert_eq!(
            ids,
            vec!["d3", "d1"],
            "expected rank order [d3, d1] (server-returned), got {ids:?}"
        );
    }

    /// Verifies that `extract_mempalace_search` falls back gracefully to the
    /// blob scan when the text is not valid JSON.
    #[test]
    fn extract_mempalace_fallback_on_plain_text() {
        let corpus: Vec<(String, String)> = vec![(
            "d1".into(),
            "The quick brown fox jumps over the lazy dog.".into(),
        )];
        let mcp_result = serde_json::json!({
            "content": [{"type": "text", "text": "The quick brown fox jumps over the lazy dog."}]
        });
        let ids = extract_mempalace_search(&mcp_result, &corpus);
        assert_eq!(ids, vec!["d1"]);
    }

    /// End-to-end: insert all 20 docs into a fresh trusty palace, run all 10
    /// queries, assert Recall@5 >= 0.80.
    ///
    /// Why: Pins the in-process baseline for the bench harness — if we ever
    /// regress retrieval quality on this fixture, this test fails.
    /// What: Builds a `TrustyDriver`, inserts the fixture, queries with top_k=5,
    /// and computes metrics.
    /// Test: This test itself.
    #[tokio::test]
    async fn trusty_driver_recall_at_5() {
        let corpus = BenchCorpus::from_path(&fixture_path()).expect("parse corpus");
        let driver = TrustyDriver::new().await.expect("init trusty driver");

        for doc in &corpus.docs {
            driver.insert(doc).await.expect("insert doc");
        }

        let mut per_query = Vec::new();
        for q in &corpus.queries {
            let start = Instant::now();
            let results = driver.query(&q.text, 5).await.expect("query");
            let elapsed_ms = start.elapsed().as_secs_f32() * 1000.0;
            per_query.push((results, q.relevant.clone(), elapsed_ms));
        }
        let m = compute_metrics("trusty-memory", &per_query, 5);
        eprintln!(
            "trusty bench: R@1={:.2} R@5={:.2} MRR={:.2} latency={:.1}ms",
            m.recall_at_1, m.recall_at_k, m.mrr, m.mean_latency_ms
        );
        // Per-query diagnostic for any future regression tuning.
        for (qidx, (results, relevant, _)) in per_query.iter().enumerate() {
            let qid = &corpus.queries[qidx].id;
            let top1 = results.first().cloned().unwrap_or_default();
            let hit = relevant.iter().any(|r| r == &top1);
            if !hit {
                eprintln!("  miss {qid}: top1={top1:?} expected={relevant:?} got={results:?}");
            }
        }
        assert!(
            m.recall_at_1 >= 0.95,
            "Recall@1 = {} (expected >= 0.95) on 100-doc corpus.",
            m.recall_at_1
        );
        assert!(
            m.recall_at_k >= 0.95,
            "Recall@5 = {} (expected >= 0.95). Per-query: {:?}",
            m.recall_at_k,
            per_query
        );
    }
}
