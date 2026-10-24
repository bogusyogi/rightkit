// Single IPC seam to the Tauri host for the commands @rightkit/logs and the device client send.
// Window controls and engine calls do not use it: they go through @rightkit/tauri-shell (shell.ts).
// There is no browser stand-in: UI behaviour is verified against the real app (rightkit-qa native
// scenarios), never against an in-page mock.
import { isTauri } from "@rightkit/platform-ui";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";

export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) throw new Error(`"${command}" needs the Tauri shell; run \`pnpm tauri:dev\``);
  return tauriInvoke<T>(command, args);
}
