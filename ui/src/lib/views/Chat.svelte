<script>
  /*
   * Why: Operators want a single top-level chat surface that ties together
   * an active palace, persisted sessions, and live SSE token streaming —
   * separate from the per-palace ChatTab so chat is a first-class view.
   * What: Three-pane layout (header / session sidebar / thread + composer)
   * driven by Svelte 5 runes. Fetches providers + palaces on mount, lists
   * sessions per palace, streams responses from POST /api/v1/chat.
   * Test: Open #/chat, pick a palace, type a message and Ctrl+Enter — a
   * new session appears in the sidebar and assistant tokens stream in.
   */
  import { onMount, tick } from 'svelte';
  import { api } from '../api.js';
  import ChatMessage from '../components/ChatMessage.svelte';
  import ToolEvent from '../components/ToolEvent.svelte';

  let palaces = $state([]);
  let provider = $state(null); // { name, model } or null
  let providers = $state([]);
  let selectedPalace = $state('');
  let sessions = $state([]);
  let currentSessionId = $state(null);
  // Thread items are heterogeneous:
  //   { type: 'user' | 'assistant', role, content, streaming? }
  //   { type: 'tool_call', name, arguments, id }
  //   { type: 'tool_result', name, content, id }
  // Only user/assistant items are sent back as history; tool events stay
  // in the display for transparency.
  let thread = $state([]);
  let input = $state('');
  let streaming = $state(false);
  let error = $state(null);
  let loadingSessions = $state(false);
  let loadingThread = $state(false);
  let messagesEl;

  onMount(async () => {
    try {
      const [provResp, palaceList] = await Promise.all([
        api.chatProviders(),
        api.listPalaces()
      ]);
      providers = provResp?.providers ?? [];
      const activeName = provResp?.active;
      provider = providers.find((p) => p.name === activeName) ?? null;
      palaces = palaceList ?? [];
      if (palaces.length > 0) {
        selectedPalace = palaces[0].id;
        await loadSessions();
      }
    } catch (e) {
      error = e.message || String(e);
    }
  });

  async function loadSessions() {
    if (!selectedPalace) return;
    loadingSessions = true;
    try {
      sessions = (await api.listSessions(selectedPalace)) ?? [];
    } catch (e) {
      error = e.message || String(e);
    } finally {
      loadingSessions = false;
    }
  }

  async function onPalaceChange() {
    currentSessionId = null;
    thread = [];
    error = null;
    await loadSessions();
  }

  async function openSession(id) {
    if (streaming) return;
    loadingThread = true;
    error = null;
    try {
      const sess = await api.getSession(selectedPalace, id);
      currentSessionId = sess.id;
      thread = (sess.history ?? []).map((m) => ({
        ...m,
        type: m.role === 'user' ? 'user' : 'assistant',
        streaming: false
      }));
      await scrollToBottom();
    } catch (e) {
      error = e.message || String(e);
    } finally {
      loadingThread = false;
    }
  }

  function newChat() {
    if (streaming) return;
    currentSessionId = null;
    thread = [];
    error = null;
    input = '';
  }

  async function deleteSession(id, ev) {
    ev.stopPropagation();
    if (streaming) return;
    if (!confirm('Delete this session?')) return;
    try {
      await api.deleteSession(selectedPalace, id);
      if (currentSessionId === id) {
        currentSessionId = null;
        thread = [];
      }
      await loadSessions();
    } catch (e) {
      error = e.message || String(e);
    }
  }

  async function scrollToBottom() {
    await tick();
    if (messagesEl) messagesEl.scrollTop = messagesEl.scrollHeight;
  }

  async function send() {
    const text = input.trim();
    if (!text || streaming || !selectedPalace) return;
    error = null;

    // Optimistically append user message.
    thread = [...thread, { type: 'user', role: 'user', content: text }];
    const userMsg = text;
    input = '';
    streaming = true;
    await scrollToBottom();

    // Create session if needed.
    if (!currentSessionId) {
      try {
        const title = userMsg.slice(0, 40);
        const sess = await api.createSession(selectedPalace, title);
        currentSessionId = sess.id;
      } catch (e) {
        error = `Failed to create session: ${e.message || e}`;
        streaming = false;
        return;
      }
    }

    // Append empty assistant placeholder. We track by id rather than index
    // because tool_call/tool_result frames may be appended after this point
    // and shift array positions.
    const assistantId = Date.now();
    thread = [
      ...thread,
      { type: 'assistant', role: 'assistant', content: '', streaming: true, id: assistantId }
    ];
    const findAssistantIdx = () => thread.findIndex((m) => m.id === assistantId);

    try {
      const res = await api.chat({
        palace_id: selectedPalace,
        session_id: currentSessionId,
        message: userMsg
      });
      if (!res.ok) {
        const txt = await res.text();
        throw new Error(`${res.status}: ${txt}`);
      }
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffer = '';
      let done = false;
      while (!done) {
        const { value, done: rdone } = await reader.read();
        done = rdone;
        if (value) buffer += decoder.decode(value, { stream: true });
        const frames = buffer.split('\n\n');
        buffer = frames.pop() || '';
        for (const frame of frames) {
          for (const line of frame.split('\n')) {
            if (!line.startsWith('data:')) continue;
            const data = line.slice(5).trim();
            if (data === '' ) continue;
            if (data === '[DONE]') {
              done = true;
              break;
            }
            try {
              const parsed = JSON.parse(data);
              if (parsed.session_id && !currentSessionId) {
                currentSessionId = parsed.session_id;
              } else if (parsed.session_id) {
                // First frame: server may echo session id we already have.
                currentSessionId = parsed.session_id;
              }
              if (parsed.delta) {
                const ai = findAssistantIdx();
                if (ai >= 0) {
                  thread[ai].content += parsed.delta;
                  thread = [...thread];
                  await scrollToBottom();
                }
              }
              if (parsed.tool_call) {
                thread = [
                  ...thread,
                  {
                    type: 'tool_call',
                    name: parsed.tool_call.name,
                    arguments: parsed.tool_call.arguments,
                    id: Date.now() + Math.random()
                  }
                ];
                await scrollToBottom();
              }
              if (parsed.tool_result) {
                thread = [
                  ...thread,
                  {
                    type: 'tool_result',
                    name: parsed.tool_result.name,
                    content: parsed.tool_result.content,
                    id: Date.now() + Math.random()
                  }
                ];
                await scrollToBottom();
              }
              if (parsed.error) {
                error = parsed.error;
              }
            } catch {
              const ai = findAssistantIdx();
              if (ai >= 0) {
                thread[ai].content += data;
                thread = [...thread];
              }
            }
          }
        }
      }
      {
        const ai = findAssistantIdx();
        if (ai >= 0) {
          thread[ai].streaming = false;
          thread = [...thread];
        }
      }
      await loadSessions();
    } catch (e) {
      error = e.message || String(e);
      const ai = findAssistantIdx();
      if (ai >= 0) {
        thread[ai].streaming = false;
        thread = [...thread];
      }
    } finally {
      streaming = false;
    }
  }

  function onKeydown(e) {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      send();
    }
  }

  function fmtTime(iso) {
    if (!iso) return '';
    try {
      return new Date(iso).toLocaleString();
    } catch {
      return iso;
    }
  }
