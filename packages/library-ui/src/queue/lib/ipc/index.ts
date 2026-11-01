import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import { getCurrentWebview as tauriCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow as tauriCurrentWindow } from "@tauri-apps/api/window";
import { open as tauriOpenDialog, save as tauriSaveDialog } from "@tauri-apps/plugin-dialog";
import { createShell, type Shell } from "@rightkit/tauri-shell";
import type { Tab } from "../../components/NavTabs.js";
import type { JobInfo } from "../../contract.js";
import type * as QaBackend from "./qaMock.js";
import { qaModeEnabled } from "./qaGate.js";

export type QaInitialState = { tab: Tab; jobs: JobInfo[]; detailId: string | null };

// Static build-time gate. vite.config defines import.meta.env?.VITE_RIGHTKIT_QA as a literal,
// so in a release build QA_BUILD is the constant false, `isQaMode` below is the constant false, and
// the bundler drops qaMock and every `isQaMode` branch in the app (a const, not a reassigned let).
const QA_BUILD = import.meta.env?.VITE_RIGHTKIT_QA === "1";

let qa: typeof QaBackend | null = null;

// Live bindings. The Tauri backend is the default; initIpc swaps in the QA mock before first render
// (see main.tsx) and only in a QA build opened with ?qa=1.
/** QA mode: a QA build opened with `?qa=1`. A compile-time constant false in a release build. */
export const isQaMode: boolean = QA_BUILD && qaModeEnabled("1", window.location.search);
export let qaState = "queue-default";
export let invoke: typeof tauriInvoke = tauriInvoke;
export let listen: typeof tauriListen = tauriListen;
export let getCurrentWebview: typeof tauriCurrentWebview = tauriCurrentWebview;
export let getCurrentWindow: typeof tauriCurrentWindow = tauriCurrentWindow;
export let open: typeof tauriOpenDialog = tauriOpenDialog;
export let save: typeof tauriSaveDialog = tauriSaveDialog;

/** Call once before the app graph is imported. Resolves at once in a release build. */
export async function initIpc(search: string = window.location.search): Promise<void> {
  if (!isQaMode) return;
  const backend = await import("./qaMock.js");
  // QA-only platform override (`?qaPlatform=`); the QA entry is never part of a release build.
  const qaPlatform = await import("@rightkit/platform-ui/qa");
  qaPlatform.bootstrapQaPlatform({ query: search });
  qa = backend;
  qaState = backend.qaState;
  invoke = backend.qaInvoke as unknown as typeof tauriInvoke;
  listen = backend.qaListen as unknown as typeof tauriListen;
  getCurrentWebview = backend.qaCurrentWebview as unknown as typeof tauriCurrentWebview;
  getCurrentWindow = backend.qaCurrentWindow as unknown as typeof tauriCurrentWindow;
  open = backend.qaOpenDialog as unknown as typeof tauriOpenDialog;
  save = backend.qaOpenDialog as unknown as typeof tauriSaveDialog;
}

let shell: Shell | null = null;

/**
 * RightKit shell (window actions, clipboard, notifications, engine calls). It routes through this
 * module's `invoke`/`listen`, so QA mode reaches the same mock as every other command.
 */
export function getShell(): Shell {
  shell ??= createShell({
    invoke: <T>(command: string, args?: Record<string, unknown>) => invoke<T>(command, args),
    listen: <T>(event: string, handler: (payload: T) => void) =>
      listen<T>(event, (e) => handler(e.payload)),
  });
  return shell;
}

export function getQaInitialState(): QaInitialState {
  return qa ? qa.getQaInitialState() : { tab: "queue", jobs: [], detailId: null };
}
