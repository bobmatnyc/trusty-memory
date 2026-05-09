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
            .embed(std::slice::from_ref(&doc.content))
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

/// Drives a peer system over MCP stdio (JSON-RPC 2.0).
///
/// Why: mempalace and kuzu-memory both expose MCP servers; talking to them via
/// their published surface is the most honest comparison we can run.
/// What: Spawns `cmd` as a subprocess, performs the `initialize` handshake,
/// then maps `insert` / `query` to `tools/call` for `memory_remember` /
/// `memory_recall` (or the configured tool names).
/// Test: Manual — run against a real `mempalace serve` binary.
pub struct McpDriver {
    name: String,
    insert_tool: String,
    query_tool: String,
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
    pub async fn spawn(name: impl Into<String>, command_line: &str) -> Result<Self> {
        let mut parts = command_line.split_whitespace();
        let program = parts
            .next()
            .ok_or_else(|| anyhow!("empty mcp command line: {command_line:?}"))?;
        let args: Vec<&str> = parts.collect();

        let mut child = Command::new(program)
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("spawn MCP server: {command_line}"))?;

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

        Ok(Self {
            name: name.into(),
            insert_tool: "memory_remember".into(),
            query_tool: "memory_recall".into(),
            doc_index: Arc::new(Mutex::new(Vec::new())),
            inner: Arc::new(Mutex::new(inner)),
        })
    }

    /// Override the MCP tool names used for insert/query (default
    /// `memory_remember` / `memory_recall`).
    #[allow(dead_code)]
    pub fn with_tool_names(mut self, insert: &str, query: &str) -> Self {
        self.insert_tool = insert.to_string();
        self.query_tool = query.to_string();
        self
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
        let mut args = serde_json::Map::new();
        args.insert(
            "text".into(),
            serde_json::Value::String(doc.content.clone()),
        );
        if let Some(room) = &doc.room {
            args.insert("room".into(), serde_json::Value::String(room.clone()));
        }
        if !doc.tags.is_empty() {
            args.insert(
                "tags".into(),
                serde_json::Value::Array(
                    doc.tags
                        .iter()
                        .cloned()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );
        }
        let params = serde_json::json!({
            "name": self.insert_tool,
            "arguments": serde_json::Value::Object(args),
        });
        let mut inner = self.inner.lock().await;
        inner.rpc("tools/call", params).await.map(|_| ())
    }

    async fn query(&self, q: &str, top_k: usize) -> Result<Vec<String>> {
        let params = serde_json::json!({
            "name": self.query_tool,
            "arguments": {
                "query": q,
                "top_k": top_k,
            },
        });
        let result = {
            let mut inner = self.inner.lock().await;
            inner.rpc("tools/call", params).await?
        };
        // MCP tool responses use {"content":[{"type":"text","text":"..."}]}.
        // We scan the concatenated text for known doc contents and map them
        // back to bench ids. This is approximate but matches how a human
        // judge would score the response.
        let mut blob = String::new();
        if let Some(arr) = result.get("content").and_then(|v| v.as_array()) {
            for c in arr {
                if let Some(t) = c.get("text").and_then(|v| v.as_str()) {
                    blob.push_str(t);
                    blob.push('\n');
                }
            }
        } else {
            blob = result.to_string();
        }
        let idx = self.doc_index.lock().await;
        let mut out: Vec<String> = Vec::new();
        for (id, content) in idx.iter() {
            // Match on a content prefix to be robust to truncation/formatting.
            let needle: String = content.chars().take(40).collect();
            if blob.contains(needle.trim()) && !out.contains(id) {
                out.push(id.clone());
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
    pub systems: Vec<String>,
    pub mempalace_cmd: Option<String>,
    pub kuzu_cmd: Option<String>,
}

/// Build the set of drivers requested by the user.
async fn build_drivers(opts: &BenchCompareOpts) -> Result<Vec<Box<dyn SystemDriver>>> {
    let mut drivers: Vec<Box<dyn SystemDriver>> = Vec::new();
    let want_trusty = opts.systems.iter().any(|s| s == "trusty");
    let want_mempalace = opts.systems.iter().any(|s| s == "mempalace");
    let want_kuzu = opts.systems.iter().any(|s| s == "kuzu");

    if want_trusty {
        drivers.push(Box::new(TrustyDriver::new().await?));
    }
    if want_mempalace {
        let cmd = opts
            .mempalace_cmd
            .as_deref()
            .ok_or_else(|| anyhow!("--mempalace-cmd required when systems includes mempalace"))?;
        drivers.push(Box::new(McpDriver::spawn("mempalace", cmd).await?));
    }
    if want_kuzu {
        let cmd = opts
            .kuzu_cmd
            .as_deref()
            .ok_or_else(|| anyhow!("--kuzu-cmd required when systems includes kuzu"))?;
        drivers.push(Box::new(McpDriver::spawn("kuzu-memory", cmd).await?));
    }
    if drivers.is_empty() {
        return Err(anyhow!(
            "no systems selected; pass --systems trusty (and optionally mempalace, kuzu)"
        ));
    }
    Ok(drivers)
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

    let drivers = build_drivers(&opts).await?;
    let mut all_metrics: Vec<SystemMetrics> = Vec::with_capacity(drivers.len());

    for driver in drivers.iter() {
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

    print_table(&all_metrics, opts.top_k);
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
        assert_eq!(corpus.docs.len(), 20, "expected 20 documents");
        assert_eq!(corpus.queries.len(), 10, "expected 10 queries");
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
        assert!(
            m.recall_at_k >= 0.80,
            "Recall@5 = {} (expected >= 0.80). Per-query: {:?}",
            m.recall_at_k,
            per_query
        );
    }
}