</script>

<div class="chat-page">
  <div class="header">
    <div class="palace-picker">
      <label class="label" for="palace-select">Palace</label>
      <select
        id="palace-select"
        class="select"
        bind:value={selectedPalace}
        onchange={onPalaceChange}
        disabled={streaming}
      >
        {#if palaces.length === 0}
          <option value="">— no palaces —</option>
        {/if}
        {#each palaces as p}
          <option value={p.id}>{p.name} ({p.id})</option>
        {/each}
      </select>
    </div>
    <div class="provider">
      {#if provider}
        <span class="badge badge-success">{provider.name}</span>
        <span class="text-mono text-xs text-muted">{provider.model}</span>
      {:else}
        <span class="badge badge-muted">No provider configured</span>
      {/if}
    </div>
  </div>

  <div class="body">
    <aside class="sessions">
      <button class="btn btn-primary btn-block" onclick={newChat} disabled={streaming}>
        + New Chat
      </button>
      <div class="session-list">
        {#if loadingSessions}
          <div class="text-muted text-sm empty-row">Loading…</div>
        {:else if sessions.length === 0}
          <div class="text-muted text-sm empty-row">No sessions yet.</div>
        {:else}
          {#each sessions as s}
            <div
              class="session-item"
              class:active={s.id === currentSessionId}
              onclick={() => openSession(s.id)}
              role="button"
              tabindex="0"
              onkeydown={(e) => e.key === 'Enter' && openSession(s.id)}
            >
              <div class="session-title" title={s.title}>{s.title || '(untitled)'}</div>
              <div class="session-meta">
                <span>{s.message_count ?? 0} msg</span>
                <span>· {fmtTime(s.updated_at || s.created_at)}</span>
              </div>
              <button
                class="session-del"
                onclick={(e) => deleteSession(s.id, e)}
                aria-label="Delete session"
                title="Delete session"
              >×</button>
            </div>
          {/each}
        {/if}
      </div>
    </aside>

    <section class="thread-pane">
      <div class="messages" bind:this={messagesEl}>
        {#if loadingThread}
          <div class="empty">Loading session…</div>
        {:else if thread.length === 0}
          <div class="empty">
            {#if selectedPalace}
              Start a new conversation. Top recall hits from
              <strong>{selectedPalace}</strong> will be added as context.
            {:else}
              Create a palace first to start chatting.
            {/if}
          </div>
        {:else}
          {#each thread as m (m.id ?? m)}
            {#if m.type === 'tool_call'}
              <ToolEvent type="tool_call" name={m.name} args={m.arguments} />
            {:else if m.type === 'tool_result'}
              <ToolEvent type="tool_result" name={m.name} content={m.content} />
            {:else}
              <ChatMessage role={m.role} content={m.content} streaming={m.streaming} />
            {/if}
          {/each}
        {/if}
        {#if error}
          <div class="error-row">
            <span class="badge badge-danger">{error}</span>
          </div>
        {/if}
      </div>
      <div class="composer">
        <textarea
          class="textarea"
          placeholder={selectedPalace ? 'Type a message…  (Ctrl/⌘+Enter to send)' : 'Select a palace first'}
          bind:value={input}
          onkeydown={onKeydown}
          disabled={streaming || !selectedPalace}
          rows="3"
        ></textarea>
        <button
          class="btn btn-primary"
          disabled={streaming || !input.trim() || !selectedPalace}
          onclick={send}
        >
          {streaming ? 'Streaming…' : 'Send'}
        </button>
      </div>
    </section>
  </div>
</div>

<style>
  .chat-page {
    display: flex;
    flex-direction: column;
    height: calc(100vh - var(--trusty-topbar-height) - var(--trusty-space-5) * 2);
    min-height: 500px;
  }
  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--trusty-space-3) var(--trusty-space-4);
    background: var(--trusty-card-bg);
    border: 1px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
    margin-bottom: var(--trusty-space-4);
  }
  .palace-picker {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-3);
  }
  .label {
    font-size: var(--trusty-fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--trusty-text-muted);
    font-weight: 600;
  }
  .select {
    padding: 6px 10px;
    border-radius: var(--trusty-radius-sm);
    border: 1px solid var(--trusty-border);
    background: var(--trusty-card-bg);
    color: var(--trusty-text-primary);
    font-family: inherit;
    font-size: var(--trusty-fs-sm);
    min-width: 240px;
  }
  .provider {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-2);
  }
  .body {
    flex: 1;
    display: grid;
    grid-template-columns: 260px 1fr;
    gap: var(--trusty-space-4);
    min-height: 0;
  }
  .sessions {
    display: flex;
    flex-direction: column;
    gap: var(--trusty-space-3);
    background: var(--trusty-card-bg);
    border: 1px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
    padding: var(--trusty-space-3);
    min-height: 0;
  }
  .btn-block {
    width: 100%;
  }
  .session-list {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .empty-row {
    padding: var(--trusty-space-3);
    text-align: center;
  }
  .session-item {
    position: relative;
    padding: var(--trusty-space-2) var(--trusty-space-3);
    border-radius: var(--trusty-radius-sm);
    cursor: pointer;
    border: 1px solid transparent;
    transition: background 0.15s ease;
  }
  .session-item:hover {
    background: var(--trusty-accent-soft);
  }
  .session-item.active {
    background: var(--trusty-accent-soft);
    border-color: var(--trusty-accent);
  }
  .session-title {
    font-size: var(--trusty-fs-sm);
    font-weight: 500;
    color: var(--trusty-text-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    padding-right: 18px;
  }
  .session-meta {
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-muted);
    margin-top: 2px;
    display: flex;
    gap: 4px;
  }
  .session-del {
    position: absolute;
    top: 4px;
    right: 4px;
    background: transparent;
    border: none;
    color: var(--trusty-text-muted);
    font-size: 16px;
    line-height: 1;
    padding: 2px 6px;
    border-radius: 4px;
    cursor: pointer;
    opacity: 0;
    transition: opacity 0.15s ease;
  }
  .session-item:hover .session-del {
    opacity: 1;
  }
  .session-del:hover {
    color: var(--trusty-danger);
    background: var(--trusty-danger-soft);
  }
  .thread-pane {
    display: flex;
    flex-direction: column;
    gap: var(--trusty-space-3);
    background: var(--trusty-card-bg);
    border: 1px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
    padding: var(--trusty-space-4);
    min-height: 0;
  }
  .messages {
    flex: 1;
    overflow-y: auto;
    padding-right: var(--trusty-space-2);
  }
  .empty {
    padding: var(--trusty-space-6);
    text-align: center;
    color: var(--trusty-text-muted);
    font-size: var(--trusty-fs-sm);
  }
  .error-row {
    margin-top: var(--trusty-space-3);
  }
  .composer {
    display: flex;
    gap: var(--trusty-space-3);
    align-items: stretch;
  }
  .composer .textarea {
    flex: 1;
    min-height: 60px;
    resize: vertical;
    font-family: inherit;
    padding: var(--trusty-space-3);
    border: 1px solid var(--trusty-border);
    border-radius: var(--trusty-radius-sm);
    background: var(--trusty-card-bg);
    color: var(--trusty-text-primary);
    font-size: var(--trusty-fs-sm);
  }
  .composer .btn {
    align-self: stretch;
    min-width: 120px;
  }
  @media (max-width: 800px) {
    .body {
      grid-template-columns: 1fr;
    }
    .sessions {
      max-height: 200px;
    }
  }
</style>
