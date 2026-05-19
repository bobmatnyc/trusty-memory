// Why: Several commands and the sidebar all need the same two settings
// (`httpPort`, `defaultPalace`) plus the workspace-folder fallback logic;
// duplicating `getConfiguration` calls would invite inconsistency.
// What: A thin typed accessor over `vscode.workspace.getConfiguration`.
// Test: Exercised by every command; logic is pure aside from VS Code reads.

import * as path from 'path';
import * as vscode from 'vscode';

/** Configuration section namespace declared in package.json. */
const SECTION = 'trustyMemory';

/**
 * Resolved extension settings.
 *
 * Why: Bundles the two daemon-targeting settings so callers pass one object.
 * What: `httpPort` is `null` when discovery should be used; `palace` is always
 * resolved to a concrete name (config value or workspace-folder fallback).
 * Test: Returned by `readConfig`.
 */
export interface TrustyMemoryConfig {
  readonly httpPort: number | null;
  readonly palace: string;
  readonly binaryPath: string;
}

/**
 * Derive the default palace name from the first workspace folder.
 *
 * Why: Most users want one palace per project; defaulting to the folder name
 * removes a configuration step.
 * What: Returns the basename of the first workspace folder, or `'default'`
 * when no folder is open.
 * Test: Pure function over the workspace folders array.
 */
function workspacePalace(): string {
  const folders = vscode.workspace.workspaceFolders;
  if (folders === undefined || folders.length === 0) {
    return 'default';
  }
  return path.basename(folders[0].uri.fsPath);
}

/**
 * Read and normalize the extension configuration.
 *
 * Why: Provides command handlers a single, fully-resolved settings object so
 * they never re-implement the workspace-folder fallback.
 * What: Reads `trustyMemory.*`; coerces `httpPort` to `number | null` and
 * resolves `defaultPalace` to a concrete name.
 * Test: Behavior verified manually via the Settings UI.
 */
export function readConfig(): TrustyMemoryConfig {
  const cfg = vscode.workspace.getConfiguration(SECTION);

  const rawPort = cfg.get<number | null>('httpPort', null);
  const httpPort =
    typeof rawPort === 'number' && Number.isFinite(rawPort) && rawPort > 0
      ? Math.trunc(rawPort)
      : null;

  const rawPalace = cfg.get<string | null>('defaultPalace', null);
  const palace =
    typeof rawPalace === 'string' && rawPalace.trim().length > 0
      ? rawPalace.trim()
      : workspacePalace();

  const rawBinary = cfg.get<string>('binaryPath', 'trusty-memory');
  const binaryPath =
    rawBinary.trim().length > 0 ? rawBinary.trim() : 'trusty-memory';

  return { httpPort, palace, binaryPath };
}
