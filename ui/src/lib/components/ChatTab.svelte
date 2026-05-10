<script>
  import { api } from '../api.js';

  let { palaceId } = $props();

  let messages = $state([]);
  let input = $state('');
  let sending = $state(false);
  let error = $state(null);

  async function send() {
    if (!input.trim() || sending) return;
    const userMsg = { role: 'user', content: input.trim() };
    messages = [...messages, userMsg];
    const history = messages.slice(0, -1);
    input = '';
    sending = true;
    error = null;

    try {
      const res = await api.chat({
        palace_id: palaceId,
        message: userMsg.content,
        history
      });
      if (!res.ok) {
        const txt = await res.text();
        throw new Error(`${res.status}: ${txt}`);
      }
      const assistantMsg = { role: 'assistant', content: '' };
      messages = [...messages, assistantMsg];
      const idx = messages.length - 1;

      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buffer = '';

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        // Parse SSE frames: "data: ...\n\n"
        const frames = buffer.split('\n\n');
        buffer = frames.pop() || '';
        for (const frame of frames) {
          for (const line of frame.split('\n')) {
            if (!line.startsWith('data:')) continue;
            const data = line.slice(5).trim();
            if (data === '[DONE]' || data === '') continue;
            try {
              const parsed = JSON.parse(data);
              if (parsed.delta) {
                messages[idx].content += parsed.delta;
                messages = [...messages];
              } else if (parsed.error) {
                error = parsed.error;
              }
            } catch {
              // Treat non-JSON data as raw text.
              messages[idx].content += data;
              messages = [...messages];
            }
          }
        }
      }
    } catch (e) {
      error = e.message;
    } finally {
      sending = false;
    }
  }
</script>

<div class="chat">
  <div class="messages">
    {#if messages.length === 0}
      <div class="empty">
        Ask a question. Top recall hits from this palace will be added as context.
      </div>
    {/if}
    {#each messages as m}
      <div class="msg msg-{m.role}">
        <div class="msg-role">{m.role}</div>
        <div class="msg-body">{m.content || (sending && m.role === 'assistant' ? '…' : '')}</div>
      </div>
    {/each}
    {#if error}
      <div class="badge badge-danger">{error}</div>
    {/if}
  </div>
  <div class="composer">
    <textarea
      class="textarea"
      placeholder="Send a message…"
      bind:value={input}
      onkeydown={(e) => {
        if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) send();
      }}
    ></textarea>
    <button class="btn btn-primary" disabled={sending || !input.trim()} onclick={send}>
      {sending ? 'Sending…' : 'Send (⌘↵)'}
    </button>
  </div>
</div>

<style>
  .chat {
    display: flex;
    flex-direction: column;
    height: calc(100vh - 260px);
    min-height: 400px;
  }
  .messages {
    flex: 1;
    overflow-y: auto;
    padding: var(--trusty-space-4);
    background: var(--trusty-card-bg);
    border: 1px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
  }
  .msg {
    margin-bottom: var(--trusty-space-4);
  }
  .msg-role {
    font-size: var(--trusty-fs-xs);
    font-weight: 600;
    color: var(--trusty-text-muted);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    margin-bottom: var(--trusty-space-1);
  }
  .msg-user .msg-role {
    color: var(--trusty-accent);
  }
  .msg-body {
    white-space: pre-wrap;
    line-height: 1.5;
  }
  .composer {
    margin-top: var(--trusty-space-3);
    display: flex;
    gap: var(--trusty-space-3);
  }
  .composer .textarea {
    flex: 1;
    min-height: 60px;
  }
  .composer .btn {
    align-self: stretch;
  }
</style>
