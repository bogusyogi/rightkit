// RightKit's mandatory webview clients over the hub's real Tauri `invoke`:
//  - `native` (@rightkit/tauri): typed commands for the device id, logs and diagnostics. Licensing is
//    native and has its own client (`hub/src/lib/license.ts`).
//  - `logs` (@rightkit/logs): batched structured events to `rightkit_logs_write`, written into the
//    hub log (`hub.jsonl`) by the native side. Only the fields listed here ever reach disk.
//    Webview errors go through `installWindowCrashCapture` to `rightkit_crash_window_error`.
import { invoke } from "@tauri-apps/api/core";
import { createLogsClient, installWindowCrashCapture } from "@rightkit/logs";
import { createRightKitTauriClient, standardRightKitDeviceGetOrCreate } from "@rightkit/tauri";

export const native = createRightKitTauriClient({
  invoke,
  deviceGetOrCreate: standardRightKitDeviceGetOrCreate,
});

export const logs = createLogsClient({
  invoke,
  allowedFields: ["surface", "kind", "name", "source", "line", "count", "code", "duration_ms"],
});

/** This install's RightKit device id (not the devicelink pairing identity). */
export const deviceId = (): Promise<string> => native.device.getOrCreate() as Promise<string>;

let installed = false;

/** Sends uncaught errors and unhandled rejections to the native crash command, which logs the kind and place only, never the message. */
export function installWebviewLog(): void {
  if (installed) return;
  installed = true;
  installWindowCrashCapture({ invoke });
  window.addEventListener("pagehide", () => void logs.flush());
  logs.info("webview_started", { surface: "hub" });
  void deviceId().catch(() => logs.warn("device_id_unavailable", { surface: "hub" }));
}
