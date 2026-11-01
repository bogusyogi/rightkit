// Bridge for the Tauri explorer backend (src-tauri/src/commands/explorer.rs). Follows the same
// conventions as library.ts: lazy invoke import so vitest never touches the Tauri API, browser-QA
// stubs, and no-op returns outside Tauri.
import { inTauri, isBrowserQaMode } from "@rightkit/document-core/state/tauriBridge";
import { hostAppName } from "../appName.js";

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

const QA_ROOT = `D:\\${hostAppName()} QA`;

const QA_LOCATIONS: ExplorerLocation[] = [
  { id: "home", label: "Home", kind: "home", dir: QA_ROOT },
  { id: "desktop", label: "Desktop", kind: "desktop", dir: `${QA_ROOT}\\Desktop` },
  { id: "documents", label: "Documents", kind: "documents", dir: QA_ROOT },
  { id: "downloads", label: "Downloads", kind: "downloads", dir: `${QA_ROOT}\\Downloads` },
  { id: "c", label: "Windows (C:)", kind: "volume", dir: "C:\\" },
];

const QA_DIRS: Record<string, ExplorerEntry[]> = {
  [QA_ROOT]: [
    { name: "Notes", kind: "dir", modified: null, size: null, path: `${QA_ROOT}\\Notes` },
    { name: "Specs", kind: "dir", modified: null, size: null, path: `${QA_ROOT}\\Specs` },
    {
      name: "source-notes.md",
      kind: "md",
      modified: 1_725_000_000_000,
      size: 4210,
      path: `${QA_ROOT}\\source-notes.md`,
    },
    {
      name: "local-preview.html",
      kind: "html",
      modified: 1_724_000_000_000,
      size: 9182,
      path: `${QA_ROOT}\\local-preview.html`,
    },
    {
      name: "scratch.txt",
      kind: "txt",
      modified: 1_723_000_000_000,
      size: 812,
      path: `${QA_ROOT}\\scratch.txt`,
    },
    {
      name: "installer.msi",
      kind: "other",
      modified: 1_722_000_000_000,
      size: 5_242_880,
      path: `${QA_ROOT}\\installer.msi`,
    },
  ],
  [`${QA_ROOT}\\Notes`]: [
    {
      name: "roadmap.md",
      kind: "md",
      modified: 1_725_100_000_000,
      size: 1204,
      path: `${QA_ROOT}\\Notes\\roadmap.md`,
    },
  ],
  [`${QA_ROOT}\\Specs`]: [],
  [`${QA_ROOT}\\Desktop`]: [],
  [`${QA_ROOT}\\Downloads`]: [],
  "C:\\": [],
};

export async function explorerLocations(): Promise<ExplorerLocation[]> {
  if (isBrowserQaMode()) return QA_LOCATIONS;
  if (!inTauri()) return [];
  return invoke<ExplorerLocation[]>("explorer_locations");
}

/** `keep` is the NAME of one child the backend must list even when its cap would cut it. */
export async function explorerListDir(dir: string, keep?: string): Promise<ExplorerListing> {
  if (isBrowserQaMode()) return { entries: QA_DIRS[dir] ?? [], truncated: false };
  if (!inTauri()) return { entries: [], truncated: false };
  return invoke<ExplorerListing>("explorer_list_dir", keep === undefined ? { dir } : { dir, keep });
}

export async function explorerWatchDir(dir: string): Promise<void> {
  if (isBrowserQaMode() || !inTauri()) return;
  await invoke("explorer_watch_dir", { dir });
}

export async function explorerUnwatchDir(dir: string): Promise<void> {
  if (isBrowserQaMode() || !inTauri()) return;
  await invoke("explorer_unwatch_dir", { dir });
}

/** OS-level reveal (Finder select / Explorer /select) for dirs and unsupported files — unlike
 *  `revealInFileExplorer` this does not need a broker grant; it only shows the file manager. */
export async function explorerReveal(path: string): Promise<void> {
  if (isBrowserQaMode() || !inTauri()) return;
  await invoke("explorer_reveal", { path });
}

/** `explorer:changed` events fire {dir} when a watched directory changes; relist that node. */
export async function onExplorerChanged(cb: (dir: string) => void): Promise<() => void> {
  if (isBrowserQaMode() || !inTauri()) return () => {};
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
