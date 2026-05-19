// Why: VS Code loads exactly one entry module per extension; this file wires
// the three commands, the sidebar tree view, and the status bar item together
// and owns their lifetimes via the `ExtensionContext` subscriptions.
// What: `activate` registers everything; `deactivate` is a no-op because all
// disposables are tracked by `context.subscriptions`.
// Test: Manual in the Extension Development Host; data layer is `api.ts`.

import { execFile } from 'child_process';
import * as vscode from 'vscode';
import { TrustyMemoryClient, TrustyMemoryError } from './api';
import { readConfig } from './config';
import { PalaceTreeProvider } from './sidebar';

/** Number of recall results requested for the quick-pick. */
const RECALL_TOP_K = 15;

/**
 * Surface an error to the user without leaking stack traces.
 *
 * Why: Every command catches failures; a shared helper keeps the messaging
 * consistent and distinguishes expected `TrustyMemoryError`s from bugs.
 * What: Shows `showErrorMessage` with the error text.
 * Test: Triggered by command failure paths.
 */
function reportError(prefix: string, err: unknown): void {
  const detail =
    err instanceof TrustyMemoryError || err instanceof Error
      ? err.message
      : String(err);
  void vscode.window.showErrorMessage(`${prefix}: ${detail}`);
}

/**
 * Store the active editor's selection as a memory in the active palace.
 *
 * Why: Backs `Trusty Memory: Store Selection` — the primary capture path.
 * What: Reads the selection, posts it to `/api/v1/palaces/{id}/drawers`, then
 * refreshes the sidebar so the new memory appears.
 * Test: Manual; daemon side covered by `create_drawer` tests.
 */
async function storeSelection(tree: PalaceTreeProvider): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  if (editor === undefined) {
    void vscode.window.showWarningMessage(
      'Trusty Memory: open a file and select text to store.',
    );
    return;
  }
  const text = editor.document.getText(editor.selection).trim();
  if (text.length === 0) {
    void vscode.window.showWarningMessage(
      'Trusty Memory: nothing selected.',
    );
    return;
  }

  try {
    const { httpPort, palace } = readConfig();
    const client = TrustyMemoryClient.fromConfig(httpPort);
    const language = editor.document.languageId;
    await client.createDrawer(palace, {
      content: text,
      tags: ['vscode', language],
      importance: 0.6,
    });
    void vscode.window.showInformationMessage(
      `Trusty Memory: stored ${text.length} chars in "${palace}".`,
    );
    tree.refresh();
  } catch (err) {
    reportError('Trusty Memory: failed to store selection', err);
  }
}

/**
 * Prompt for a query and show recall results in a quick-pick.
 *
 * Why: Backs `Trusty Memory: Recall` — lets the user search the palace.
 * What: Asks for a query string, calls `/api/v1/palaces/{id}/recall`, and
 * renders hits; choosing one opens its content in an untitled editor.
 * Test: Manual; daemon side covered by `recall_handler` tests.
 */
async function recall(): Promise<void> {
  const query = await vscode.window.showInputBox({
    prompt: 'Trusty Memory: recall query',
    placeHolder: 'What do you want to remember?',
  });
  if (query === undefined || query.trim().length === 0) {
    return;
  }

  try {
    const { httpPort, palace } = readConfig();
    const client = TrustyMemoryClient.fromConfig(httpPort);
    const results = await client.recall(palace, query.trim(), RECALL_TOP_K);
    if (results.length === 0) {
      void vscode.window.showInformationMessage(
        `Trusty Memory: no memories matched "${query}".`,
      );
      return;
    }

    const items: (vscode.QuickPickItem & { content: string })[] =
      results.map((r) => ({
        label: r.drawer.content.split('\n', 1)[0].trim(),
        description: `${(r.score * 100).toFixed(0)}% · ${r.layer}`,
        detail: r.drawer.tags.join(', '),
        content: r.drawer.content,
      }));

    const picked = await vscode.window.showQuickPick(items, {
      title: `Recall: ${query}`,
      matchOnDescription: true,
      matchOnDetail: true,
    });
    if (picked === undefined) {
      return;
    }
    const doc = await vscode.workspace.openTextDocument({
      content: picked.content,
    });
    await vscode.window.showTextDocument(doc, { preview: true });
  } catch (err) {
    reportError('Trusty Memory: recall failed', err);
  }
}

