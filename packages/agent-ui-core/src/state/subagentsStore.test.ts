import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  connectSubagentsIpc,
  type SubagentLifecycleEvent,
  type SubagentsIpcAdapter,
  useSubagentsStore,
} from "./subagentsStore";

describe("subagents lifecycle store", () => {
  beforeEach(() => {
    useSubagentsStore.getState().reset();
  });

  it("moves a subagent from starting through running to done", () => {
    const store = useSubagentsStore.getState();

    store.start({ id: "worker-1", name: "Researcher" });
    expect(useSubagentsStore.getState().subagents).toEqual([
      expect.objectContaining({ id: "worker-1", name: "Researcher", status: "starting" }),
    ]);

    useSubagentsStore.getState().markRunning("worker-1");
    useSubagentsStore.getState().markDone("worker-1");

    expect(useSubagentsStore.getState().subagents[0]).toMatchObject({ status: "done" });
  });

  it("records terminal failures and stops without changing other rows", () => {
    const store = useSubagentsStore.getState();

    store.start({ id: "worker-1", name: "Researcher" });
    store.start({ id: "worker-2", name: "Reviewer" });
    useSubagentsStore.getState().markFailed("worker-1");
    useSubagentsStore.getState().markStopped("worker-2");

    expect(useSubagentsStore.getState().subagents).toEqual([
      expect.objectContaining({ id: "worker-1", status: "failed" }),
      expect.objectContaining({ id: "worker-2", status: "stopped" }),
    ]);
  });

  it("does not overwrite a terminal lifecycle state", () => {
    const store = useSubagentsStore.getState();

    store.start({ id: "worker-1", name: "Researcher" });
    useSubagentsStore.getState().markDone("worker-1");
    useSubagentsStore.getState().markRunning("worker-1");

    expect(useSubagentsStore.getState().subagents[0]).toMatchObject({ status: "done" });
  });

  it("adapts IPC lifecycle events without coupling the shell to Tauri", () => {
    let publish: ((event: SubagentLifecycleEvent) => void) | undefined;
    const adapter: SubagentsIpcAdapter = {
      subscribe(listener) {
        publish = listener;
        return vi.fn();
      },
      stop: vi.fn().mockResolvedValue(undefined),
    };

    const disconnect = connectSubagentsIpc(adapter);
    publish?.({ type: "started", subagent: { id: "worker-1", name: "Researcher" } });
    publish?.({ type: "running", id: "worker-1" });

    expect(useSubagentsStore.getState().subagents[0]).toMatchObject({ status: "running" });
    disconnect();
  });
});
