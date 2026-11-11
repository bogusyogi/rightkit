import { Channel } from "@tauri-apps/api/core";
import type { EngineLifecycleEvent, JobEvent, JobKindEvent } from "../../contract.js";
import { invoke } from "./index.js";
import { dispatchStreamEvent } from "./streamEvents.js";
import type { EngineSnapshot, EngineStreamHandlers, StreamEvent } from "./streamEvents.js";

export interface EngineSubscription {
  /** `null` in a QA build, where the mock backend has no `subscribe`. */
  snapshot: EngineSnapshot | null;
  close(): void;
}

/**
 * Attach to the engine once: one IPC returns the job snapshot and the lifecycle
 * state, and every later change arrives on the same ordered channel. The shell
 * attaches before it reads the snapshot, so nothing lands in the gap between
 * them.
 */
export async function subscribeEngine(
  handlers: EngineStreamHandlers,
): Promise<EngineSubscription> {
  let open = true;
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
