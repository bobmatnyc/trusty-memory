//! Dreaming — background idle-time memory consolidation (NLP-only, no LLM).
//!
//! Why: Long-running palaces accumulate near-duplicate drawers, low-importance
//! noise, and stale closet indexes. Periodic consolidation during idle windows
//! keeps retrieval fast and the L1 cache focused on what matters — without
//! ever calling an LLM.
//! What: `DreamConfig` (tunables), `DreamStats` (per-cycle telemetry), and
//! `Dreamer` (idle clock + `dream_cycle` doing dedup, prune, and closet
//! refresh).
//! Test: `cargo test -p trusty-memory-core dream::tests::` exercises every
//! moving part — defaults, idle clock, merge, prune, closet refresh.

use crate::decay::DecayConfig;
use crate::embed::FastEmbedder;
use crate::palace::Drawer;
use crate::retrieval::{recall_deep, PalaceHandle};
use anyhow::{Context, Result};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Tunables for the dream loop.
///
/// Why: The defaults bias toward conservative consolidation (rare cycles, only
/// merge near-identical drawers, only prune truly forgotten ones).
/// What: Plain values, all overridable.
/// Test: `dream_config_defaults`.
#[derive(Debug, Clone)]
pub struct DreamConfig {
    /// Seconds of inactivity before a dream cycle is allowed to run.
    pub idle_secs: u64,
    /// Cosine similarity above which two drawers are treated as duplicates.
    pub dedup_threshold: f32,
    /// Effective importance below which old drawers are pruned.
    pub prune_importance: f32,
    /// Wall-clock budget for one dream cycle.
    pub max_cycle_ms: u64,
}

impl Default for DreamConfig {
    fn default() -> Self {
        Self {
            idle_secs: 300,
            dedup_threshold: 0.95,
            prune_importance: 0.05,
            max_cycle_ms: 5_000,
        }
    }
}

/// Per-cycle dream telemetry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DreamStats {
    pub merged: usize,
    pub pruned: usize,
    pub closets_updated: usize,
    pub duration_ms: u64,
}

/// Background memory consolidator.
///
/// Why: We need a small, testable unit that owns the idle clock and the
/// consolidation logic — separate from the daemon that schedules it.
/// What: `last_activity` is a unix-seconds atomic touched on every recall /
/// remember; `dream_cycle` runs synchronously and returns stats.
/// Test: `dreamer_touch_resets_idle` plus the cycle tests below.
pub struct Dreamer {
    pub config: DreamConfig,
    last_activity: Arc<AtomicU64>,
}

impl Dreamer {
    /// Build a new dreamer with the given config and `last_activity = now`.
    ///
    /// Why: A fresh palace shouldn't immediately dream — start the idle clock
    /// from "now" so the first cycle waits a full `idle_secs`.
    /// What: Captures `SystemTime::now()` as unix seconds.
    /// Test: `dreamer_touch_resets_idle`.
    pub fn new(config: DreamConfig) -> Self {
        Self {
            config,
            last_activity: Arc::new(AtomicU64::new(now_secs())),
        }
    }

    /// Record activity (call from recall / remember paths).
    pub fn touch(&self) {
        self.last_activity.store(now_secs(), Ordering::Relaxed);
    }

    /// Has the palace been idle longer than `idle_secs`?
    pub fn is_idle(&self) -> bool {
        let last = self.last_activity.load(Ordering::Relaxed);
        now_secs().saturating_sub(last) >= self.config.idle_secs
    }

