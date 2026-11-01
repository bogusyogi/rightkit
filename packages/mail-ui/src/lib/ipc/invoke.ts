import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { browserQaEnabled } from "./qaGate";
import { trace } from "../telemetry";

// Commands whose IPC call we intentionally do NOT trace. `append_diag` is the
// legacy FE diagnostics path and is already a separate log file.
const IPC_TRACE_SKIP = new Set<string>(["append_diag"]);

// Which argument keys reach the log is the closed `ALLOWED_FIELDS` list of telemetry.ts, enforced by
// the shared logs client (@rightkit/logs `allowedFields`, which also sanitizes the values). The
// call's arguments are passed to `trace` as they are; there is no second allowlist here.

export async function invoke<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (!IPC_TRACE_SKIP.has(command)) {
    trace("ipc", `ipc.${command}`, args);
  }
  if (typeof import.meta.env !== "undefined" && import.meta.env.DEV && browserQaEnabled()) {
    const { qaInvoke } = await import("./qaMock");
    return qaInvoke<T>(command, args);
  }
  return tauriInvoke<T>(command, args);
}
