/*
 * Why: Hash-based router avoids server-side route matching and works under
 * the embedded SPA shell at any mount path.
 * What: Provides a reactive `route` rune-backed store and helpers to push.
 * Test: Change window.location.hash and confirm `route.path` updates.
 */

function parse(hash) {
  const raw = (hash || '').replace(/^#/, '') || '/';
  const [pathPart, queryPart] = raw.split('?');
  const segments = pathPart.split('/').filter(Boolean);
  const query = Object.fromEntries(new URLSearchParams(queryPart || ''));
  return { path: pathPart || '/', segments, query };
}

let _route = $state(parse(typeof window !== 'undefined' ? window.location.hash : '/'));

if (typeof window !== 'undefined') {
  window.addEventListener('hashchange', () => {
    _route = parse(window.location.hash);
  });
}

export function getRoute() {
  return _route;
}

export function navigate(path) {
  window.location.hash = path;
}

export const router = {
  get current() {
    return _route;
  }
};
