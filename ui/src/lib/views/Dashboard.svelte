<script>
  import { getStatus, getPalaces, getConfig } from '../state.svelte.js';
  import { navigate } from '../router.svelte.js';

  let status = $derived(getStatus());
  let palaces = $derived(getPalaces());
  let config = $derived(getConfig());

  let totalDrawers = $derived(palaces.reduce((sum, p) => sum + (p.drawer_count || 0), 0));
  let recent = $derived(
    [...palaces]
      .sort((a, b) => new Date(b.created_at) - new Date(a.created_at))
      .slice(0, 5)
  );
</script>

<h1 class="page-title">Dashboard</h1>

<div class="stat-grid">
  <div class="stat">
    <div class="stat-label">Palaces</div>
    <div class="stat-value">{palaces.length}</div>
    <div class="stat-meta">total namespaces</div>
  </div>
  <div class="stat">
    <div class="stat-label">Drawers</div>
    <div class="stat-value">{totalDrawers}</div>
    <div class="stat-meta">across all palaces</div>
  </div>
  <div class="stat">
    <div class="stat-label">Daemon</div>
    <div class="stat-value" style="color: var(--trusty-success)">
      {status ? 'Running' : '—'}
    </div>
    <div class="stat-meta">{status ? `v${status.version}` : 'not connected'}</div>
  </div>
  <div class="stat">
    <div class="stat-label">Default Palace</div>
    <div class="stat-value" style="font-size: 1.25rem">
      {status?.default_palace || '—'}
    </div>
    <div class="stat-meta">{config?.openrouter_configured ? 'OpenRouter ready' : 'OpenRouter not set'}</div>
  </div>
</div>

<div class="card">
  <div class="card-header flex-between">
    <span>Recent palaces</span>
    <button class="btn btn-sm btn-primary" onclick={() => navigate('/palaces')}>
      Manage all
    </button>
  </div>
  <div class="card-body" style="padding: 0">
    {#if recent.length === 0}
      <div class="empty">
        No palaces yet. <a href="#/palaces" onclick={(e) => { e.preventDefault(); navigate('/palaces'); }}>Create one</a>.
      </div>
    {:else}
      <table class="table">
        <thead>
          <tr>
            <th>Name</th>
            <th>ID</th>
            <th>Drawers</th>
            <th>Created</th>
          </tr>
        </thead>
        <tbody>
          {#each recent as p}
            <tr style="cursor: pointer" onclick={() => navigate(`/palaces/${p.id}`)}>
              <td><strong>{p.name}</strong></td>
              <td class="text-mono text-xs text-muted">{p.id}</td>
              <td>{p.drawer_count}</td>
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
</style>