    /// Spawn the background dream loop.
    ///
    /// Why: A long-lived daemon needs a per-palace task that wakes periodically,
    /// checks the idle clock, and runs one cycle when appropriate.
    /// What: Spawns a tokio task that sleeps `idle_secs`, calls `dream_cycle`
    /// when `is_idle`, and logs the resulting stats. Runs forever; cancel by
    /// dropping the daemon.
    /// Test: Behavioral coverage via direct `dream_cycle` calls; the loop
    /// itself is just a sleep + dispatch.
    pub fn start(self: Arc<Self>, handle: Arc<PalaceHandle>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let interval = Duration::from_secs(self.config.idle_secs.max(1));
            loop {
                tokio::time::sleep(interval).await;
                if !self.is_idle() {
                    continue;
                }
                match self.dream_cycle(&handle).await {
                    Ok(stats) => tracing::info!(
                        palace = %handle.id,
                        merged = stats.merged,
                        pruned = stats.pruned,
                        closets_updated = stats.closets_updated,
                        duration_ms = stats.duration_ms,
                        "dream cycle complete"
                    ),
                    Err(e) => tracing::warn!(palace = %handle.id, "dream cycle failed: {e:#}"),
                }
            }
        })
    }

    /// Spawn the background dream loop with a cooperative shutdown signal.
    ///
    /// Why: A long-running daemon needs to stop its background workers cleanly
    /// on SIGTERM / Ctrl-C; otherwise the process can block on shutdown waiting
    /// for an in-flight cycle, or worse, terminate mid-cycle and leave on-disk
    /// state inconsistent. A `tokio::sync::watch` channel is the cheapest way
    /// to fan out a single cancel signal to every spawned task.
    /// What: Spawns a tokio task that races the inter-cycle sleep against the
    /// shutdown signal. When `shutdown` flips to `true`, the loop logs and
    /// exits cleanly. When the shutdown sender is dropped, the loop also
    /// exits (treated as a cancel).
    /// Test: `dreamer_shutdown_terminates_loop` — spawn the loop, flip the
    /// shutdown flag, await the join handle.
    pub fn start_with_shutdown(
        self: Arc<Self>,
        handle: Arc<PalaceHandle>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let interval = Duration::from_secs(self.config.idle_secs.max(1));
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    res = shutdown.changed() => {
                        // Sender closed (`Err`) or value changed to true: shut down.
                        if res.is_err() || *shutdown.borrow() {
                            tracing::info!(palace = %handle.id, "dreamer shutting down");
                            return;
                        }
                    }
                }
                if *shutdown.borrow() {
                    tracing::info!(palace = %handle.id, "dreamer shutting down");
                    return;
                }
                if !self.is_idle() {
                    continue;
                }
                match self.dream_cycle(&handle).await {
                    Ok(stats) => tracing::info!(
                        palace = %handle.id,
                        merged = stats.merged,
                        pruned = stats.pruned,
                        closets_updated = stats.closets_updated,
                        duration_ms = stats.duration_ms,
                        "dream cycle complete"
                    ),
                    Err(e) => tracing::warn!(palace = %handle.id, "dream cycle failed: {e:#}"),
                }
            }
        })
    }

    /// Run one synchronous dream cycle: dedup, prune, closet refresh, flush.
    ///
    /// Why: Consolidation must happen as a single, bounded unit so we can
    /// schedule it conservatively and report telemetry to the operator.
    /// What:
    ///   1. Dedup near-duplicates by L3-searching each drawer; if the top
    ///      neighbor's score >= `dedup_threshold`, merge into the higher-
    ///      importance survivor and `forget` the loser.
    ///   2. Prune drawers whose effective importance falls below
    ///      `prune_importance` AND whose age exceeds 30 days.
    ///   3. Rebuild the closet index (keyword -> drawer ids) from current
    ///      drawer table contents.
    ///   4. Flush the L1 snapshot.
    ///
    /// Test: `dream_cycle_merges_duplicates`, `dream_cycle_prunes_low_importance`,
    /// `closet_refresh_builds_index`.
    pub async fn dream_cycle(&self, handle: &Arc<PalaceHandle>) -> Result<DreamStats> {
        let started = std::time::Instant::now();
        let budget = Duration::from_millis(self.config.max_cycle_ms);

        let merged = self
            .dedup_pass(handle, started, budget)
            .await
            .context("dream dedup pass")?;
        let pruned = self
            .prune_pass(handle, started, budget)
            .await
            .context("dream prune pass")?;
        let closets_updated = self.refresh_closets(handle);

        // Persist the trimmed L1 snapshot so a restart sees the consolidated state.
        if let Err(e) = handle.flush() {
            tracing::warn!("dream flush failed: {e:#}");
        }

        Ok(DreamStats {
            merged,
            pruned,
            closets_updated,
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// Find near-duplicates and merge survivors; returns the merge count.
    async fn dedup_pass(
        &self,
        handle: &Arc<PalaceHandle>,
        started: std::time::Instant,
        budget: Duration,
    ) -> Result<usize> {
        let snapshot: Vec<Drawer> = handle.drawers.read().clone();
        if snapshot.len() < 2 {
            return Ok(0);
        }

        // Embedder is heavy; only build it once we know there's work to do.
        let embedder = FastEmbedder::new()
            .await
            .context("init embedder for dream dedup")?;

        let mut merges: usize = 0;
        let mut already_removed: std::collections::HashSet<Uuid> = std::collections::HashSet::new();

        for drawer in snapshot.iter() {
            if started.elapsed() >= budget {
                break;
            }
            if already_removed.contains(&drawer.id) {
                continue;
            }
            // Top-3 keeps the dedup pass cheap; the first neighbor is `drawer`
            // itself (score ~1.0) so we look at index 1+.
            let hits = recall_deep(handle, &embedder, &drawer.content, 3).await?;
            for hit in hits.into_iter().skip(1) {
                if hit.drawer.id == drawer.id || already_removed.contains(&hit.drawer.id) {
                    continue;
                }
                // `hit.score = effective_importance * cosine`; renormalize by
                // dividing out the survivor's effective importance to recover
                // a similarity-only signal. If that's not possible (zero
                // importance) fall back to the raw score.
                let age = DecayConfig::age_days(hit.drawer.created_at);
                let boost = hit.drawer.accumulated_boost(&handle.decay_config);
                let eff =
                    handle
                        .decay_config
                        .effective_importance(hit.drawer.importance, age, boost);
                let similarity = if eff > 0.0 {
                    hit.score / eff
                } else {
                    hit.score
                };
                if similarity < self.config.dedup_threshold {
                    continue;
                }

                // Pick survivor (higher importance wins; ties keep `drawer`).
                let (survivor, loser) = if drawer.importance >= hit.drawer.importance {
                    (drawer.clone(), hit.drawer.clone())
                } else {
                    (hit.drawer.clone(), drawer.clone())
                };
                merge_into(handle, &survivor, &loser);
                let _ = handle.forget(loser.id).await;
                already_removed.insert(loser.id);
                merges += 1;
                // Only one merge per source to keep behavior predictable.
                break;
            }
        }
        Ok(merges)
    }

    /// Drop drawers whose effective importance is below `prune_importance`
    /// AND that are older than 30 days. Returns the prune count.
    async fn prune_pass(
        &self,
        handle: &Arc<PalaceHandle>,
        started: std::time::Instant,
        budget: Duration,
    ) -> Result<usize> {
        const MIN_AGE_DAYS: f32 = 30.0;
        let snapshot: Vec<Drawer> = handle.drawers.read().clone();
        let mut victims: Vec<Uuid> = Vec::new();

        for drawer in snapshot.iter() {
            if started.elapsed() >= budget {
                break;
            }
            let age = DecayConfig::age_days(drawer.created_at);
            let boost = drawer.accumulated_boost(&handle.decay_config);
            let eff = handle
                .decay_config
                .effective_importance(drawer.importance, age, boost);
            if eff < self.config.prune_importance && age > MIN_AGE_DAYS {
                victims.push(drawer.id);
            }
        }

        // The decay floor (`DecayConfig::floor`) clamps `effective_importance`
        // from below, so very-low-importance drawers may still surface as
        // `floor`. Treat the user's `prune_importance` as the *base* threshold
        // when the decay floor would otherwise mask the signal.
        if victims.is_empty() {
            for drawer in snapshot.iter() {
                let age = DecayConfig::age_days(drawer.created_at);
                if drawer.importance < self.config.prune_importance && age > MIN_AGE_DAYS {
                    victims.push(drawer.id);
                }
            }
        }

        let count = victims.len();
        for id in victims {
            let _ = handle.forget(id).await;
        }
        Ok(count)
    }

    /// Rebuild closets: simple whitespace tokenization, stop-word filter,
    /// keyword -> drawer ids. Returns the number of keywords indexed.
    fn refresh_closets(&self, handle: &Arc<PalaceHandle>) -> usize {
        let snapshot: Vec<Drawer> = handle.drawers.read().clone();
        let mut new_index: HashMap<String, Vec<Uuid>> = HashMap::new();
        for drawer in snapshot.iter() {
            for kw in extract_keywords(&drawer.content) {
                new_index.entry(kw).or_default().push(drawer.id);
            }
        }
        let count = new_index.len();
        let mut closets = handle.closets.write();
        *closets = new_index;
        count
    }
}

/// Merge `loser` content into `survivor` (in-memory drawer table only).
///
/// Why: Dreaming consolidates duplicates without losing information; we
/// concatenate the loser's content into the survivor (capped) and union tags.
/// What: Updates the in-memory drawer entry for `survivor.id`. The vector
/// store entry remains keyed to the survivor; the loser's vector is removed
/// by the caller via `handle.forget`.
fn merge_into(handle: &Arc<PalaceHandle>, survivor: &Drawer, loser: &Drawer) {
    let mut drawers = handle.drawers.write();
    if let Some(target) = drawers.iter_mut().find(|d| d.id == survivor.id) {
        let mut combined = target.content.clone();
        combined.push_str("\n\nAlso: ");
        combined.push_str(&loser.content);
        if combined.len() > 500 {
            combined.truncate(500);
        }
        target.content = combined;
        target.importance = target.importance.max(loser.importance);
        for tag in &loser.tags {
            if !target.tags.contains(tag) {
                target.tags.push(tag.clone());
            }
        }
    }
}

/// Stop-word filter for closet keyword extraction.
const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "of", "in", "on", "at",
    "to", "for", "with", "and", "or", "but", "not", "no", "yes", "i", "you", "he", "she", "it",
    "we", "they", "this", "that", "these", "those", "as", "by", "from", "into", "over", "under",
    "if", "then", "than", "so", "do", "does", "did", "have", "has", "had", "will", "would",
    "shall", "should", "can", "could", "may", "might", "must", "about", "any", "all", "some",
    "more", "most", "such",
];

/// Extract keyword tokens from a drawer's content.
///
/// Why: Closets are a lightweight pre-computed index; we want stable, deduped
/// keyword tokens so the dream cycle's index is reproducible.
/// What: Lowercases, strips non-alphanumeric chars, drops stop-words and
/// tokens shorter than 3 chars, and dedups within a single drawer.
/// Test: Indirectly via `closet_refresh_builds_index`.
pub fn extract_keywords(content: &str) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for raw in content.split_whitespace() {
        let token: String = raw
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect();
        if token.len() < 3 {
            continue;
        }
        if STOP_WORDS.iter().any(|s| *s == token) {
            continue;
        }
        if seen.insert(token.clone()) {
            out.push(token);
        }
    }
    out
}

