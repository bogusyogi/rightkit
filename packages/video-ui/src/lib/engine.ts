import { invoke } from "@tauri-apps/api/core";
import type { EngineEvent as ShellEngineEvent } from "@rightkit/tauri-shell";

import type { EngineFrame } from "./progress";
import { shell } from "./shell";

// the host app view model adapted from shared engine lifecycle snapshots.
export type EngineState = "starting" | "ready" | "restarting" | "failed" | "stopped";

export type EngineHello = { engine: string; version: string; contract_version: number };

export type EngineStatus = {
  state: EngineState;
  hello: EngineHello | null;
  restarts: number;
  last_error: string | null;
};

export type DoctorCheck = {
  id?: string;
  name?: string;
  status?: string;
  detail?: string;
  message?: string;
};

export type DoctorReport = {
  readiness?: { status?: string; checks?: DoctorCheck[] };
};

export const engineStatus = async (): Promise<EngineStatus> => {
  // Typed lifecycle snapshot from @rightkit/tauri-shell 0.1.2 (`rk_shell_engine_status`).
  const status = (await shell.engine.status()) as {
    state: EngineState | "error";
    hello?: EngineHello | null;
    restarts?: number;
    last_error?: string | null;
    message?: string;
  } | null;
  if (!status) return { state: "failed", hello: null, restarts: 0, last_error: "no engine configured" };
  return {
    state: status.state === "error" ? "failed" : status.state,
    hello: (status.hello ?? null) as EngineHello | null,
    restarts: status.restarts ?? 0,
    last_error: status.last_error ?? status.message ?? null,
  };
};
export const engineDoctor = () => engineCall<DoctorReport>("doctor");

export type EngineCallError = { code: string; message: string; origin: "supervisor" | "engine" };

/** Allowlisted passthrough (see `ALLOWED_METHODS` in the supervisor). */
let activeCalls = 0;
export const engineBusy = () => activeCalls > 0;
export async function engineCall<T = unknown>(method: string, params: Record<string, unknown> = {}, tag?: string): Promise<T> {
  activeCalls += 1;
  try {
    // Tagged call from @rightkit/tauri-shell 0.1.2: events echo the tag, failures reject typed.
    return await shell.engine.callTagged<T>(method, params, tag);
  } finally {
    activeCalls -= 1;
  }
}

export const revealPath = (path: string) => invoke<void>("reveal_path", { path });

export type { EngineEvent, EngineFrame } from "./progress";

/** Every engine event payload (`{call, id, tag, event}`), lifecycle frames included. */
export const onEngineRaw = (handler: (payload: ShellEngineEvent) => void): Promise<() => void> =>
  shell.engine.onEvent(handler);

/** Frames are `{id: engine request id, tag: the caller's tag for that call, event}`. */
export function onEngineEvent(handler: (frame: EngineFrame) => void): Promise<() => void> {
  return shell.engine.onEvent((frame) => {
    if (frame.call === "$lifecycle" || frame.id === undefined) return;
    handler({ id: frame.id, tag: frame.tag ?? null, event: frame.event as EngineFrame["event"] });
  });
}

export function errorCode(error: unknown): string | undefined {
  if (error && typeof error === "object" && "code" in error) {
    return String((error as { code: unknown }).code);
  }
  return undefined;
}

export function errorMessage(error: unknown): string {
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  return String(error);
}
