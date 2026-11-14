/**
 * The one module in this package that imports `@tauri-apps/api` and `@tauri-apps/plugin-process`
 * (desktop contract R31, O22). The renderer reaches the native shell only through these names, so a
 * later host swap replaces this file and nothing else. Lazy imports stay lazy: the window, app and
 * process APIs load on first use. The dialog plugin's `open` is not re-exported here: this package
 * does not depend on `@tauri-apps/plugin-dialog`.
 */
export { invoke } from "@tauri-apps/api/core";
export { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** The running build's version, from the native shell. */
export async function getAppVersion(): Promise<string> {
  const { getVersion } = await import("@tauri-apps/api/app");
  return getVersion();
}

/** Close this window. Rejects outside the shell. */
export async function closeCurrentWindow(): Promise<void> {
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  await getCurrentWindow().close();
}

/** Set the dock or taskbar badge; `undefined` clears it. Rejects outside the shell. */
export async function setBadgeCount(count: number | undefined): Promise<void> {
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  await getCurrentWindow().setBadgeCount(count);
}

/** Restart the app. */
export async function relaunchApp(): Promise<void> {
  const { relaunch } = await import("@tauri-apps/plugin-process");
  await relaunch();
}
