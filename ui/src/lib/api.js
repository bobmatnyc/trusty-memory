/*
 * Why: All UI components hit the same /api/v1 surface; centralizing fetch
 * logic gives us one place to handle errors, base URL, and JSON parsing.
 * What: Thin wrappers returning parsed JSON or throwing on non-2xx.
 * Test: Console-call api.status() and confirm shape matches /api/v1/status.
 */

const BASE = '/api/v1';

async function request(path, opts = {}) {
  const res = await fetch(BASE + path, {
    headers: { 'Content-Type': 'application/json' },
    ...opts
  });
  if (!res.ok) {
    let detail = '';
    try {
      detail = await res.text();
    } catch {
      /* ignore */
    }
    throw new Error(`${res.status} ${res.statusText}: ${detail}`);
  }
  if (res.status === 204) return null;
  const ct = res.headers.get('content-type') || '';
  if (ct.includes('application/json')) return res.json();
  return res.text();
}

export const api = {
  status: () => request('/status'),
  config: () => request('/config'),

  listPalaces: () => request('/palaces'),
  createPalace: (body) =>
    request('/palaces', { method: 'POST', body: JSON.stringify(body) }),
  getPalace: (id) => request(`/palaces/${encodeURIComponent(id)}`),

  listDrawers: (id, { room, tag, limit = 50 } = {}) => {
    const qs = new URLSearchParams();
    if (room) qs.set('room', room);
    if (tag) qs.set('tag', tag);
    qs.set('limit', String(limit));
    return request(`/palaces/${encodeURIComponent(id)}/drawers?${qs}`);
  },
  createDrawer: (id, body) =>
    request(`/palaces/${encodeURIComponent(id)}/drawers`, {
      method: 'POST',
      body: JSON.stringify(body)
    }),
  deleteDrawer: (id, drawerId) =>
    request(
      `/palaces/${encodeURIComponent(id)}/drawers/${encodeURIComponent(drawerId)}`,
      { method: 'DELETE' }
    ),

  recall: (id, { q, top_k = 10, deep = false } = {}) => {
    const qs = new URLSearchParams({
      q,
      top_k: String(top_k),
      deep: String(!!deep)
    });
    return request(`/palaces/${encodeURIComponent(id)}/recall?${qs}`);
  },

  kgQuery: (id, subject) => {
    const qs = new URLSearchParams({ subject });
    return request(`/palaces/${encodeURIComponent(id)}/kg?${qs}`);
  },
  kgAssert: (id, body) =>
    request(`/palaces/${encodeURIComponent(id)}/kg`, {
      method: 'POST',
      body: JSON.stringify(body)
    }),

  dreamStatus: () => request('/dream/status'),
  palaceDreamStatus: (id) =>
    request(`/palaces/${encodeURIComponent(id)}/dream/status`),
  runDream: () => request('/dream/run', { method: 'POST' }),

  chat: (body) =>
    fetch(BASE + '/chat', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body)
    }),

  chatProviders: () => request('/chat/providers'),
  listSessions: (palaceId) =>
    request(`/palaces/${encodeURIComponent(palaceId)}/chat/sessions`),
  createSession: (palaceId, title) =>
    request(`/palaces/${encodeURIComponent(palaceId)}/chat/sessions`, {
      method: 'POST',
      body: JSON.stringify({ title })
    }),
  getSession: (palaceId, sessionId) =>
    request(
      `/palaces/${encodeURIComponent(palaceId)}/chat/sessions/${encodeURIComponent(sessionId)}`
    ),
  deleteSession: (palaceId, sessionId) =>
    request(
      `/palaces/${encodeURIComponent(palaceId)}/chat/sessions/${encodeURIComponent(sessionId)}`,
      { method: 'DELETE' }
    )
};
