import {
  baseName,
  formatFromPath,
  type DocFormat,
  type LoadedDocument,
  type ViewMode,
  type ViewMeta,
} from "@rightkit/document-core/documents/types";
import { inTauri, isBrowserQaMode } from "@rightkit/document-core/state/tauriBridge";
import { hostAppName } from "../appName";

export interface FileEntry {
  /** Opaque backend token for a known file. This is not a filesystem path. */
  token: string;
  displayName: string;
  /** Location metadata for identity/display only. Opening still exchanges token for a grant. */
  displayPath?: string;
  format: DocFormat;
  /** Last-opened time in epoch ms (recent_files.opened_at × 1000). Undefined when unavailable. */
  openedAt?: number;
}

export interface OpenFileEntry {
  /** Authorized real filesystem path for an already-open file. */
  path: string;
  displayName: string;
  format: DocFormat;
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

interface RawEntry {
  path: string;
  display_name: string;
  display_path?: string;
  format: DocFormat;
  /** Unix epoch seconds; absent from older persisted payloads. */
  opened_at?: number;
}

function map(rows: RawEntry[]): FileEntry[] {
  return rows.map((r) => ({
    token: r.path,
    displayName: r.display_name,
    displayPath: r.display_path,
    format: r.format,
    openedAt: r.opened_at ? r.opened_at * 1000 : undefined,
  }));
}

export async function listRecent(): Promise<FileEntry[]> {
  if (isBrowserQaMode()) {
    const now = Date.now();
    return [
      {
        token: `D:\\${hostAppName()} QA\\source-notes.md`,
        displayName: "source-notes.md",
        format: "md",
        openedAt: now - 12 * 60_000,
      },
      {
        token: `D:\\${hostAppName()} QA\\local-preview.html`,
        displayName: "local-preview.html",
        format: "html",
        openedAt: now - 3 * 3_600_000,
      },
      {
        token: `D:\\${hostAppName()} QA\\scratch.txt`,
        displayName: "scratch.txt",
        format: "txt",
        openedAt: now - 26 * 3_600_000,
      },
    ];
  }
  if (!inTauri()) return [];
  return map(await invoke<RawEntry[]>("list_recent", { limit: 20 }));
}

export async function clearRecent(): Promise<void> {
  if (isBrowserQaMode() || !inTauri()) return;
  await invoke("clear_recent");
}

export async function togglePin(entry: OpenFileEntry): Promise<boolean> {
  if (isBrowserQaMode()) return true;
  if (!inTauri()) return false;
  return invoke("toggle_pin", {
    path: entry.path,
    displayName: entry.displayName,
    format: entry.format,
  });
}

export async function isPinnedPath(path: string): Promise<boolean> {
  if (isBrowserQaMode()) return false;
  if (!inTauri()) return false;
  return invoke<boolean>("is_pinned_path", { path });
}

export interface PinEntry {
  path: string;
  displayName: string;
  format: string;
  /** "file" | "dir" — directory pins power the explorer sidebar's pinned folders. */
  kind: "file" | "dir";
}

interface RawPinEntry {
  path: string;
  display_name: string;
  format: string;
  kind: "file" | "dir";
}

/** All pins — files AND directories (dirs first, then files, in pin order). */
export async function listPins(): Promise<PinEntry[]> {
  if (isBrowserQaMode()) return [];
  if (!inTauri()) return [];
  const rows = await invoke<RawPinEntry[]>("list_pins");
  return rows.map((r) => ({
    path: r.path,
    displayName: r.display_name,
    format: r.format,
    kind: r.kind,
  }));
}

export async function togglePinDir(path: string, displayName: string): Promise<boolean> {
  if (isBrowserQaMode()) return true;
  if (!inTauri()) return false;
  return invoke("toggle_pin_dir", { path, displayName });
}

export async function recordRecent(doc: LoadedDocument): Promise<void> {
  if (isBrowserQaMode()) return;
  if (!inTauri()) return;
  await invoke("record_recent", {
    path: doc.path,
    displayName: baseName(doc.path),
    format: formatFromPath(doc.path),
  });
}

export async function saveViewState(
  path: string,
  lastView: ViewMode,
  viewMeta?: ViewMeta,
): Promise<void> {
  if (isBrowserQaMode()) return;
  if (!inTauri()) return;
  await invoke("save_view_state", {
    path,
    lastView,
    viewMeta: viewMeta ? JSON.stringify(viewMeta) : null,
  });
}
