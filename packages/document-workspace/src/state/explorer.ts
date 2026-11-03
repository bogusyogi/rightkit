// Bridge for the Tauri explorer backend (src-tauri/src/commands/explorer.rs). Follows the same
// conventions as library.ts: lazy invoke import so the Tauri API loads only in the native app, and
// no-op returns outside Tauri.
import { inTauri } from "@rightkit/document-core/state/tauriBridge";

export type ExplorerLocationKind =
  | "home"
  | "desktop"
  | "documents"
  | "downloads"
  | "icloud"
  | "onedrive"
  | "volume"
  | "root";

export interface ExplorerLocation {
  id: string;
  label: string;
  kind: ExplorerLocationKind | string;
  dir: string;
}

export interface ExplorerEntry {
  name: string;
  /** "dir" | a supported format id (e.g. "md", "pdf") | "other". */
  kind: string;
  /** Unix epoch milliseconds; null when unknown. */
  modified: number | null;
  /** Bytes; null for directories. */
  size: number | null;
  /** Dirs: canonical dir path. Supported files: broker-granted path (openable via `open`). */
  path: string;
}

export interface ExplorerListing {
  entries: ExplorerEntry[];
  truncated: boolean;
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export async function explorerLocations(): Promise<ExplorerLocation[]> {
  if (!inTauri()) return [];
  return invoke<ExplorerLocation[]>("explorer_locations");
}

/** `keep` is the NAME of one child the backend must list even when its cap would cut it. */
export async function explorerListDir(dir: string, keep?: string): Promise<ExplorerListing> {
  if (!inTauri()) return { entries: [], truncated: false };
  return invoke<ExplorerListing>("explorer_list_dir", keep === undefined ? { dir } : { dir, keep });
}

export async function explorerWatchDir(dir: string): Promise<void> {
  if (!inTauri()) return;
  await invoke("explorer_watch_dir", { dir });
}

export async function explorerUnwatchDir(dir: string): Promise<void> {
  if (!inTauri()) return;
  await invoke("explorer_unwatch_dir", { dir });
}

/** OS-level reveal (Finder select / Explorer /select) for dirs and unsupported files — unlike
 *  `revealInFileExplorer` this does not need a broker grant; it only shows the file manager. */
export async function explorerReveal(path: string): Promise<void> {
  if (!inTauri()) return;
  await invoke("explorer_reveal", { path });
}

/** `explorer:changed` events fire {dir} when a watched directory changes; relist that node. */
export async function onExplorerChanged(cb: (dir: string) => void): Promise<() => void> {
  if (!inTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  const un = await listen<{ dir: string }>("explorer:changed", (e) => cb(e.payload.dir));
  return () => {
    try {
      un();
    } catch {
      /* bridge teardown race */
    }
  };
}
