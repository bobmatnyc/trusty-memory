<script>
  import Sidebar from './lib/components/Sidebar.svelte';
  import Topbar from './lib/components/Topbar.svelte';
  import Dashboard from './lib/views/Dashboard.svelte';
  import Palaces from './lib/views/Palaces.svelte';
  import PalaceDetail from './lib/views/PalaceDetail.svelte';
  import Config from './lib/views/Config.svelte';
  import { getRoute } from './lib/router.svelte.js';
  import { refreshStatus, refreshPalaces, refreshConfig } from './lib/state.svelte.js';
  import { onMount } from 'svelte';

  let bootError = $state(null);

  onMount(async () => {
    try {
      await Promise.all([refreshStatus(), refreshPalaces(), refreshConfig()]);
    } catch (e) {
      bootError = e.message || String(e);
    }
  });

  let route = $derived(getRoute());

  let view = $derived.by(() => {
    const segs = route.segments;
    if (segs.length === 0) return { kind: 'dashboard' };
    if (segs[0] === 'palaces' && segs.length === 1) return { kind: 'palaces' };
    if (segs[0] === 'palaces' && segs.length >= 2) return { kind: 'palace-detail', id: segs[1] };
    if (segs[0] === 'config') return { kind: 'config' };
    return { kind: 'dashboard' };
  });
</script>

<div class="layout">
  <Sidebar />
  <div class="main">
    <Topbar />
    <div class="content">
      {#if bootError}
        <div class="card" style="border-color: var(--trusty-danger)">
          <div class="card-header" style="color: var(--trusty-danger)">Connection error</div>
          <div class="card-body">
            <p>{bootError}</p>
            <p class="text-muted text-sm">
              Make sure trusty-memory is running with <code>trusty-memory serve --http 127.0.0.1:3031</code>.
            </p>
          </div>
        </div>
      {:else if view.kind === 'dashboard'}
        <Dashboard />
      {:else if view.kind === 'palaces'}
        <Palaces />
      {:else if view.kind === 'palace-detail'}
        <PalaceDetail id={view.id} />
      {:else if view.kind === 'config'}
        <Config />
      {/if}
    </div>
  </div>
</div>

<style>
  .layout {
    display: flex;
    min-height: 100vh;
  }
  .main {
    flex: 1;
    display: flex;
    flex-direction: column;
    margin-left: var(--trusty-sidebar-width);
    min-width: 0;
  }
  .content {
    padding: var(--trusty-space-5) var(--trusty-space-6);
    flex: 1;
    min-width: 0;
  }
</style>
