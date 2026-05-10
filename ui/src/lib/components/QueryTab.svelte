<script>
  import { api } from '../api.js';

  let { palaceId } = $props();

  let query = $state('');
  let topK = $state(10);
  let deep = $state(false);
  let results = $state([]);
  let loading = $state(false);
  let error = $state(null);
  let elapsedMs = $state(null);

  async function run() {
    if (!query.trim()) return;
    loading = true;
    error = null;
    const t0 = performance.now();
    try {
      results = await api.recall(palaceId, { q: query.trim(), top_k: topK, deep });
      elapsedMs = Math.round(performance.now() - t0);
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  function layerLabel(l) {
    return ['L0 identity', 'L1 essential', 'L2 recall', 'L3 deep'][l] || `L${l}`;
  }
  function layerClass(l) {
    return [
      'badge-info',
      'badge-success',
      'badge',
      'badge-warning'
    ][l] || 'badge';
  }
</script>

<div class="card mb-4">
  <div class="card-body">
    <div class="form-group">
      <label class="form-label" for="q">Query</label>
      <input
        id="q"
        class="input"
        placeholder="What does this palace know about…"
        bind:value={query}
        onkeydown={(e) => e.key === 'Enter' && run()}
      />
    </div>
    <div class="row">
      <div class="form-group">
        <label class="form-label" for="topk">Top K: {topK}</label>
        <input id="topk" type="range" min="1" max="50" bind:value={topK} />
      </div>
      <div class="form-group">
        <label class="form-label">Mode</label>
        <div class="flex-gap-2">
          <button
            class="btn"
            class:btn-primary={!deep}
            onclick={() => (deep = false)}
          >L2 Recall</button>
          <button
            class="btn"
            class:btn-primary={deep}
            onclick={() => (deep = true)}
          >L3 Deep</button>
        </div>
      </div>
      <div class="form-group" style="display: flex; align-items: flex-end">
        <button class="btn btn-primary" disabled={loading || !query.trim()} onclick={run}>
          {loading ? 'Searching…' : 'Search'}
        </button>
      </div>
    </div>
    {#if error}<div class="badge badge-danger">{error}</div>{/if}
  </div>
</div>

{#if results.length > 0}
  <div class="text-muted text-sm mb-3">
    {results.length} results in {elapsedMs}ms
  </div>
  <div class="results">
    {#each results as r}
      <div class="card mb-3">
        <div class="card-body">
          <div class="flex-between mb-3">
            <div class="flex-gap-2">
              <span class="badge {layerClass(r.layer)}">{layerLabel(r.layer)}</span>
              <span class="badge badge-muted">score {r.score.toFixed(3)}</span>
            </div>
            <span class="text-xs text-muted">{new Date(r.drawer.created_at).toLocaleString()}</span>
          </div>
          <div style="white-space: pre-wrap">{r.drawer.content}</div>
          {#if r.drawer.tags?.length}
            <div class="mt-3">
              {#each r.drawer.tags as t}<span class="tag">{t}</span>{/each}
            </div>
          {/if}
        </div>
      </div>
    {/each}
  </div>
{:else if !loading && query}
  <div class="empty">No results</div>
{/if}

<style>
  .row {
    display: grid;
    grid-template-columns: 1fr 1fr auto;
    gap: var(--trusty-space-3);
    align-items: end;
  }
</style>
