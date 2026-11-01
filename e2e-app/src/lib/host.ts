// Single IPC seam to the Tauri shell. There is no browser stand-in: UI behaviour is verified
// against the real app (rightkit-qa native scenarios), never against an in-page mock.
import { invoke as tauriInvoke } from "@tauri-apps/api/core";

export const isTauri = (): boolean => "__TAURI_INTERNALS__" in globalThis;

export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) throw new Error(`"${command}" needs the Tauri shell; install a development build with \`right-release dev\``);
  return tauriInvoke<T>(command, args);
}
