// The one native shell client. Window controls, links, menu actions and theme mirroring all go through
// @rightkit/tauri-shell's own rk_shell_* commands, so the renderer needs no @tauri-apps/api/window import
// and no core:window permission. react-shell never imports Tauri; the app passes it this bridge.
import { isTauri } from "@rightkit/platform-ui";
import type { AppShellBridge } from "@rightkit/react-shell";
import { createShell, type Shell } from "@rightkit/tauri-shell";

let client: Shell | undefined;

/** The shell client (engine, window, menu ...). Throws outside the Tauri webview: there is no stand-in. */
export function getShell(): Shell {
  client ??= createShell();
  return client;
}

// Built once so the frame sees a stable object (it re-subscribes menu actions when the bridge changes).
// shell.bridge() is structurally an AppShellBridge; no adapter is needed.
export const shellBridge: AppShellBridge | undefined = isTauri() ? getShell().bridge() : undefined;
