import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { acquireResource, updateResource } from "@rightkit/document-core/app/resourceCensus";

/** Rectangle for the native PDFKit view, in window CSS pixels. */
export interface NativePdfFrame {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface NativePdfOutlineNode {
  title: string;
  page: number | null;
  children: NativePdfOutlineNode[];
}

export type NativePdfEditingAction =
  | { kind: "annotation"; mode: "freeText" | "ink" | "highlight" | "stamp" }
  | { kind: "text" }
  | { kind: "stamp"; file: File }
  | { kind: "ocr" }
  | { kind: "ocr-document" };

export interface NativePdfState {
  owner: string;
  page: number;
  numPages: number;
  scale: number;
  fit: "width" | "page" | null;
  rotation: number;
  /** Native panel state survives pane eviction & parked-reader reuse. */
  panel?: "outline" | "thumbs" | null;
  locked: boolean;
  hasOutline: boolean;
  outline: NativePdfOutlineNode[];
}

export type NativePdfResourcePhase = "active" | "parked" | "released";

export interface NativePdfResourceEvent {
  owner: string;
  path: string;
  identity: string | null;
  generation: number;
  phase: NativePdfResourcePhase;
  reason: string;
  activeCount: number;
  parkedCount: number;
  releasedCount: number;
}

export interface NativePdfOpenArgs {
  owner: string;
  path: string;
  frame: NativePdfFrame;
  readerRgb?: [number, number, number];
  initialView: {
    page?: number;
    scale?: number;
    fit?: "width" | "page" | null;
    rotation?: number;
  };
  password?: string;
}

export interface NativePdfUpdateArgs {
  owner: string;
  frame?: NativePdfFrame;
  readerRgb?: [number, number, number];
  page?: number;
  scale?: number;
  /** An empty string clears a fit mode. */
  fit?: "width" | "page" | "";
  rotation?: number;
  /** Empty string means no side panel. */
  panel?: "" | "thumbs" | "outline";
  visible?: boolean;
}

export interface NativePdfFindArgs {
  owner: string;
  query?: string;
  matchCase?: boolean;
  wholeWord?: boolean;
  direction?: 1 | -1;
  index?: number;
  clear?: boolean;
}

export interface NativePdfSearchResult {
  count: number;
  activeIndex: number;
}

export interface NativePdfKeyEvent {
  /** Native key events may omit owner because window has one native PDF host. */
  owner?: string;
  key: string;
  code: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

type QueueTask<T> = () => Promise<T>;

// The shell's rightkit_pdf_* commands and events (rightkit-tauri-shell pdf_host). Failures cross
// IPC as `{ code, message }`, not as an Error or a string.
const CMD = {
  open: "rightkit_pdf_open",
  update: "rightkit_pdf_update",
  state: "rightkit_pdf_state",
  close: "rightkit_pdf_close",
  suspend: "rightkit_pdf_suspend",
  find: "rightkit_pdf_find",
  print: "rightkit_pdf_print",
} as const;
const EVENT_STATE = "rightkit://pdf-state";
const EVENT_RESOURCE = "rightkit://pdf-resource";
const EVENT_KEY = "rightkit://pdf-key";

export interface NativePdfError extends Error {
  /** The shell's stable code, for example `unsupported`, `not_open`, `stale_owner`. */
  code?: string;
}

function toError(cause: unknown): NativePdfError {
  if (cause instanceof Error) return cause;
  if (cause && typeof cause === "object" && typeof (cause as { message?: unknown }).message === "string") {
    const error: NativePdfError = new Error((cause as { message: string }).message);
    const code = (cause as { code?: unknown }).code;
    if (typeof code === "string") error.code = code;
    return error;
  }
  return new Error(String(cause));
}

async function call<T>(command: string, args: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (cause) {
    throw toError(cause);
  }
}

// Native PDFKit views are window-owned. Serializing every lifecycle command prevents a queued
// StrictMode cleanup (or a document switch) from racing a newer open and reusing an old native view.
let queue: Promise<unknown> = Promise.resolve();
let resourceListener: Promise<() => void> | null = null;
let resourceEpoch = 0;
const resourceHandles = new Map<number, () => void>();

// PDFKit generations survive tab reactivation, unlike React owners. Keep this listener
// outside pane lifetime so native expiry and terminal-close events remain observable.
function ensureResourceListener(): Promise<unknown> {
  if (!resourceListener) {
    const epoch = resourceEpoch;
    resourceListener = listenNativePdfResource((event) => {
      if (epoch !== resourceEpoch) return;
      const release = resourceHandles.get(event.generation);
      if (event.phase === "released") {
        release?.();
        resourceHandles.delete(event.generation);
        return;
      }
      const kind = event.phase === "parked" ? "pdf-native-parked-reader" : "pdf-native-reader";
      if (release) {
        updateResource(release, { kind });
      } else {
        resourceHandles.set(
          event.generation,
          acquireResource({ kind, documentPath: event.path, format: "pdf" }),
        );
      }
    }).catch((error: unknown) => {
      if (epoch === resourceEpoch) resourceListener = null;
      throw error;
    });
  }
  return resourceListener;
}

function enqueue<T>(task: QueueTask<T>): Promise<T> {
  const run = queue.then(task, task);
  queue = run.then(
    () => undefined,
    () => undefined,
  );
  return run;
}

export function nativePdfOpen(args: NativePdfOpenArgs): Promise<NativePdfState> {
  return enqueue(async () => {
    await ensureResourceListener();
    return call<NativePdfState>(CMD.open, { ...args });
  });
}

export function nativePdfUpdate(args: NativePdfUpdateArgs): Promise<NativePdfState> {
  return enqueue(() => call<NativePdfState>(CMD.update, { ...args }));
}

export function nativePdfState(owner: string): Promise<NativePdfState> {
  return enqueue(() => call<NativePdfState>(CMD.state, { owner }));
}

export type NativePdfCloseReason =
  | "terminal-close"
  | "reload"
  | "edit-handoff"
  | "fallback"
  | "open-replaced";

export function nativePdfClose(
  owner: string,
  path?: string,
  reason: NativePdfCloseReason = "terminal-close",
): Promise<void> {
  return enqueue(() =>
    call<void>(CMD.close, {
      owner,
      ...(path === undefined ? {} : { path }),
      reason,
    }),
  );
}

/** Close any native reader for a path when its app tab is genuinely removed. */
export function nativePdfClosePath(
  path: string,
  reason: NativePdfCloseReason = "terminal-close",
): Promise<void> {
  return nativePdfClose("", path, reason);
}

export function nativePdfSuspend(owner: string, path: string): Promise<void> {
  return enqueue(() => call<void>(CMD.suspend, { owner, path }));
}

export function nativePdfFind(args: NativePdfFindArgs): Promise<NativePdfSearchResult> {
  return enqueue(() =>
    call<{ count: number; activeIndex: number | null }>(CMD.find, { ...args }).then((result) => ({
      count: result.count,
      activeIndex: result.activeIndex ?? -1,
    })),
  );
}

export function nativePdfPrint(owner: string): Promise<boolean> {
  return enqueue(() => call<boolean>(CMD.print, { owner }));
}

export async function listenNativePdfState(
  onState: (state: NativePdfState) => void,
): Promise<() => void> {
  return listen<NativePdfState>(EVENT_STATE, (event) => onState(event.payload));
}

export async function listenNativePdfResource(
  onResource: (event: NativePdfResourceEvent) => void,
): Promise<() => void> {
  return listen<NativePdfResourceEvent>(
    EVENT_RESOURCE,
    (event) => onResource(event.payload),
    { target: { kind: "WebviewWindow", label: getCurrentWindow().label } },
  );
}

export async function listenNativePdfKey(
  onKey: (event: NativePdfKeyEvent) => void,
): Promise<() => void> {
  return listen<NativePdfKeyEvent>(EVENT_KEY, (event) => onKey(event.payload));
}

/** Release frontend observation on HMR/test reset; it does not send a native command. */
export function resetNativePdfBridgeForTests(): void {
  queue = Promise.resolve();
  resourceEpoch++;
  void resourceListener?.then((unlisten) => unlisten()).catch(() => {});
  resourceListener = null;
  for (const release of resourceHandles.values()) release();
  resourceHandles.clear();
}

if (import.meta.hot) import.meta.hot.dispose(resetNativePdfBridgeForTests);
