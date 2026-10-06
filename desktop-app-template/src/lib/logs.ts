import { createLogsClient } from "@rightkit/logs";
import { invoke } from "./host";

// Closed, content-free field allow-list. Never add free-text fields.
export const logs = createLogsClient({
  invoke,
  allowedFields: ["surface", "kind", "duration_ms", "count", "code"],
});

let installed = false;
export function installLogs(): void {
  if (installed) return;
  installed = true;
  logs.info("app_started", { surface: "renderer" });
  window.addEventListener("error", () => logs.error("renderer_error", { surface: "renderer", kind: "uncaught" }));
  window.addEventListener("unhandledrejection", () => logs.error("renderer_error", { surface: "renderer", kind: "rejection" }));
}
