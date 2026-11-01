import { Channel } from "@tauri-apps/api/core";
import type { EngineLifecycleEvent, JobEvent, JobKindEvent } from "../../contract";
import { invoke, isQaMode, listen } from "./index";
import { dispatchStreamEvent } from "./streamEvents";
import type { EngineSnapshot, EngineStreamHandlers, StreamEvent } from "./streamEvents";

export interface EngineSubscription {
  /** `null` in a QA build, where the mock backend has no `subscribe`. */
  snapshot: EngineSnapshot | null;
  close(): void;
}

/**
 * Attach to the engine once: one IPC returns the job snapshot and the lifecycle
 * state, and every later change arrives on the same ordered channel. The shell
 * attaches before it reads the snapshot, so nothing lands in the gap between
 * them. A QA build keeps the mock's per-event listeners.
 */
export async function subscribeEngine(
  handlers: EngineStreamHandlers,
): Promise<EngineSubscription> {
  let open = true;
  if (isQaMode) {
    const unlisten = await Promise.all([
      listen<JobEvent>("job://update", (e) => open && handlers.onJobUpdate(e.payload)),
      listen<JobKindEvent>("job://kind", (e) => open && handlers.onJobKind(e.payload)),
      listen<EngineLifecycleEvent>(
        "engine://state",
        (e) => open && handlers.onEngineState(e.payload),
      ),
    ]);
    return {
      snapshot: null,
      close: () => {
        open = false;
        for (const f of unlisten) f();
      },
    };
  }
  const channel = new Channel<StreamEvent>();
  channel.onmessage = (event) => {
    if (open) dispatchStreamEvent(handlers, event);
  };
  const snapshot = await invoke<EngineSnapshot>("subscribe", { onEvent: channel });
  return {
    snapshot,
    close: () => {
      open = false;
    },
  };
}
