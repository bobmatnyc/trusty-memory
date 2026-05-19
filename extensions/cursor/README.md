# Trusty Memory — VS Code / Cursor Extension

A sidebar panel and editor commands for interacting with the
[trusty-memory](../../README.md) daemon from VS Code or Cursor.

## Features

### Commands (Command Palette)

| Command | Description |
|---------|-------------|
| `Trusty Memory: Store Selection` | Store the highlighted editor text as a memory in the active palace. |
| `Trusty Memory: Recall` | Quick-pick semantic search over the active palace. |
| `Trusty Memory: Sync Cursor Rules` | Run `trusty-memory cursor sync` for the current workspace. |
| `Trusty Memory: Refresh` | Re-fetch the sidebar. |

### Sidebar panel

A "Trusty Memory" view in the activity bar lists every palace with its memory
count. Expand a palace to see its top-15 (L1) memories. Use the refresh button
in the view title to re-fetch.

### Status bar

A status bar item shows the active palace name and memory count. Click it to
focus the sidebar.

## Configuration (`settings.json`)

| Setting | Default | Description |
|---------|---------|-------------|
| `trustyMemory.httpPort` | `null` | Daemon TCP port. When `null`, read from the daemon's `http_addr` discovery file. |
| `trustyMemory.defaultPalace` | `null` | Palace to operate on. When `null`, derived from the workspace folder name. |
| `trustyMemory.binaryPath` | `trusty-memory` | Path to the CLI binary (used by `Sync Cursor Rules`). |

## Building

```sh
npm install
npm run typecheck   # strict TypeScript verification
npm run build       # esbuild bundle -> dist/extension.js
npm run package     # produce a .vsix (requires @vscode/vsce)
```

From the repository root, `make ext-build` runs the install + build steps.

## Architecture

- `src/api.ts` — typed HTTP client + `http_addr` port discovery.
- `src/config.ts` — settings accessor with workspace-folder fallback.
- `src/sidebar.ts` — `TreeDataProvider` for the palace/drawer tree.
- `src/extension.ts` — command registration and lifecycle wiring.

The extension talks to `http://127.0.0.1:<port>/api/v1/*`. `Sync Cursor Rules`
shells out to the CLI because the daemon exposes no cursor-sync HTTP endpoint.
