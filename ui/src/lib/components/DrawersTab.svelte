<script>
  import { api } from '../api.js';
  import { onMount } from 'svelte';

  let { palaceId, onChanged } = $props();

  let drawers = $state([]);
  let roomFilter = $state('');
  let loading = $state(false);
  let error = $state(null);

  let showAdd = $state(false);
  let addContent = $state('');
  let addRoom = $state('general');
  let addTags = $state('');
  let addImportance = $state(0.5);
  let adding = $state(false);

  const ROOMS = [
    'general',
    'frontend',
    'backend',
    'testing',
    'planning',
    'documentation',
    'research',
    'configuration',
    'meetings'
  ];

  async function load() {
    loading = true;
    error = null;
    try {
      drawers = await api.listDrawers(palaceId, {
        room: roomFilter || undefined,
        limit: 100
      });
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  onMount(load);
  $effect(() => {
    palaceId;
    roomFilter;
    load();
  });

  async function add() {
    if (!addContent.trim()) return;
    adding = true;
    error = null;
    try {
      await api.createDrawer(palaceId, {
        content: addContent.trim(),
        room: addRoom,
        tags: addTags
          .split(',')
          .map((t) => t.trim())
          .filter(Boolean),
        importance: Number(addImportance)
      });
      addContent = '';
      addTags = '';
      showAdd = false;
      await load();
      onChanged?.();
    } catch (e) {
      error = e.message;
    } finally {
      adding = false;
    }
  }

  async function remove(drawerId) {
    if (!confirm('Delete this drawer?')) return;
    try {
      await api.deleteDrawer(palaceId, drawerId);
      await load();
      onChanged?.();
    } catch (e) {
      error = e.message;
    }
  }
</script>

<div class="flex-between mb-4">
  <div class="flex-gap-2" style="align-items: center">
    <label class="text-sm text-muted" for="room-filter">Filter:</label>
    <select id="room-filter" class="select" style="width: auto" bind:value={roomFilter}>
      <option value="">All rooms</option>
      {#each ROOMS as r}
        <option value={r}>{r}</option>
      {/each}
    </select>
  </div>
  <button class="btn btn-primary" onclick={() => (showAdd = !showAdd)}>
    {showAdd ? 'Cancel' : '+ Add Memory'}
  </button>
</div>

{#if showAdd}
  <div class="card mb-4">
    <div class="card-body">
      <div class="form-group">
        <label class="form-label" for="content">Content</label>
        <textarea
          id="content"
          class="textarea"
          placeholder="What should the palace remember?"
          bind:value={addContent}
        ></textarea>
      </div>
      <div class="row">
        <div class="form-group">
          <label class="form-label" for="room">Room</label>
          <select id="room" class="select" bind:value={addRoom}>
            {#each ROOMS as r}
              <option value={r}>{r}</option>
            {/each}
          </select>
        </div>
        <div class="form-group">
          <label class="form-label" for="tags">Tags (comma-separated)</label>
          <input id="tags" class="input" placeholder="rust, async" bind:value={addTags} />
        </div>
        <div class="form-group">
          <label class="form-label" for="importance">Importance: {Number(addImportance).toFixed(2)}</label>
          <input
            id="importance"
            type="range"
            min="0"
            max="1"
            step="0.05"
            bind:value={addImportance}
          />
        </div>
      </div>
      {#if error}<div class="badge badge-danger mb-3">{error}</div>{/if}
      <button class="btn btn-primary" disabled={adding || !addContent.trim()} onclick={add}>
        {adding ? 'Storing…' : 'Store memory'}
      </button>
    </div>
  </div>
{/if}

<div class="card">
  {#if loading}
    <div class="empty">Loading…</div>
  {:else if drawers.length === 0}
    <div class="empty">No drawers found.</div>
  {:else}
    <table class="table">
      <thead>
        <tr>
          <th style="width: 50%">Content</th>
          <th>Tags</th>
          <th>Importance</th>
          <th>Created</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each drawers as d}
          <tr>
            <td class="truncate" style="max-width: 480px" title={d.content}>{d.content}</td>
            <td>
              {#each d.tags as t}<span class="tag">{t}</span>{/each}
              {#if d.tags.length === 0}<span class="text-muted text-xs">—</span>{/if}
            </td>
            <td>
              <span class="bar"><span class="bar-fill" style="width: {(d.importance * 100).toFixed(0)}%"></span></span>
              <span class="text-xs text-muted" style="margin-left: 6px">{d.importance.toFixed(2)}</span>
            </td>
            <td class="text-muted text-xs">{new Date(d.created_at).toLocaleDateString()}</td>
            <td>
              <button class="btn btn-sm btn-danger" onclick={() => remove(d.id)}>Delete</button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>

<style>
  .row {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: var(--trusty-space-3);
  }
</style>
