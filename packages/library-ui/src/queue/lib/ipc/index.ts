// The queue's Tauri bindings. The queue components are ScrapeRight's and still call its commands by
// name (the queue is left here until a queue UI package exists); the library list and reader above
// them take a host `LibrarySource` instead. The QA mock that used to ship here is the app's.
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import { getCurrentWebview as tauriCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow as tauriCurrentWindow } from "@tauri-apps/api/window";
import { open as tauriOpenDialog, save as tauriSaveDialog } from "@tauri-apps/plugin-dialog";
import { createShell, type Shell } from "@rightkit/tauri-shell";

export const invoke = tauriInvoke;
export const listen = tauriListen;
export const getCurrentWebview = tauriCurrentWebview;
export const getCurrentWindow = tauriCurrentWindow;
export const open = tauriOpenDialog;
export const save = tauriSaveDialog;

let shell: Shell | null = null;

/** RightKit shell (window actions, clipboard, notifications, engine calls) over this module's `invoke`/`listen`. */
export function getShell(): Shell {
  shell ??= createShell({
    invoke: <T>(command: string, args?: Record<string, unknown>) => invoke<T>(command, args),
    listen: <T>(event: string, handler: (payload: T) => void) => listen<T>(event, (e) => handler(e.payload)),
  });
  return shell;
}
