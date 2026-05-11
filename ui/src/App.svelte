<script>
  import Sidebar from './lib/components/Sidebar.svelte';
  import Topbar from './lib/components/Topbar.svelte';
  import Dashboard from './lib/views/Dashboard.svelte';
  import Palaces from './lib/views/Palaces.svelte';
  import PalaceDetail from './lib/views/PalaceDetail.svelte';
  import Config from './lib/views/Config.svelte';
  import Chat from './lib/views/Chat.svelte';
  import { api } from './lib/api.js';
  import { getRoute } from './lib/router.svelte.js';
  import {
    refreshStatus,
    refreshPalaces,
    refreshConfig,
    refreshDreamStatus
  } from './lib/state.svelte.js';
  import { onMount } from 'svelte';

  let bootError = $state(null);
  let hasChat = $state(false);

  onMount(async () => {
    try {
      await Promise.all([
        refreshStatus(),
        refreshPalaces(),
        refreshConfig(),
        refreshDreamStatus()
      ]);
    } catch (e) {
      bootError = e.message || String(e);
    }
    // Non-fatal: chat is optional, depending on configured providers.
    try {
      const provResp = await api.chatProviders();
      hasChat = !!provResp?.active;
    } catch {
      hasChat = false;
    }
  });

  let route = $derived(getRoute());

  let view = $derived.by(() => {
    const segs = route.segments;
    if (segs.length === 0) return { kind: 'dashboard' };
    if (segs[0] === 'palaces' && segs.length === 1) return { kind: 'palaces' };
    if (segs[0] === 'palaces' && segs.length >= 2) return { kind: 'palace-detail', id: segs[1] };
    if (segs[0] === 'config') return { kind: 'config' };
    if (segs[0] === 'chat') return { kind: 'chat' };
    return { kind: 'dashboard' };
  });
</script>

<div class="layout">
  <Sidebar {hasChat} />
  <div class="main">
    <Topbar />
    <div class="content">
      {#if bootError}
        <div class="card" style="border-color: var(--trusty-danger)">
          <div class="card-header" style="color: var(--trusty-danger)">Connection error</div>
          <div class="card-body">
            <p>{bootError}</p>
            <p class="text-muted text-sm">
              Make sure trusty-memory is running with <code>--http</code> enabled.
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
      {:else if view.kind === 'chat'}
        <Chat />
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
