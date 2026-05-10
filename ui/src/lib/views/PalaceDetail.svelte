<script>
  import DrawersTab from '../components/DrawersTab.svelte';
  import QueryTab from '../components/QueryTab.svelte';
  import KgTab from '../components/KgTab.svelte';
  import ChatTab from '../components/ChatTab.svelte';
  import { api } from '../api.js';
  import { setActivePalace, getConfig } from '../state.svelte.js';
  import { onMount } from 'svelte';

  let { id } = $props();

  let palace = $state(null);
  let error = $state(null);
  let tab = $state('drawers');

  let config = $derived(getConfig());
  let chatEnabled = $derived(!!config?.openrouter_configured);

  async function load() {
    try {
      palace = await api.getPalace(id);
      setActivePalace(id);
    } catch (e) {
      error = e.message;
    }
  }

  onMount(load);
  $effect(() => {
    // Re-load when id changes.
    id;
    load();
  });

  const tabs = [
    { key: 'drawers', label: 'Drawers' },
    { key: 'query', label: 'Query' },
    { key: 'kg', label: 'Knowledge Graph' },
    { key: 'chat', label: 'Chat' }
  ];
</script>

{#if error}
  <div class="card" style="border-color: var(--trusty-danger)">
    <div class="card-body">{error}</div>
  </div>
{:else if !palace}
  <div class="empty">Loading…</div>
{:else}
  <div class="header mb-4">
    <div>
      <h1 class="page-title">{palace.name}</h1>
      <div class="text-muted text-sm text-mono">{palace.id}</div>
      {#if palace.description}
        <div class="text-sm mt-3">{palace.description}</div>
      {/if}
    </div>
    <div class="flex-gap-2">
      <span class="badge">{palace.drawer_count} drawers</span>
      <span class="badge badge-muted">created {new Date(palace.created_at).toLocaleDateString()}</span>
    </div>
  </div>

  <div class="tabs mb-4">
    {#each tabs as t}
      {#if t.key !== 'chat' || chatEnabled}
        <button
          class="tab"
          class:active={tab === t.key}
          onclick={() => (tab = t.key)}
        >
          {t.label}
        </button>
      {/if}
    {/each}
  </div>

  {#if tab === 'drawers'}
    <DrawersTab palaceId={palace.id} onChanged={load} />
  {:else if tab === 'query'}
    <QueryTab palaceId={palace.id} />
  {:else if tab === 'kg'}
    <KgTab palaceId={palace.id} />
  {:else if tab === 'chat' && chatEnabled}
    <ChatTab palaceId={palace.id} />
  {/if}
{/if}

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0 0 var(--trusty-space-1) 0;
    font-weight: 600;
  }
  .header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: var(--trusty-space-4);
  }
  .tabs {
    display: flex;
    gap: 4px;
    border-bottom: 1px solid var(--trusty-border);
  }
  .tab {
    background: transparent;
    border: none;
    padding: 10px 16px;
    font-size: var(--trusty-fs-sm);
    font-weight: 500;
    color: var(--trusty-text-muted);
    border-bottom: 2px solid transparent;
    margin-bottom: -1px;
    cursor: pointer;
  }
  .tab:hover {
    color: var(--trusty-text-primary);
  }
  .tab.active {
    color: var(--trusty-accent);
    border-bottom-color: var(--trusty-accent);
  }
</style>
