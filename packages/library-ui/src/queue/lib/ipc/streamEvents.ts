import type { EngineLifecycleEvent, JobEvent, JobInfo, JobKindEvent } from "../../contract";

/** One ordered stream from the shell's `subscribe` command (#178). */
export type StreamEvent =
  | { type: "job_update"; data: JobEvent }
  | { type: "job_kind"; data: JobKindEvent }
  | { type: "engine_state"; data: EngineLifecycleEvent }
  /** The shell's receiver lagged and dropped events; `data` is a fresh full snapshot. */
  | { type: "resync"; data: EngineSnapshot };

/** Returned by `subscribe` atomically with the stream attaching. */
export interface EngineSnapshot {
  jobs: JobInfo[];
  engine_state: EngineLifecycleEvent["state"];
}

export interface EngineStreamHandlers {
  onJobUpdate(event: JobEvent): void;
  onJobKind(event: JobKindEvent): void;
  onEngineState(event: EngineLifecycleEvent): void;
  onResync(snapshot: EngineSnapshot): void;
}

export function dispatchStreamEvent(handlers: EngineStreamHandlers, event: StreamEvent): void {
  switch (event.type) {
    case "job_update":
      handlers.onJobUpdate(event.data);
      break;
    case "job_kind":
      handlers.onJobKind(event.data);
      break;
    case "engine_state":
      handlers.onEngineState(event.data);
      break;
    case "resync":
      handlers.onResync(event.data);
      break;
  }
}

/** A lifecycle event for a state read from the snapshot, which carries no diagnostics. */
export function lifecycleFromState(state: EngineLifecycleEvent["state"]): EngineLifecycleEvent {
  return { state, generation: 0, attempt: 0, restart_count_window: 0, message: "" };
}
