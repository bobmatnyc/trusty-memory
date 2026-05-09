//! Temporal knowledge graph backed by SQLite (WAL mode).
//!
//! Why: Some facts are relational and time-bounded ("Alice worked at Acme from
//! 2020 to 2023"). Vector search alone can't represent that; a triple store with
//! `valid_from`/`valid_to` columns can.
//! What: `Triple` record + `KnowledgeGraph` (rusqlite + r2d2 pool) implementation.
//! Test: Asserting (s,p,o) twice should close the first interval and open a
//! new one; `query_active` returns only the latest.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
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
pub struct KnowledgeGraph {
    pool: Pool<SqliteConnectionManager>,
}

impl KnowledgeGraph {
    /// Open (or create) a SQLite database at `path` in WAL mode.
    ///
    /// Why: WAL mode allows concurrent readers + a single writer, matching our
    /// many-readers / few-writers workload.
    /// What: Builds an r2d2 pool, sets `journal_mode=WAL`, runs migrations.
    /// Test: `open(temp)` succeeds and creates schema; second `open` on same
    /// path also succeeds (idempotent migrations).
    pub fn open(path: &Path) -> Result<Self> {
        let manager = SqliteConnectionManager::file(path);
        let pool = Pool::builder()
            .max_size(8)
            .build(manager)
            .context("failed to build sqlite connection pool")?;

        let conn = pool.get().context("failed to get sqlite connection")?;

        // Enable WAL mode. `pragma_update` doesn't return rows, so use query_row.
        conn.query_row("PRAGMA journal_mode=WAL", [], |row| {
            row.get::<_, String>(0)
        })
        .context("failed to enable WAL journal mode")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS entities (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                entity_type TEXT NOT NULL,
                properties  TEXT
            );

            CREATE TABLE IF NOT EXISTS triples (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                subject     TEXT NOT NULL,
                predicate   TEXT NOT NULL,
                object      TEXT NOT NULL,
                valid_from  TEXT NOT NULL,
                valid_to    TEXT,
                confidence  REAL NOT NULL DEFAULT 1.0,
                provenance  TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_triples_subj_active
                ON triples(subject) WHERE valid_to IS NULL;",
        )
        .context("failed to run schema migrations")?;

        Ok(Self { pool })
    }

    /// Assert a fact. If a contradicting active triple exists (same
    /// subject+predicate, `valid_to IS NULL`), close it by setting `valid_to`
    /// to this triple's `valid_from`, then insert this one as the new active
    /// fact.
    ///
    /// Why: Temporal model — facts have intervals. New assertion supersedes
    /// the prior active row instead of overwriting it, preserving history.
    /// What: Runs UPDATE-then-INSERT inside a single connection on a blocking
    /// task so the async reactor isn't blocked by sqlite I/O.
    /// Test: After two asserts of differing objects on same (s,p),
    /// `query_active` returns exactly one row with the latest object.
    pub async fn assert(&self, triple: Triple) -> Result<()> {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = pool.get().context("failed to get sqlite connection")?;
            let close_ts = triple.valid_from.to_rfc3339();

            conn.execute(
                "UPDATE triples
                    SET valid_to = ?1
                    WHERE subject = ?2 AND predicate = ?3 AND valid_to IS NULL",
                rusqlite::params![close_ts, triple.subject, triple.predicate],
            )
            .context("failed to close prior active interval")?;

            conn.execute(
                "INSERT INTO triples
                    (subject, predicate, object, valid_from, confidence, provenance)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    triple.subject,
                    triple.predicate,
                    triple.object,
                    triple.valid_from.to_rfc3339(),
                    triple.confidence,
                    triple.provenance,
                ],
            )
            .context("failed to insert new active triple")?;

