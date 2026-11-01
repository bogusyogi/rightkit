// The native shell for @rightkit/react-shell. Window controls, external links and native menu actions all go through
// @rightkit/tauri-shell (the renderer side of the rightkit-tauri-shell crate); the renderer never imports
// @tauri-apps/api/window, and the capability file grants no core:window permission.
import type { AppShellBridge } from "@rightkit/react-shell";
import { createShell } from "@rightkit/tauri-shell";
import { isTauri } from "./host";

// createShell() throws outside a Tauri webview, so a plain browser preview has no shell and the frame renders without native controls.
export const shell = isTauri() ? createShell() : undefined;
export const shellBridge: AppShellBridge | undefined = shell?.bridge();

/** Tell the host the renderer is mounted: the shell reveals the main window (tauri.conf.json starts it hidden). No-op outside Tauri. */
export async function shellReady(): Promise<void> {
  await shell?.ready();
}
