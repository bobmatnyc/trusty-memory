// Why: All trusty-memory interaction goes through one typed HTTP client so the
// extension never sprinkles raw `fetch` calls (and string-typed JSON) through
// the command handlers. Centralizing also concentrates port discovery and
// error normalization in a single place.
// What: A `TrustyMemoryClient` that resolves the daemon base URL (explicit
// config port, or the daemon-written `http_addr` discovery file) and exposes
// typed methods mirroring the `/api/v1/*` routes the sidebar and commands need.
// Test: Exercised manually against a running daemon; the daemon side is
// covered by `cargo test -p trusty-memory-mcp web::tests`.

import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

/**
 * Shape of a single drawer (atomic memory unit) as serialized by the daemon.
 *
 * Why: `/api/v1/palaces/{id}/drawers` and recall results both embed this; a
 * shared interface keeps the sidebar and quick-pick rendering type-safe.
 * What: Mirrors `trusty_memory_core::palace::Drawer` field-for-field.
 * Test: Field names are pinned by the Rust `#[derive(Serialize)]`.
 */
export interface Drawer {
  readonly id: string;
  readonly room_id: string;
  readonly content: string;
  readonly importance: number;
  readonly source_file: string | null;
  readonly created_at: string;
  readonly tags: readonly string[];
  readonly last_accessed_at: string | null;
  readonly access_count: number;
}

/**
 * Palace summary as returned by `/api/v1/palaces`.
 *
 * Why: The sidebar lists palaces with their memory counts; this is the exact
 * payload the daemon sends.
 * What: Mirrors the `PalaceInfo` struct in `trusty-memory-mcp/src/web.rs`.
 * Test: Field names pinned by the daemon's serializer.
 */
export interface PalaceInfo {
  readonly id: string;
  readonly name: string;
  readonly description: string | null;
  readonly drawer_count: number;
  readonly vector_count: number;
  readonly kg_triple_count: number;
  readonly wing_count: number;
  readonly created_at: string;
}

/**
 * A single recall hit as returned by `/api/v1/palaces/{id}/recall`.
 *
 * Why: The recall quick-pick needs the drawer plus its relevance score and the
 * retrieval layer that produced it.
 * What: Mirrors the JSON object built in `recall_handler`.
 * Test: Field names pinned by the daemon's `json!` macro.
 */
export interface RecallResult {
  readonly drawer: Drawer;
  readonly score: number;
  readonly layer: string;
}

/** Application name used by the daemon for its data directory. */
const APP_NAME = 'trusty-memory';

/**
 * Error raised when the daemon cannot be reached or returns a non-2xx status.
 *
 * Why: Command handlers must distinguish "expected, user-facing" failures
 * (daemon down, bad request) from programmer errors so they can surface a
 * clean `showErrorMessage` without a stack trace.
 * What: Carries a human-readable message; thrown by every client method.
 * Test: Asserted indirectly via command-handler error paths.
 */
export class TrustyMemoryError extends Error {
  public constructor(message: string) {
    super(message);
    this.name = 'TrustyMemoryError';
  }
}

/**
 * Resolve the daemon's data directory (where `http_addr` lives).
 *
 * Why: Port discovery reads the daemon-written `http_addr` file; its location
 * follows the same `dirs::data_dir()` rules as `trusty_common::resolve_data_dir`.
 * What: Returns the macOS `Application Support` path, the XDG/AppData path on
 * other platforms, or a `~/.trusty-memory` fallback.
 * Test: Path layout verified manually against a running daemon.
 */
function dataDir(): string {
  const home = os.homedir();
  switch (process.platform) {
    case 'darwin':
      return path.join(home, 'Library', 'Application Support', APP_NAME);
    case 'win32': {
      const appData = process.env.APPDATA;
      return appData !== undefined && appData.length > 0
        ? path.join(appData, APP_NAME)
        : path.join(home, `.${APP_NAME}`);
    }
    default: {
      const xdg = process.env.XDG_DATA_HOME;
      return xdg !== undefined && xdg.length > 0
        ? path.join(xdg, APP_NAME)
        : path.join(home, '.local', 'share', APP_NAME);
    }
  }
}

/**
 * Read the daemon's bound HTTP address from its discovery file.
 *
 * Why: The daemon auto-port-walks, so the port is only known at runtime via
 * the `http_addr` file it writes on startup.
 * What: Reads `{dataDir}/http_addr`, trims whitespace, returns `host:port`
 * or `undefined` when the file is absent (daemon never started).
 * Test: Covered by the Rust side `daemon_addr_round_trips`.
 */
function readDiscoveredAddr(): string | undefined {
  const file = path.join(dataDir(), 'http_addr');
  try {
    const raw = fs.readFileSync(file, 'utf8').trim();
    return raw.length > 0 ? raw : undefined;
  } catch {
    return undefined;
  }
}

/**
 * Resolve the base URL of the trusty-memory daemon.
 *
 * Why: Commands and the sidebar need a single, validated `http://host:port`
 * origin; configuration (explicit port) must override discovery.
 * What: When `configPort` is set, builds `http://127.0.0.1:<port>`. Otherwise
 * reads the `http_addr` discovery file. Throws `TrustyMemoryError` when
 * neither yields an address.
 * Test: Exercised by `TrustyMemoryClient.fromConfig`.
 */
