<script>
  import {
    getActivePalace,
    setActivePalace,
    getStatus,
    getPalaces
  } from '../state.svelte.js';
  import { getRoute } from '../router.svelte.js';

  let active = $derived(getActivePalace());
  let status = $derived(getStatus());
  let palaces = $derived(getPalaces());
  let route = $derived(getRoute());

  let crumbs = $derived.by(() => {
    const segs = route.segments;
    if (segs.length === 0) return ['Dashboard'];
    const parts = [];
    if (segs[0] === 'palaces') {
      parts.push('Palaces');
      if (segs.length > 1) parts.push(segs[1]);
    } else if (segs[0] === 'config') {
      parts.push('Config');
    }
    return parts.length ? parts : ['Dashboard'];
  });
</script>

<header class="topbar">
  <div class="crumbs">
    {#each crumbs as crumb, i}
      {#if i > 0}<span class="sep">/</span>{/if}
      <span class="crumb">{crumb}</span>
    {/each}
  </div>
  <div class="actions">
    <select
      class="select palace-select"
      value={active}
      onchange={(e) => setActivePalace(e.currentTarget.value)}
    >
      <option value="">— select palace —</option>
      {#each palaces as p}
        <option value={p.id}>{p.name}</option>
      {/each}
    </select>
    {#if status}
      <span class="badge badge-success">v{status.version}</span>
    {:else}
      <span class="badge badge-muted">connecting…</span>
    {/if}
  </div>
</header>

<style>
  .topbar {
    height: var(--trusty-topbar-height);
    background: var(--trusty-card-bg);
    border-bottom: 1px solid var(--trusty-border);
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 var(--trusty-space-6);
    position: sticky;
    top: 0;
    z-index: 10;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-secondary);
  }
  .crumb {
    font-weight: 500;
  }
  .crumb:last-child {
    color: var(--trusty-text-primary);
    font-weight: 600;
  }
  .sep {
    color: var(--trusty-text-muted);
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .palace-select {
    width: auto;
    min-width: 180px;
    padding: 6px 10px;
    font-size: var(--trusty-fs-sm);
  }
</style>