/**
 * Trigger `trusty-memory cursor sync` for the current workspace.
 *
 * Why: Backs `Trusty Memory: Sync Cursor Rules`. The daemon HTTP API has no
 * cursor-sync endpoint, so the extension shells out to the CLI, which writes
 * `.cursor/rules/trusty-memory.mdc`.
 * What: Runs the configured binary with `cursor sync --palace <name> --dir
 * <workspace>`; reports stdout/stderr on completion.
 * Test: Manual; CLI behavior covered by `cargo test` cursor tests.
 */
async function syncCursorRules(): Promise<void> {
  const folders = vscode.workspace.workspaceFolders;
  if (folders === undefined || folders.length === 0) {
    void vscode.window.showWarningMessage(
      'Trusty Memory: open a workspace folder before syncing Cursor rules.',
    );
    return;
  }
  const { palace, binaryPath } = readConfig();
  const cwd = folders[0].uri.fsPath;

  await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: `Trusty Memory: syncing Cursor rules for "${palace}"…`,
    },
    () =>
      new Promise<void>((resolve) => {
        execFile(
          binaryPath,
          ['cursor', 'sync', '--palace', palace, '--dir', cwd],
          { cwd, timeout: 30_000 },
          (error, stdout, stderr) => {
            if (error !== null) {
              reportError(
                'Trusty Memory: cursor sync failed',
                new Error(stderr.trim().length > 0 ? stderr : error.message),
              );
            } else {
              const summary =
                stdout.trim().split('\n').pop() ?? 'Cursor rules synced.';
              void vscode.window.showInformationMessage(
                `Trusty Memory: ${summary}`,
              );
            }
            resolve();
          },
        );
      }),
  );
}

/**
 * Refresh the status bar item from the active palace.
 *
 * Why: The status bar shows the active palace name + memory count and clicks
 * through to the sidebar.
 * What: Reads the tree's cached active palace and updates the item text.
 * Test: Manual; refreshed on every tree refresh event.
 */
function updateStatusBar(
  item: vscode.StatusBarItem,
  tree: PalaceTreeProvider,
): void {
  const active = tree.activePalace();
  if (active === undefined) {
    const { palace } = readConfig();
    item.text = `$(database) ${palace}`;
    item.tooltip = 'Trusty Memory — daemon not reachable. Click to open.';
  } else {
    item.text = `$(database) ${active.name}: ${active.drawer_count}`;
    item.tooltip = `Trusty Memory — ${active.drawer_count} memories in "${active.name}". Click to open.`;
  }
  item.show();
}

/**
 * Extension entry point — register commands, the view, and the status bar.
 *
 * Why: VS Code calls `activate` once on first use of any contributed command
 * or view; all disposables registered here are cleaned up on shutdown.
 * What: Wires the `PalaceTreeProvider`, three palette commands, the refresh
 * command, and the click-to-open status bar item.
 * Test: Manual in the Extension Development Host.
 */
export function activate(context: vscode.ExtensionContext): void {
  const tree = new PalaceTreeProvider();

  const view = vscode.window.createTreeView('trustyMemory.palaces', {
    treeDataProvider: tree,
  });

  const statusBar = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Left,
    100,
  );
  statusBar.command = 'trustyMemory.palaces.focus';
  updateStatusBar(statusBar, tree);

  context.subscriptions.push(
    view,
    statusBar,
    tree.onDidRefresh(() => updateStatusBar(statusBar, tree)),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('trustyMemory')) {
        tree.refresh();
      }
    }),
    vscode.commands.registerCommand('trustyMemory.storeSelection', () =>
      storeSelection(tree),
    ),
    vscode.commands.registerCommand('trustyMemory.recall', () => recall()),
    vscode.commands.registerCommand('trustyMemory.syncCursorRules', () =>
      syncCursorRules(),
    ),
    vscode.commands.registerCommand('trustyMemory.refreshSidebar', () =>
      tree.refresh(),
    ),
  );

  // Populate the tree (and therefore the status bar) on activation.
  tree.refresh();
}

/**
 * Extension teardown hook.
 *
 * Why: Required by the VS Code API surface.
 * What: No-op — every disposable is owned by `context.subscriptions`.
 * Test: N/A (side-effect free).
 */
export function deactivate(): void {
  // Intentionally empty.
}