            Ok(())
        })
        .await
        .context("assert spawn_blocking join error")??;
        Ok(())
    }

    /// Return all currently active triples (`valid_to IS NULL`) for a subject.
    ///
    /// Why: Most queries want "what is true *now*" — filtering on
    /// `valid_to IS NULL` uses the partial index `idx_triples_subj_active`.
    /// What: SELECT and map rows to `Triple`, parsing RFC3339 datetimes.
    /// Test: After asserting one triple, `query_active(subject)` returns it;
    /// for unknown subjects returns empty vec.
    pub async fn query_active(&self, subject: &str) -> Result<Vec<Triple>> {
        let pool = self.pool.clone();
        let subject = subject.to_string();
        let triples = tokio::task::spawn_blocking(move || -> Result<Vec<Triple>> {
            let conn = pool.get().context("failed to get sqlite connection")?;
            let mut stmt = conn
                .prepare(
                    "SELECT subject, predicate, object, valid_from, valid_to,
                            confidence, provenance
                       FROM triples
                       WHERE subject = ?1 AND valid_to IS NULL",
                )
                .context("failed to prepare query_active statement")?;

            let rows = stmt
                .query_map(rusqlite::params![subject], |row| {
                    let valid_from: String = row.get(3)?;
                    let valid_to: Option<String> = row.get(4)?;
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        valid_from,
                        valid_to,
                        row.get::<_, f64>(5)? as f32,
                        row.get::<_, Option<String>>(6)?,
                    ))
                })
                .context("failed to query active triples")?;

            let mut out = Vec::new();
            for row in rows {
                let (subject, predicate, object, vf, vt, confidence, provenance) =
                    row.context("failed to read triple row")?;
                let valid_from = DateTime::parse_from_rfc3339(&vf)
                    .context("invalid valid_from datetime")?
                    .with_timezone(&Utc);
                let valid_to = match vt {
                    Some(s) => Some(
                        DateTime::parse_from_rfc3339(&s)
                            .context("invalid valid_to datetime")?
                            .with_timezone(&Utc),
                    ),
                    None => None,
                };
                out.push(Triple {
                    subject,
                    predicate,
                    object,
                    valid_from,
                    valid_to,
                    confidence,
                    provenance,
                });
            }
            Ok(out)
        })
        .await
        .context("query_active spawn_blocking join error")??;
        Ok(triples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn open_creates_schema() {
        let dir = tempdir().unwrap();
        let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
        // If open succeeds, schema was created
        let result = kg.query_active("nonexistent").await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn assert_then_query_active_returns_fact() {
        let dir = tempdir().unwrap();
        let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
        let triple = Triple {
            subject: "alice".to_string(),
            predicate: "works_at".to_string(),
            object: "Acme Corp".to_string(),
            valid_from: Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: None,
        };
        kg.assert(triple).await.unwrap();
        let active = kg.query_active("alice").await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].object, "Acme Corp");
    }

    #[tokio::test]
    async fn second_assert_closes_prior_interval() {
        let dir = tempdir().unwrap();
        let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
        let t1 = Triple {
            subject: "alice".to_string(),
            predicate: "works_at".to_string(),
            object: "Acme Corp".to_string(),
            valid_from: Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: None,
        };
        kg.assert(t1).await.unwrap();

        let t2 = Triple {
            subject: "alice".to_string(),
            predicate: "works_at".to_string(),
            object: "Beta Inc".to_string(),
            valid_from: Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: None,
        };
        kg.assert(t2).await.unwrap();

        let active = kg.query_active("alice").await.unwrap();
        assert_eq!(active.len(), 1, "should have exactly 1 active triple");
        assert_eq!(active[0].object, "Beta Inc");
    }

    #[tokio::test]
    async fn wal_mode_enabled() {
        let dir = tempdir().unwrap();
        let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
        // WAL mode creates a -wal file after first write
        let triple = Triple {
            subject: "s".to_string(),
            predicate: "p".to_string(),
            object: "o".to_string(),
            valid_from: Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: None,
        };
        kg.assert(triple).await.unwrap();
        assert!(
            dir.path().join("kg.db-wal").exists() || dir.path().join("kg.db-shm").exists(),
            "WAL mode should create -wal or -shm sidecar files"
        );
    }
}
