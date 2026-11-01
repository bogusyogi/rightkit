import { createLogsClient, type LogsInvoke } from "@rightkit/logs";
import { invoke } from "./tauri";

export const logs = createLogsClient({
  invoke: invoke as LogsInvoke,
  allowedFields: [
    "action",
    "backend",
    "code",
    "count",
    "duration_ms",
    "method",
    "model",
    "outcome",
    "source",
    "state",
    "status",
    "surface",
  ],
});
