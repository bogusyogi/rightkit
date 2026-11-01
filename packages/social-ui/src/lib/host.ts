// Single IPC seam to the Tauri shell. There is no browser stand-in: UI behaviour is verified
// against the real app, never against an in-page mock.
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { isTauri } from "@rightkit/platform-ui";
import { asShellError, type EngineCallError } from "@rightkit/tauri-shell";
import { shell } from "./shell";

export { isTauri };

export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) throw new Error(`"${command}" needs the Tauri shell; run \`pnpm tauri:dev\``);
  return tauriInvoke<T>(command, args);
}

/**
 * The typed rejection `engine.callTagged` returns (`@rightkit/tauri-shell` 0.1.2 `EngineCallError`): `origin` is
 * `supervisor` when the call was refused before the engine ran, `engine` for the engine's own reply.
 */
export type { EngineCallError };

export type EngineCallResult<T> = { ok: true; value: T } | { ok: false; error: EngineCallError };

/** Per-call tag; the shell echoes it on every engine event of that call. */
let tagCounter = 0;

/**
 * One engine method through the shell (`shell.engine.callTagged`, the owner's typed wrapper). Never throws: a refused
 * method, an engine error or a missing shell all come back as `{ ok: false, error }`.
 */
export async function engineCall<T>(method: string, params: Record<string, unknown>): Promise<EngineCallResult<T>> {
  tagCounter += 1;
  try {
    const value = await shell.engine.callTagged<T>(method, params, `postright-ui-${tagCounter}`);
    return { ok: true, value };
  } catch (raw) {
    const e = asShellError(raw) as Partial<EngineCallError> & { code: string; message: string };
    return {
      ok: false,
      error: { code: e.code, message: e.message, origin: e.origin === "engine" ? "engine" : "supervisor", ...(e.details !== undefined ? { details: e.details } : {}) },
    };
  }
}
