// Single IPC seam. In Tauri: real invoke. In browser QA (no Tauri runtime): a deterministic mock
// so hidden-browser UI tests need neither the shell nor the engine.
import { invoke as tauriInvoke } from "@tauri-apps/api/core";

export const isTauri = (): boolean => "__TAURI_INTERNALS__" in globalThis;

type Mock = (args?: Record<string, unknown>) => unknown;

/** Browser-QA mock table. Keep keys equal to real command names. */
const mocks: Record<string, Mock> = {
  engine_call: (args) => {
    if (args?.cmd === "doctor") return { ok: true, version: "mock" };
    return args?.input ?? null;
  },
  rightkit_logs_write: () => undefined,
  rightkit_logs_collect: () => ({}),
  rightkit_logs_clear: () => undefined,
  device_get_or_create: () => "00000000-0000-4000-8000-000000000000",
};

export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauri()) return tauriInvoke<T>(command, args);
  const mock = mocks[command];
  if (!mock) throw new Error(`no browser-QA mock for command: ${command}`);
  return (await mock(args)) as T;
}
