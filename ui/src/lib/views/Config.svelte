<script>
  import { getConfig, getStatus } from '../state.svelte.js';

  let config = $derived(getConfig());
  let status = $derived(getStatus());
</script>

<h1 class="page-title">Configuration</h1>

<div class="card mb-4">
  <div class="card-header">Daemon</div>
  <div class="card-body">
    <div class="row">
      <div>
        <div class="text-muted text-xs">Version</div>
        <div class="text-mono">{status?.version || '—'}</div>
      </div>
      <div>
        <div class="text-muted text-xs">Data root</div>
        <div class="text-mono text-sm truncate">{status?.data_root || '—'}</div>
      </div>
      <div>
        <div class="text-muted text-xs">Default palace</div>
        <div class="text-mono">{status?.default_palace || '—'}</div>
      </div>
    </div>
  </div>
</div>

<div class="card">
  <div class="card-header">OpenRouter</div>
  <div class="card-body">
    <div class="form-group">
      <label class="form-label">API Key</label>
      <div>
        {#if config?.openrouter_configured}
          <span class="badge badge-success">configured</span>
        {:else}
          <span class="badge badge-warning">not set</span>
          <span class="text-muted text-sm" style="margin-left: 8px">
            Set with: <code>trusty-memory config set openrouter.api_key sk-or-...</code>
          </span>
        {/if}
      </div>
    </div>
    <div class="form-group">
      <label class="form-label">Model</label>
      <div class="text-mono">{config?.model || '—'}</div>
    </div>
    <div class="text-muted text-sm">
      Configuration lives at <code>~/.trusty-memory/config.toml</code>. Edit via
      <code>trusty-memory config show</code> / <code>set</code>.
    </div>
  </div>
</div>

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0 0 var(--trusty-space-5) 0;
    font-weight: 600;
  }
  .row {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: var(--trusty-space-5);
  }
</style>
