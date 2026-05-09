//! Temporal knowledge graph backed by SQLite (WAL mode).
//!
//! Why: Some facts are relational and time-bounded ("Alice worked at Acme from
//! 2020 to 2023"). Vector search alone can't represent that; a triple store with
//! `valid_from`/`valid_to` columns can.
//! What: `Triple` record + `KnowledgeGraph` (rusqlite + r2d2 pool) stub.
//! Test: After implementation, asserting (s,p,o) twice should close the first
//! interval and open a new one; `query_active` returns only the latest.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Triple {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub valid_from: DateTime<Utc>,
    pub valid_to: Option<DateTime<Utc>>,
    /// Confidence in [0.0, 1.0] from the asserter.
    pub confidence: f32,
    /// Free-form provenance string (drawer id, source URL, agent name, ...).
    pub provenance: Option<String>,
}

/// Schema (created on `open` if missing):
///
/// ```sql
/// CREATE TABLE IF NOT EXISTS entities (
///     id          TEXT PRIMARY KEY,
///     name        TEXT NOT NULL,
///     entity_type TEXT NOT NULL,
///     properties  TEXT  -- JSON
/// );
///
/// CREATE TABLE IF NOT EXISTS triples (
///     id          INTEGER PRIMARY KEY AUTOINCREMENT,
///     subject     TEXT NOT NULL,
///     predicate   TEXT NOT NULL,
///     object      TEXT NOT NULL,
///     valid_from  TEXT NOT NULL,  -- ISO-8601
///     valid_to    TEXT,           -- NULL = currently active
///     confidence  REAL NOT NULL,
///     provenance  TEXT
/// );
///
/// CREATE INDEX IF NOT EXISTS idx_triples_subj_active
///     ON triples(subject) WHERE valid_to IS NULL;
/// ```
pub struct KnowledgeGraph;

impl KnowledgeGraph {
    /// Open (or create) a SQLite database at `path` in WAL mode.
    ///
    /// Why: WAL mode allows concurrent readers + a single writer, matching our
    /// many-readers / few-writers workload.
    /// What: Stub — implementation will set `journal_mode=WAL`, run migrations.
    /// Test: `open(temp).await` then a second `open` on same path must succeed.
    pub fn open(_path: &Path) -> Result<Self> {
        Ok(Self)
    }

    /// Assert a fact. If a contradicting active triple exists, set its
    /// `valid_to` and insert this one as the new active fact.
    pub async fn assert(&self, _triple: Triple) -> Result<()> {
        Ok(())
    }

    /// Return all currently active triples (`valid_to IS NULL`) for a subject.
    pub async fn query_active(&self, _subject: &str) -> Result<Vec<Triple>> {
        Ok(Vec::new())
    }
}
