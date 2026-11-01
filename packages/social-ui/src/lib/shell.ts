import { createShell } from "@rightkit/tauri-shell";

/** The native shell bridge (window controls, external links, menu actions, native theme). */
export const shell = createShell();
export const shellBridge = shell.bridge();
