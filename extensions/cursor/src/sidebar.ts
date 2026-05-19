// Why: The sidebar panel must show palaces and, for the active palace, its
// top-15 memories — a two-level tree that VS Code renders via a
// `TreeDataProvider`. Keeping it in its own module isolates the tree logic
// from command wiring in `extension.ts`.
// What: `PalaceTreeProvider` implements `vscode.TreeDataProvider<TreeNode>`:
// root nodes are palaces, child nodes are drawers of the active palace.
// Test: Manual against a live daemon; the data layer is `api.ts`.

import * as vscode from 'vscode';
import { readConfig } from './config';
import {
  Drawer,
  PalaceInfo,
  TrustyMemoryClient,
  TrustyMemoryError,
} from './api';

/** Maximum drawers shown under a palace node (L1 budget). */
const TOP_DRAWERS = 15;

/**
 * A palace node in the tree.
 *
 * Why: Discriminated union member so `getChildren`/`getTreeItem` can branch
 * type-safely without `any`.
 * What: Carries the full `PalaceInfo` payload.
 * Test: Constructed in `getChildren`.
 */
interface PalaceNode {
  readonly kind: 'palace';
  readonly palace: PalaceInfo;
}

/**
 * A drawer (memory) node in the tree.
 *
 * Why: Leaf node under a palace; carries the drawer for tooltip rendering.
 * What: Holds the owning palace name plus the `Drawer`.
 * Test: Constructed in `getChildren`.
 */
interface DrawerNode {
  readonly kind: 'drawer';
  readonly palaceName: string;
  readonly drawer: Drawer;
}

/**
 * A placeholder node used to surface an error or empty state in the tree.
 *
 * Why: Throwing from `getChildren` produces an opaque "no items"; an explicit
 * message node tells the user what went wrong (e.g. daemon down).
 * What: Carries a label and an optional codicon id.
 * Test: Constructed in the `getChildren` catch branch.
 */
interface MessageNode {
  readonly kind: 'message';
  readonly label: string;
  readonly icon: string;
}

/** Union of every node the tree can render. */
export type TreeNode = PalaceNode | DrawerNode | MessageNode;

/** First line of `text`, trimmed and length-capped for a tree label. */
function summarize(text: string, max: number): string {
  const firstLine = text.split('\n', 1)[0].trim();
  return firstLine.length > max
    ? `${firstLine.slice(0, max - 1)}…`
    : firstLine;
}

/**
 * Tree data provider for the Trusty Memory sidebar panel.
 *
 * Why: VS Code drives sidebar rendering through this interface; it also owns
 * the "active palace" selection so the status bar can read it.
 * What: Root level lists palaces; expanding a palace lists its top-15 drawers.
 * `refresh()` re-fetches and fires the change event.
 * Test: Manual; underlying HTTP covered by `cargo test` on the daemon.
 */
