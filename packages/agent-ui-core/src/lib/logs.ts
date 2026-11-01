import {
  createLogsClient,
  installWindowCrashCapture,
} from "@rightkit/logs";
import { invoke } from "@tauri-apps/api/core";

export const logs = createLogsClient({
  invoke,
  allowedFields: ["surface", "kind", "duration_ms", "count", "code"],
});

let installed = false;

export function installLogs(): void {
  if (installed) return;
  installed = true;
  logs.info("renderer_started", { surface: "renderer" });
  // Window error/unhandledrejection capture is RightKit's: scrubbed, rate-limited, and sent to the
  // native `rightkit_crash_window_error` command (rightkit-crash).
  installWindowCrashCapture({ invoke });
}
