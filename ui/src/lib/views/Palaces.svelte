<script>
  import { api } from '../api.js';
  import { getPalaces, refreshPalaces } from '../state.svelte.js';
  import { navigate } from '../router.svelte.js';

  let palaces = $derived(getPalaces());
  let showModal = $state(false);
  let newName = $state('');
  let newDescription = $state('');
  let creating = $state(false);
  let error = $state(null);

  async function create() {
    if (!newName.trim()) return;
    creating = true;
    error = null;
    try {
      await api.createPalace({
        name: newName.trim(),
        description: newDescription.trim() || null
      });
      await refreshPalaces();
      showModal = false;
      newName = '';
      newDescription = '';
    } catch (e) {
      error = e.message;
    } finally {
      creating = false;
    }
  }
</script>

<div class="flex-between mb-5">
  <h1 class="page-title">Palaces</h1>
  <button class="btn btn-primary" onclick={() => (showModal = true)}>+ New Palace</button>
</div>

<div class="card">
  {#if palaces.length === 0}
    <div class="empty">
      No palaces yet. Click <strong>+ New Palace</strong> to create one.
    </div>
  {:else}
    <table class="table">
      <thead>
        <tr>
          <th>Name</th>
          <th>ID</th>
          <th>Description</th>
          <th>Drawers</th>
          <th>Created</th>
        </tr>
      </thead>
      <tbody>
        {#each palaces as p}
          <tr style="cursor: pointer" onclick={() => navigate(`/palaces/${p.id}`)}>
            <td><strong>{p.name}</strong></td>
            <td class="text-mono text-xs text-muted">{p.id}</td>
            <td class="text-sm text-muted truncate" style="max-width: 320px">
              {p.description || '—'}
            </td>
            <td><span class="badge">{p.drawer_count}</span></td>
            <td class="text-muted text-sm">{new Date(p.created_at).toLocaleDateString()}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>

{#if showModal}
  <div class="modal-backdrop" onclick={() => (showModal = false)} role="presentation">
    <div class="modal" onclick={(e) => e.stopPropagation()} role="dialog">
      <div class="modal-header">
        <span class="modal-title">Create Palace</span>
        <button class="btn btn-sm" onclick={() => (showModal = false)}>×</button>
      </div>
      <div class="modal-body">
        <div class="form-group">
          <label class="form-label" for="palace-name">Name (kebab-case)</label>
          <input id="palace-name" class="input" placeholder="my-project" bind:value={newName} />
        </div>
        <div class="form-group">
          <label class="form-label" for="palace-desc">Description</label>
          <textarea id="palace-desc" class="textarea" placeholder="Optional summary…" bind:value={newDescription}></textarea>
        </div>
        {#if error}
          <div class="badge badge-danger">{error}</div>
        {/if}
      </div>
      <div class="modal-footer">
        <button class="btn" onclick={() => (showModal = false)}>Cancel</button>
        <button class="btn btn-primary" disabled={creating || !newName.trim()} onclick={create}>
          {creating ? 'Creating…' : 'Create'}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0;
    font-weight: 600;
  }
</style>
