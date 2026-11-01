import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { EngineLifecycleEvent, JobEvent, JobKindEvent } from "../../src/queue/contract";
import {
  dispatchStreamEvent,
  lifecycleFromState,
  type EngineStreamHandlers,
} from "../../src/queue/lib/ipc/streamEvents";

function recorder() {
  const seen: string[] = [];
  const handlers: EngineStreamHandlers = {
    onJobUpdate: (e: JobEvent) => seen.push(`update:${e.id}:${e.pct}`),
    onJobKind: (e: JobKindEvent) => seen.push(`kind:${e.id}`),
    onEngineState: (e: EngineLifecycleEvent) => seen.push(`state:${e.state}`),
  };
  return { seen, handlers };
}

describe("engine stream dispatch", () => {
  it("routes each tagged event to its handler in arrival order", () => {
    const { seen, handlers } = recorder();
    dispatchStreamEvent(handlers, {
      type: "job_update",
      data: {
        id: "a",
        stage: "downloading",
        pct: 40,
        msg: null,
        error: null,
        error_kind: null,
        needs_auth: null,
      },
    });
    dispatchStreamEvent(handlers, { type: "job_kind", data: { id: "a", kind: "carousel" } });
    dispatchStreamEvent(handlers, { type: "engine_state", data: lifecycleFromState("ready") });
    assert.deepEqual(seen, ["update:a:40", "kind:a", "state:ready"]);
  });

  it("builds a lifecycle event from a snapshot state", () => {
    const event = lifecycleFromState("recovering");
    assert.equal(event.state, "recovering");
    assert.equal(event.message, "");
  });
});