export function resolveBaseUrl(configPort: number | null): string {
  if (configPort !== null && Number.isFinite(configPort) && configPort > 0) {
    return `http://127.0.0.1:${Math.trunc(configPort)}`;
  }
  const discovered = readDiscoveredAddr();
  if (discovered === undefined) {
    throw new TrustyMemoryError(
      'Could not locate the trusty-memory daemon. Start it with ' +
        '`trusty-memory serve`, or set `trustyMemory.httpPort` in settings.',
    );
  }
  // The discovery file may store a bare `host:port`; normalize to a URL.
  return discovered.startsWith('http')
    ? discovered
    : `http://${discovered}`;
}

/** JSON request body for creating a drawer. */
interface CreateDrawerBody {
  readonly content: string;
  readonly room?: string;
  readonly tags?: readonly string[];
  readonly importance?: number;
}

/**
 * Typed HTTP client for the trusty-memory daemon `/api/v1` surface.
 *
 * Why: Keeps every command handler free of URL construction and JSON casts.
 * What: Wraps `fetch` with timeout handling and 2xx checking; exposes the
 * subset of endpoints the extension uses.
 * Test: Manual against a live daemon; daemon side covered by Rust tests.
 */
export class TrustyMemoryClient {
  private readonly baseUrl: string;
  private static readonly TIMEOUT_MS = 8000;

  private constructor(baseUrl: string) {
    this.baseUrl = baseUrl.replace(/\/+$/, '');
  }

  /**
   * Build a client from the extension configuration.
   *
   * Why: Callers should not duplicate port-resolution logic.
   * What: Resolves the base URL from the configured port (or discovery file)
   * and returns a ready-to-use client.
   * Test: Throws `TrustyMemoryError` when no daemon address is resolvable.
   */
  public static fromConfig(configPort: number | null): TrustyMemoryClient {
    return new TrustyMemoryClient(resolveBaseUrl(configPort));
  }

  /**
   * Issue a JSON request and parse the response body.
   *
   * Why: Every endpoint shares timeout, error-normalization, and JSON-parsing
   * logic; concentrating it avoids drift between methods.
   * What: Performs `fetch` with an abort-based timeout, raises
   * `TrustyMemoryError` on network failure or non-2xx status, and returns the
   * decoded JSON typed as `T`.
   * Test: Error branches exercised via command-handler failure paths.
   */
  private async request<T>(
    pathname: string,
    init?: RequestInit,
  ): Promise<T> {
    const controller = new AbortController();
    const timer = setTimeout(
      () => controller.abort(),
      TrustyMemoryClient.TIMEOUT_MS,
    );
    let response: Response;
    try {
      response = await fetch(`${this.baseUrl}${pathname}`, {
        ...init,
        signal: controller.signal,
      });
    } catch (err) {
      const reason = err instanceof Error ? err.message : String(err);
      throw new TrustyMemoryError(
        `Failed to reach trusty-memory daemon at ${this.baseUrl}: ${reason}`,
      );
    } finally {
      clearTimeout(timer);
    }

    if (!response.ok) {
      const body = await response.text().catch(() => '');
      throw new TrustyMemoryError(
        `trusty-memory daemon returned ${response.status} ${response.statusText}` +
          (body.length > 0 ? `: ${body}` : ''),
      );
    }

    if (response.status === 204) {
      return undefined as T;
    }
    return (await response.json()) as T;
  }

  /**
   * List all palaces with their memory counts.
   *
   * Why: The sidebar's top-level nodes are palaces.
   * What: `GET /api/v1/palaces`.
   * Test: Daemon side covered by `palace_list_includes_richer_counts`.
   */
  public listPalaces(): Promise<PalaceInfo[]> {
    return this.request<PalaceInfo[]>('/api/v1/palaces');
  }

  /**
   * List drawers for a palace, importance-ranked, capped at `limit`.
   *
   * Why: The sidebar shows the top-15 (L1) memories for the active palace.
   * What: `GET /api/v1/palaces/{id}/drawers?limit=<n>`.
   * Test: Daemon side covered by `list_drawers` retrieval tests.
   */
  public listDrawers(palaceId: string, limit: number): Promise<Drawer[]> {
    const id = encodeURIComponent(palaceId);
    return this.request<Drawer[]>(
      `/api/v1/palaces/${id}/drawers?limit=${Math.trunc(limit)}`,
    );
  }

  /**
   * Store a new memory in the given palace.
   *
   * Why: Backs the `Store Selection` command.
   * What: `POST /api/v1/palaces/{id}/drawers` with the drawer body; returns
   * the new drawer id.
   * Test: Daemon side covered by `create_drawer` / `remember` tests.
   */
  public async createDrawer(
    palaceId: string,
    body: CreateDrawerBody,
  ): Promise<string> {
    const id = encodeURIComponent(palaceId);
    const result = await this.request<{ id: string }>(
      `/api/v1/palaces/${id}/drawers`,
      {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
      },
    );
    return result.id;
  }

  /**
   * Run a semantic recall query against a palace.
   *
   * Why: Backs the `Recall` quick-pick command.
   * What: `GET /api/v1/palaces/{id}/recall?q=<query>&top_k=<n>`.
   * Test: Daemon side covered by `recall_handler` tests.
   */
  public recall(
    palaceId: string,
    query: string,
    topK: number,
  ): Promise<RecallResult[]> {
    const id = encodeURIComponent(palaceId);
    const q = encodeURIComponent(query);
    return this.request<RecallResult[]>(
      `/api/v1/palaces/${id}/recall?q=${q}&top_k=${Math.trunc(topK)}`,
    );
  }
}
