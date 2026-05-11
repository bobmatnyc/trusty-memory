<script>
  /*
   * Why: The dashboard is the operator's at-a-glance machine-health view —
   * it must surface palace, drawer, vector, and KG counts; expose the dream
   * cycle's last-run telemetry; and let the operator force a run.
   * What: Four top stat cards, a Dream Cycle panel with run-now button, an
   * enhanced palaces table, and a system-info card. All data flows through
   * the centralized `state.svelte.js` store.
   * Test: Run `pnpm dev` from `ui/`, open http://127.0.0.1:3031, confirm
   * counters render, click "Run now" and watch the panel refresh.
   */
  import {
    getStatus,
    getPalaces,
    getConfig,
    getDreamStatus,
    refreshStatus,
    refreshPalaces,
    refreshDreamStatus,
    runDream
  } from '../state.svelte.js';
  import { navigate } from '../router.svelte.js';

  let status = $derived(getStatus());
  let palaces = $derived(getPalaces());
  let config = $derived(getConfig());
  let dream = $derived(getDreamStatus());

  let totalDrawers = $derived(
    status?.total_drawers ??
      palaces.reduce((sum, p) => sum + (p.drawer_count || 0), 0)
  );
  let totalVectors = $derived(
    status?.total_vectors ??
      palaces.reduce((sum, p) => sum + (p.vector_count || 0), 0)
  );
  let totalKg = $derived(
    status?.total_kg_triples ??
      palaces.reduce((sum, p) => sum + (p.kg_triple_count || 0), 0)
  );

  let recent = $derived(
    [...palaces]
      .sort((a, b) => new Date(b.created_at) - new Date(a.created_at))
      .slice(0, 10)
  );

  /**
   * Why: Operators need a quick "how stale is this telemetry?" signal — raw
   * ISO timestamps make that hard at a glance.
   * What: Returns a human-readable "X seconds/minutes/hours/days ago" string.
   * Test: Pass a date 2 minutes in the past, expect "2 minutes ago".
   */
  function timeAgo(iso) {
    if (!iso) return 'Never';
    const then = new Date(iso).getTime();
    const diff = Date.now() - then;
    if (Number.isNaN(diff)) return 'Unknown';
    const s = Math.max(0, Math.floor(diff / 1000));
    if (s < 60) return `${s}s ago`;
    const m = Math.floor(s / 60);
    if (m < 60) return `${m}m ago`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}h ago`;
    const d = Math.floor(h / 24);
    return `${d}d ago`;
  }

  let dreamStale = $derived.by(() => {
    if (!dream?.last_run_at) return false;
    const ageMs = Date.now() - new Date(dream.last_run_at).getTime();
    return ageMs > 24 * 60 * 60 * 1000;
  });

  let running = $state(false);
  let runError = $state(null);

  async function onRunDream() {
    runError = null;
    running = true;
    try {
      await runDream();
      // The dream cycle changes drawer/vector totals via dedup + compact —
      // refresh palaces and status so the top stats stay honest.
      await Promise.all([refreshDreamStatus(), refreshStatus(), refreshPalaces()]);
    } catch (e) {
      runError = e.message || String(e);
    } finally {
      running = false;
    }
  }
</script>

<h1 class="page-title">Dashboard</h1>

<div class="stat-grid">
  <div class="stat">
    <div class="stat-label">Palaces</div>
    <div class="stat-value">{palaces.length}</div>
    <div class="stat-meta">namespaces</div>
  </div>
  <div class="stat">
    <div class="stat-label">Drawers</div>
    <div class="stat-value">{totalDrawers}</div>
    <div class="stat-meta">stored memories</div>
  </div>
  <div class="stat">
    <div class="stat-label">Vectors</div>
    <div class="stat-value">{totalVectors}</div>
    <div class="stat-meta">HNSW index</div>
  </div>
  <div class="stat">
    <div class="stat-label">KG Triples</div>
    <div class="stat-value">{totalKg}</div>
    <div class="stat-meta">knowledge graph</div>
  </div>
</div>

<div class="grid-2">
  <div class="card">
    <div class="card-header flex-between">
      <span>Dream Cycle</span>
      <button
        class="btn btn-sm btn-primary"
        onclick={onRunDream}
        disabled={running}
      >
        {running ? 'Running…' : 'Run now'}
      </button>
    </div>
    <div class="card-body">
      <div class="dream-row">
        <span class="dream-label">Last run</span>
        <span class="dream-value">
          {timeAgo(dream?.last_run_at)}
          {#if dreamStale}
            <span class="badge badge-warning" style="margin-left: 8px">stale</span>
          {/if}
        </span>
      </div>
      <div class="dream-grid">
        <div class="mini-stat">
          <div class="mini-label">Merged</div>
          <div class="mini-value">{dream?.merged ?? 0}</div>
        </div>
        <div class="mini-stat">
          <div class="mini-label">Pruned</div>
          <div class="mini-value">{dream?.pruned ?? 0}</div>
        </div>
        <div class="mini-stat">
          <div class="mini-label">Compacted</div>
          <div class="mini-value">{dream?.compacted ?? 0}</div>
        </div>
        <div class="mini-stat">
          <div class="mini-label">Closets</div>
          <div class="mini-value">{dream?.closets_updated ?? 0}</div>
        </div>
        <div class="mini-stat">
          <div class="mini-label">Duration</div>
          <div class="mini-value">{dream?.duration_ms ?? 0}<span class="unit">ms</span></div>
        </div>
      </div>
      {#if dreamStale}
        <p class="text-muted text-sm mt-3">
          Dream cycle hasn't run in over 24 hours — the background loop may be paused.
        </p>
      {/if}
      {#if runError}
        <p class="text-sm mt-3" style="color: var(--trusty-danger)">{runError}</p>
      {/if}
    </div>
  </div>

  <div class="card">
    <div class="card-header">System</div>
    <div class="card-body">
      <div class="sys-row">
        <span class="sys-label">Version</span>
        <span class="text-mono">{status?.version ?? '—'}</span>
      </div>
      <div class="sys-row">
        <span class="sys-label">Default palace</span>
        <span class="text-mono">{status?.default_palace ?? '—'}</span>
      </div>
      <div class="sys-row">
        <span class="sys-label">Data root</span>
        <span class="text-mono text-xs text-muted truncate" style="max-width: 60%">
          {status?.data_root ?? '—'}
        </span>
      </div>
      <div class="sys-row">
        <span class="sys-label">OpenRouter</span>
        {#if config?.openrouter_configured}
          <span class="badge badge-success">ready</span>
        {:else}
          <span class="badge badge-muted">not set</span>
        {/if}
      </div>
      <div class="sys-row">
        <span class="sys-label">Daemon</span>
        <span class="badge badge-success">{status ? 'running' : '—'}</span>
      </div>
    </div>
  </div>
</div>

<div class="card mt-4">
  <div class="card-header flex-between">
    <span>Palaces</span>
    <button class="btn btn-sm btn-primary" onclick={() => navigate('/palaces')}>
      Manage all
    </button>
  </div>
  <div class="card-body" style="padding: 0">
    {#if recent.length === 0}
      <div class="empty">
        No palaces yet.
        <a
          href="#/palaces"
          onclick={(e) => {
            e.preventDefault();
            navigate('/palaces');
          }}
        >Create one</a>.
      </div>
    {:else}
      <table class="table">
        <thead>
          <tr>
            <th>Name</th>
            <th>ID</th>
            <th>Drawers</th>
            <th>Vectors</th>
            <th>KG</th>
            <th>Wings</th>
            <th>Created</th>
          </tr>
        </thead>
        <tbody>
          {#each recent as p}
            <tr style="cursor: pointer" onclick={() => navigate(`/palaces/${p.id}`)}>
              <td><strong>{p.name}</strong></td>
              <td class="text-mono text-xs text-muted">{p.id}</td>
              <td>{p.drawer_count ?? 0}</td>
              <td>{p.vector_count ?? 0}</td>
              <td>{p.kg_triple_count ?? 0}</td>
              <td>{p.wing_count ?? 0}</td>
              <td class="text-muted text-sm">{new Date(p.created_at).toLocaleString()}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </div>
</div>

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0 0 var(--trusty-space-5) 0;
    font-weight: 600;
  }
  .grid-2 {
    display: grid;
    grid-template-columns: 2fr 1fr;
    gap: var(--trusty-space-4);
  }
  @media (max-width: 900px) {
    .grid-2 {
      grid-template-columns: 1fr;
    }
  }
  .dream-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding-bottom: var(--trusty-space-3);
    border-bottom: 1px solid var(--trusty-border);
    margin-bottom: var(--trusty-space-4);
  }
  .dream-label {
    font-size: var(--trusty-fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--trusty-text-muted);
  }
  .dream-value {
    font-weight: 600;
    font-size: var(--trusty-fs-md);
  }
  .dream-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(110px, 1fr));
    gap: var(--trusty-space-3);
  }
  .mini-stat {
    padding: var(--trusty-space-3);
    background: var(--trusty-content-bg);
    border-radius: var(--trusty-radius);
  }
  .mini-label {
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    margin-bottom: var(--trusty-space-1);
  }
  .mini-value {
    font-size: var(--trusty-fs-lg);
    font-weight: 700;
  }
  .unit {
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-muted);
    margin-left: 2px;
  }
  .sys-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--trusty-space-2) 0;
    border-bottom: 1px solid var(--trusty-border);
  }
  .sys-row:last-child {
    border-bottom: none;
  }
  .sys-label {
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-muted);
  }
</style>