export class PalaceTreeProvider
  implements vscode.TreeDataProvider<TreeNode>
{
  private readonly changeEmitter =
    new vscode.EventEmitter<TreeNode | undefined>();

  public readonly onDidChangeTreeData: vscode.Event<TreeNode | undefined> =
    this.changeEmitter.event;

  /** Cached palace list, populated on each root-level `getChildren`. */
  private palaces: readonly PalaceInfo[] = [];

  /**
   * Fired after a refresh so the status bar can re-render with fresh counts.
   *
   * Why: The status bar shows the active palace's memory count and must stay
   * in sync with the tree without polling.
   * What: A void event raised at the end of every successful root refresh.
   * Test: Wired in `extension.ts`.
   */
  private readonly refreshEmitter = new vscode.EventEmitter<void>();
  public readonly onDidRefresh: vscode.Event<void> =
    this.refreshEmitter.event;

  /**
   * Force the tree to re-fetch from the daemon.
   *
   * Why: Backs the sidebar refresh button and post-store updates.
   * What: Fires `onDidChangeTreeData` with `undefined` to invalidate the root.
   * Test: Triggered by the `trustyMemory.refreshSidebar` command.
   */
  public refresh(): void {
    this.changeEmitter.fire(undefined);
  }

  /**
   * Return the currently active palace, if it exists in the daemon.
   *
   * Why: The status bar needs the active palace's name and drawer count.
   * What: Looks up the configured/derived palace name in the cached list.
   * Test: Returns `undefined` before the first refresh or when absent.
   */
  public activePalace(): PalaceInfo | undefined {
    const name = readConfig().palace;
    return this.palaces.find((p) => p.id === name || p.name === name);
  }

  /**
   * Build the `TreeItem` rendered for a node.
   *
   * Why: Required by `TreeDataProvider`; maps our union to VS Code visuals.
   * What: Palaces are collapsible with a count description; drawers are leaves
   * with an importance-derived icon and a content tooltip.
   * Test: Visual verification in the Extension Host.
   */
  public getTreeItem(node: TreeNode): vscode.TreeItem {
    switch (node.kind) {
      case 'palace': {
        const item = new vscode.TreeItem(
          node.palace.name,
          vscode.TreeItemCollapsibleState.Collapsed,
        );
        item.description = `${node.palace.drawer_count} memories`;
        item.iconPath = new vscode.ThemeIcon('database');
        item.tooltip = node.palace.description ?? node.palace.name;
        item.contextValue = 'trustyMemory.palace';
        return item;
      }
      case 'drawer': {
        const item = new vscode.TreeItem(
          summarize(node.drawer.content, 80),
          vscode.TreeItemCollapsibleState.None,
        );
        item.description = node.drawer.tags.join(', ');
        item.iconPath = new vscode.ThemeIcon(
          node.drawer.importance >= 0.7 ? 'star-full' : 'note',
        );
        item.tooltip = new vscode.MarkdownString(
          `**Importance:** ${node.drawer.importance.toFixed(2)}\n\n` +
            node.drawer.content,
        );
        item.contextValue = 'trustyMemory.drawer';
        return item;
      }
      case 'message': {
        const item = new vscode.TreeItem(
          node.label,
          vscode.TreeItemCollapsibleState.None,
        );
        item.iconPath = new vscode.ThemeIcon(node.icon);
        return item;
      }
    }
  }

  /**
   * Resolve the children of `node` (or the root when `node` is undefined).
   *
   * Why: Drives the two-level palace/drawer tree.
   * What: Root -> palace nodes; palace node -> its top-15 drawers; drawer
   * nodes are leaves. Errors become a single `MessageNode`.
   * Test: Manual against a live daemon.
   */
  public async getChildren(node?: TreeNode): Promise<TreeNode[]> {
    if (node === undefined) {
      return this.rootChildren();
    }
    if (node.kind === 'palace') {
      return this.drawerChildren(node.palace);
    }
    return [];
  }

  /** Fetch palaces for the root level, caching them for `activePalace`. */
  private async rootChildren(): Promise<TreeNode[]> {
    try {
      const { httpPort } = readConfig();
      const client = TrustyMemoryClient.fromConfig(httpPort);
      this.palaces = await client.listPalaces();
      this.refreshEmitter.fire();
      if (this.palaces.length === 0) {
        return [
          {
            kind: 'message',
            label: 'No palaces yet — store a memory to create one.',
            icon: 'info',
          },
        ];
      }
      return this.palaces.map((palace) => ({ kind: 'palace', palace }));
    } catch (err) {
      this.palaces = [];
      this.refreshEmitter.fire();
      const message =
        err instanceof TrustyMemoryError
          ? err.message
          : 'Unexpected error loading palaces.';
      return [{ kind: 'message', label: message, icon: 'error' }];
    }
  }

  /** Fetch the top-15 drawers for a palace node. */
  private async drawerChildren(palace: PalaceInfo): Promise<TreeNode[]> {
    try {
      const { httpPort } = readConfig();
      const client = TrustyMemoryClient.fromConfig(httpPort);
      const drawers = await client.listDrawers(palace.id, TOP_DRAWERS);
      if (drawers.length === 0) {
        return [
          { kind: 'message', label: 'No memories.', icon: 'info' },
        ];
      }
      return drawers.map((drawer) => ({
        kind: 'drawer',
        palaceName: palace.name,
        drawer,
      }));
    } catch (err) {
      const message =
        err instanceof TrustyMemoryError
          ? err.message
          : 'Unexpected error loading memories.';
      return [{ kind: 'message', label: message, icon: 'error' }];
    }
  }
}
