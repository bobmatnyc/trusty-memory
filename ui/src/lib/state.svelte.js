/*
 * Why: Centralised reactive state for the active palace selector and a cache
 * of /api/v1/status so multiple views don't refetch on every mount.
 * What: Exports getters/setters using Svelte 5 runes.
 * Test: Mount two views, change the palace in one, observe the other update.
 */

import { api } from './api.js';

let _activePalace = $state(localStorage.getItem('trusty.activePalace') || '');
let _status = $state(null);
let _palaces = $state([]);
let _config = $state(null);
let _dreamStatus = $state(null);

export function getActivePalace() {
  return _activePalace;
}

export function setActivePalace(id) {
  _activePalace = id || '';
  if (id) localStorage.setItem('trusty.activePalace', id);
  else localStorage.removeItem('trusty.activePalace');
}

export function getStatus() {
  return _status;
}

export function getPalaces() {
  return _palaces;
}

export function getConfig() {
  return _config;
}

export async function refreshStatus() {
  _status = await api.status();
  return _status;
}

export async function refreshPalaces() {
  _palaces = await api.listPalaces();
  // Auto-select first palace if none active.
  if (!_activePalace && _palaces.length > 0) {
    setActivePalace(_palaces[0].id);
  }
  return _palaces;
}

export async function refreshConfig() {
  _config = await api.config();
  return _config;
}

export function getDreamStatus() {
  return _dreamStatus;
}

export async function refreshDreamStatus() {
  _dreamStatus = await api.dreamStatus();
  return _dreamStatus;
}

export async function runDream() {
  _dreamStatus = await api.runDream();
  return _dreamStatus;
}

/*
 * Why: The dashboard subscribes to live `DaemonEvent` pushes from the daemon's
 * /sse endpoint so palace/drawer/dream changes appear instantly without
 * polling. EventSource auto-reconnects on transient disconnects.
 * What: Opens an EventSource, routes each event to the appropriate refresher,
 * and returns the source so callers can close it on teardown.
 * Test: Trigger a remember via /api/v1/palaces/.../drawers and observe the
 * drawer table + total_drawers stat card update without a manual refresh.
 */
export function initEventStream() {
  const es = new EventSource('/sse');
  es.onmessage = (e) => {
    let event;
    try {
      event = JSON.parse(e.data);
    } catch {
      return;
    }
    switch (event.type) {
      case 'palace_created':
        refreshPalaces();
        refreshStatus();
        break;
      case 'drawer_added':
      case 'drawer_deleted':
        refreshPalaces();
        refreshStatus();
        break;
      case 'dream_completed':
        refreshDreamStatus();
        refreshStatus();
        break;
      case 'status_changed':
        refreshStatus();
        break;
      case 'connected':
      case 'lag':
      default:
        break;
    }
  };
  es.onerror = () => {
    // EventSource reconnects automatically; just note the blip.
    console.warn('SSE connection lost, will reconnect...');
  };
  return es;
}
