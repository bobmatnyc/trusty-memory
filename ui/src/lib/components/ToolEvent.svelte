<script>
  /*
   * Why: Operators need transparency into what tools the LLM invokes and what
   * each tool returns — surfacing these inline in the chat thread makes the
   * agent's reasoning auditable without leaving the chat surface.
   * What: Renders a single tool_call or tool_result event as a collapsible
   * card. Tool calls show a wrench icon + formatted JSON args; tool results
   * show a green check + a one-line summary, both expandable for raw content.
   * Test: Render with type='tool_call' and confirm args display as pretty
   * JSON when expanded; render with type='tool_result' and confirm the body
   * is hidden by default and revealed on click.
   */
  let { type, name, args = null, content = '' } = $props();

  let open = $state(false);

  let isCall = $derived(type === 'tool_call');

  // Pretty-print args (object) or content (string-that-might-be-JSON).
  let prettyArgs = $derived.by(() => {
    if (!args) return '';
    if (typeof args === 'string') {
      try {
        return JSON.stringify(JSON.parse(args), null, 2);
      } catch {
        return args;
      }
    }
    try {
      return JSON.stringify(args, null, 2);
    } catch {
      return String(args);
    }
  });

  let prettyContent = $derived.by(() => {
    if (!content) return '';
    if (typeof content !== 'string') {
      try {
        return JSON.stringify(content, null, 2);
      } catch {
        return String(content);
      }
    }
    try {
      return JSON.stringify(JSON.parse(content), null, 2);
    } catch {
      return content;
    }
  });

  // Best-effort short summary for tool_result header.
  let resultSummary = $derived.by(() => {
    if (isCall) return '';
    if (!content) return 'returned';
    try {
      const parsed = JSON.parse(content);
      if (Array.isArray(parsed)) {
        return `returned ${parsed.length} result${parsed.length === 1 ? '' : 's'}`;
      }
      if (parsed && typeof parsed === 'object') {
        const keys = Object.keys(parsed);
        return `returned object (${keys.length} field${keys.length === 1 ? '' : 's'})`;
      }
    } catch {
      // Fall through to length-based summary.
    }
    const len = typeof content === 'string' ? content.length : 0;
    return `returned ${len} char${len === 1 ? '' : 's'}`;
  });

  function toggle() {
    open = !open;
  }
</script>

<div class="tool-event" class:is-call={isCall} class:is-result={!isCall}>
  <button class="header" onclick={toggle} aria-expanded={open}>
    <span class="icon">{isCall ? '🔧' : '✓'}</span>
    <span class="label">
      {#if isCall}
        Called <strong>{name}</strong>
      {:else}
        <strong>{name}</strong> {resultSummary}
      {/if}
    </span>
    <span class="caret">{open ? '▼' : '▶'}</span>
  </button>
  {#if open}
    <pre class="body">{isCall ? prettyArgs : prettyContent}</pre>
  {/if}
</div>

<style>
  .tool-event {
    margin: var(--trusty-space-2) 0;
    border: 1px solid var(--trusty-border);
    border-radius: var(--trusty-radius-sm);
    background: var(--trusty-bg-subtle, rgba(0, 0, 0, 0.02));
    overflow: hidden;
    max-width: 78%;
  }
  .is-result {
    border-color: var(--trusty-success, #10b981);
    background: var(--trusty-success-soft, rgba(16, 185, 129, 0.06));
  }
  .header {
    width: 100%;
    display: flex;
    align-items: center;
    gap: var(--trusty-space-2);
    padding: 6px 10px;
    background: transparent;
    border: none;
    cursor: pointer;
    text-align: left;
    font-family: inherit;
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-primary);
  }
  .header:hover {
    background: rgba(0, 0, 0, 0.03);
  }
  .icon {
    font-size: 13px;
    line-height: 1;
  }
  .is-result .icon {
    color: var(--trusty-success, #10b981);
  }
  .label {
    flex: 1;
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-muted);
  }
  .label strong {
    color: var(--trusty-text-primary);
    font-weight: 600;
  }
  .caret {
    font-size: 10px;
    color: var(--trusty-text-muted);
  }
  .body {
    margin: 0;
    padding: 8px 12px;
    font-family: var(--trusty-font-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
    font-size: 11.5px;
    line-height: 1.5;
    color: var(--trusty-text-primary);
    background: var(--trusty-card-bg);
    border-top: 1px solid var(--trusty-border);
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 320px;
    overflow: auto;
  }
</style>
