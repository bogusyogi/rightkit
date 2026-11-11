import { videoHost, type VideoEngineHello, type VideoEngineState, type VideoEngineStatus } from "../host.js";
import type { EngineFrame } from "./progress.js";

export type EngineState = VideoEngineState;
export type EngineHello = VideoEngineHello;
export type EngineStatus = VideoEngineStatus;

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

/** The host engine's lifecycle snapshot. */
export const engineStatus = (): Promise<EngineStatus> => videoHost().engine.status();
export const engineDoctor = () => engineCall<DoctorReport>("doctor");

export type EngineCallError = { code: string; message: string; origin: "supervisor" | "engine" };

let activeCalls = 0;
export const engineBusy = () => activeCalls > 0;
/** One call to the host's video engine; events of the call echo `tag`; failures reject typed. */
export async function engineCall<T = unknown>(method: string, params: Record<string, unknown> = {}, tag?: string): Promise<T> {
  activeCalls += 1;
  try {
    return await videoHost().engine.call<T>(method, params, tag);
  } finally {
    activeCalls -= 1;
  }
}

export const revealPath = (path: string) => videoHost().revealPath(path);

export type { EngineEvent, EngineFrame } from "./progress.js";

/** Frames are `{id: engine request id, tag: the caller's tag for that call, event}`. */
export function onEngineEvent(handler: (frame: EngineFrame) => void): Promise<() => void> {
  return videoHost().engine.onEvent(handler);
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
