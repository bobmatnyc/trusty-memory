<script>
  /*
   * Why: Chat threads need consistent bubble styling for user vs assistant
   * messages, with a streaming indicator while tokens arrive over SSE.
   * What: Renders a single message bubble. User bubbles are right-aligned
   * and accent-colored; assistant bubbles are left-aligned and gray. A
   * blinking cursor appears at the end of assistant bubbles while streaming.
   * Test: Render with role='user' and confirm right alignment; render with
   * role='assistant' streaming=true and confirm cursor blinks after content.
   */
  let { role, content, streaming = false } = $props();

  let isUser = $derived(role === 'user');
</script>

<div class="row" class:row-user={isUser}>
  <div class="bubble" class:bubble-user={isUser} class:bubble-assistant={!isUser}>
    <div class="role">{role}</div>
    <div class="body">{content}{#if streaming && !isUser}<span class="cursor"></span>{/if}</div>
  </div>
</div>

<style>
  .row {
    display: flex;
    justify-content: flex-start;
    margin-bottom: var(--trusty-space-3);
  }
  .row-user {
    justify-content: flex-end;
  }
  .bubble {
    max-width: 78%;
    padding: var(--trusty-space-3) var(--trusty-space-4);
    border-radius: var(--trusty-radius-lg);
    font-size: var(--trusty-fs-sm);
    line-height: 1.55;
    box-shadow: var(--trusty-shadow-sm);
  }
  .bubble-user {
    background: var(--trusty-accent);
    color: var(--trusty-text-inverse);
    border-bottom-right-radius: var(--trusty-radius-sm);
  }
  .bubble-assistant {
    background: var(--trusty-card-bg);
    color: var(--trusty-text-primary);
    border: 1px solid var(--trusty-border);
    border-bottom-left-radius: var(--trusty-radius-sm);
  }
  .role {
    font-size: var(--trusty-fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    font-weight: 600;
    opacity: 0.75;
    margin-bottom: 4px;
  }
  .bubble-user .role {
    color: rgba(255, 255, 255, 0.85);
  }
  .bubble-assistant .role {
    color: var(--trusty-text-muted);
  }
  .body {
    white-space: pre-wrap;
    word-wrap: break-word;
  }
  .cursor {
    display: inline-block;
    width: 7px;
    height: 1em;
    margin-left: 2px;
    vertical-align: text-bottom;
    background: currentColor;
    opacity: 0.7;
    animation: blink 1s steps(2, start) infinite;
  }
  @keyframes blink {
    to {
      visibility: hidden;
    }
  }
</style>