/// Current unix timestamp in seconds. Saturates to 0 on clock errors.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// Quiet a dead-code warning for the legacy import re-export when the type is
// only used through `Arc<PalaceHandle>` in this module.
#[allow(dead_code)]
type _PalaceHandleRef = RwLock<()>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palace::{Palace, PalaceId, RoomType};
    use crate::retrieval::PalaceHandle;
    use chrono::{Duration as ChronoDuration, Utc};
    use tempfile::tempdir;

    /// Why: Lock the default config values so accidental changes are caught.
    #[test]
    fn dream_config_defaults() {
        let cfg = DreamConfig::default();
        assert_eq!(cfg.idle_secs, 300);
        assert!((cfg.dedup_threshold - 0.95).abs() < 1e-6);
        assert!((cfg.prune_importance - 0.05).abs() < 1e-6);
        assert_eq!(cfg.max_cycle_ms, 5_000);
    }

    /// Why: `touch` must reset the idle clock; with `idle_secs=0` `is_idle`
    /// flips to `true` immediately, and `touch` must NOT make it stay false
    /// for >= idle_secs of zero. We use idle_secs=2 and assert the transition.
    #[test]
    fn dreamer_touch_resets_idle() {
        let dreamer = Dreamer::new(DreamConfig {
            idle_secs: 2,
            ..DreamConfig::default()
        });
        // Just-constructed: last_activity = now, so idle_secs has not elapsed.
        assert!(!dreamer.is_idle(), "fresh dreamer should not be idle yet");

        // Force the idle clock far into the past.
        dreamer
            .last_activity
            .store(now_secs().saturating_sub(10), Ordering::Relaxed);
        assert!(dreamer.is_idle(), "should be idle after 10s simulated wait");

        // Touch resets it.
        dreamer.touch();
        assert!(!dreamer.is_idle(), "touch should reset idle clock");
    }

    async fn open_test_handle(name: &str) -> Arc<PalaceHandle> {
        let dir = tempdir().unwrap();
        let palace = Palace {
            id: PalaceId::new(name),
            name: name.into(),
            description: None,
            created_at: Utc::now(),
            data_dir: dir.path().join(name),
        };
        std::fs::create_dir_all(&palace.data_dir).unwrap();
        let handle = PalaceHandle::open(&palace).unwrap();
        // Keep the tempdir alive by leaking it for the duration of the test —
        // tests are short and tempdir cleanup at process exit is fine.
        std::mem::forget(dir);
        handle
    }

    /// Why: Two near-identical drawers should collapse to one after a dream
    /// cycle so the L1 cache isn't filled with duplicates.
    /// What: Insert two drawers with the same content (verbatim — embeddings
    /// will land identically), run a dream cycle with default config, and
    /// assert the count drops from 2 to 1.
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_cycle_merges_duplicates() {
        let handle = open_test_handle("dream-merge").await;
        handle
            .remember(
                "Rust uses HNSW for vector search".into(),
                RoomType::Backend,
                vec!["rust".into()],
                0.7,
            )
            .await
            .unwrap();
        handle
            .remember(
                "Rust uses HNSW for vector search".into(),
                RoomType::Backend,
                vec!["rust".into()],
                0.6,
            )
            .await
            .unwrap();
        assert_eq!(handle.drawers.read().len(), 2);

        let dreamer = Dreamer::new(DreamConfig::default());
        let stats = dreamer.dream_cycle(&handle).await.unwrap();

        assert_eq!(stats.merged, 1, "expected exactly one merge");
        assert_eq!(handle.drawers.read().len(), 1, "expected dedup to 1 drawer");
    }

    /// Why: Old, low-importance drawers must be pruned so storage doesn't
    /// grow without bound.
    /// What: Insert one drawer with importance=0.01 and back-date its
    /// `created_at` to 60 days ago (older than the 30-day prune floor); run
    /// dream_cycle and assert it's gone.
    /// Test: This test itself.
    #[tokio::test]
    async fn dream_cycle_prunes_low_importance() {
        let handle = open_test_handle("dream-prune").await;
        handle
            .remember(
                "very stale fact nobody cares about".into(),
                RoomType::General,
                vec![],
                0.01,
            )
            .await
            .unwrap();
        // Back-date this drawer to satisfy the >30 days requirement.
        {
            let mut drawers = handle.drawers.write();
            for d in drawers.iter_mut() {
                d.created_at = Utc::now() - ChronoDuration::days(60);
            }
        }
        assert_eq!(handle.drawers.read().len(), 1);

        let dreamer = Dreamer::new(DreamConfig::default());
        let stats = dreamer.dream_cycle(&handle).await.unwrap();

        assert_eq!(stats.pruned, 1, "expected exactly one prune");
        assert!(
            handle.drawers.read().is_empty(),
            "low-importance aged drawer should be removed"
        );
    }

    /// Why: The serve daemon must be able to terminate the dream loop on
    /// SIGTERM/Ctrl-C; verify the watch-channel shutdown path actually causes
    /// the spawned task to exit instead of looping forever.
    /// What: Spawn `start_with_shutdown` with `idle_secs=10` (so it would
    /// otherwise sleep), flip the shutdown flag, and assert the join handle
    /// completes within a short bounded timeout.
    /// Test: This test itself.
    #[tokio::test]
    async fn dreamer_shutdown_terminates_loop() {
        let handle = open_test_handle("dream-shutdown").await;
        let dreamer = Arc::new(Dreamer::new(DreamConfig {
            idle_secs: 10,
            ..DreamConfig::default()
        }));
        let (tx, rx) = tokio::sync::watch::channel(false);
        let join = dreamer.clone().start_with_shutdown(handle, rx);

        // Yield once so the task is scheduled.
        tokio::task::yield_now().await;
        tx.send(true).expect("send shutdown signal");

        // The task should exit promptly — bound the wait to keep the test fast.
        let outcome = tokio::time::timeout(Duration::from_secs(2), join).await;
        assert!(
            outcome.is_ok(),
            "dream loop did not exit within 2s of shutdown"
        );
        outcome.unwrap().expect("join handle clean exit");
    }

    /// Why: After a dream cycle, the closet index should map keywords from
    /// drawer content back to that drawer's id so L2 can use it as a cheap
    /// pre-filter.
    /// What: Insert a drawer with a distinctive keyword, run the cycle, and
    /// assert the closets map contains that keyword pointing to the drawer.
    /// Test: This test itself.
    #[tokio::test]
    async fn closet_refresh_builds_index() {
        let handle = open_test_handle("dream-closets").await;
        let id = handle
            .remember(
                "Quokkas are the happiest marsupials in Australia".into(),
                RoomType::General,
                vec![],
                0.5,
            )
            .await
            .unwrap();

        let dreamer = Dreamer::new(DreamConfig::default());
        let stats = dreamer.dream_cycle(&handle).await.unwrap();
        assert!(
            stats.closets_updated > 0,
            "closet index should be non-empty"
        );

        let closets = handle.closets.read();
        let entry = closets.get("quokkas").expect("expected `quokkas` keyword");
        assert!(
            entry.contains(&id),
            "closet entry must reference the source drawer"
        );
    }
}
