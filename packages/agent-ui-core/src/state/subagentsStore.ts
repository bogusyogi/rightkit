import { create } from "zustand";

export const SUBAGENT_LIFECYCLE_STATUSES = [
  "starting",
  "running",
  "done",
  "failed",
  "stopped",
] as const;

export type SubagentLifecycleStatus = (typeof SUBAGENT_LIFECYCLE_STATUSES)[number];

export type Subagent = {
  id: string;
  name: string;
  status: SubagentLifecycleStatus;
};

export type SubagentStart = Pick<Subagent, "id" | "name">;

export type SubagentLifecycleEvent =
  | { type: "started"; subagent: SubagentStart }
  | { type: "running"; id: string }
  | { type: "done"; id: string }
  | { type: "failed"; id: string }
  | { type: "stopped"; id: string };

/**
 * Boundary for Tauri IPC. The shell owns presentation; a later integration
 * only needs to adapt daemon events and stop requests to this stable contract.
 */
export interface SubagentsIpcAdapter {
  subscribe(listener: (event: SubagentLifecycleEvent) => void): () => void;
  stop(id: string): Promise<void>;
}

function updateStatus(
  subagents: readonly Subagent[],
  id: string,
  status: Exclude<SubagentLifecycleStatus, "starting">,
): Subagent[] {
  return subagents.map((subagent) =>
    subagent.id === id && (subagent.status === "starting" || subagent.status === "running")
      ? { ...subagent, status }
      : subagent,
  );
}

export type SubagentsStoreState = {
  subagents: Subagent[];
  start: (subagent: SubagentStart) => void;
  markRunning: (id: string) => void;
  markDone: (id: string) => void;
  markFailed: (id: string) => void;
  markStopped: (id: string) => void;
  applyLifecycleEvent: (event: SubagentLifecycleEvent) => void;
  reset: () => void;
};

export const useSubagentsStore = create<SubagentsStoreState>((set, get) => ({
  subagents: [],
  start(subagent) {
    set((state) => ({
      subagents: [
        ...state.subagents.filter((existing) => existing.id !== subagent.id),
        { ...subagent, status: "starting" },
      ],
    }));
  },
  markRunning(id) {
    set((state) => ({ subagents: updateStatus(state.subagents, id, "running") }));
  },
  markDone(id) {
    set((state) => ({ subagents: updateStatus(state.subagents, id, "done") }));
  },
  markFailed(id) {
    set((state) => ({ subagents: updateStatus(state.subagents, id, "failed") }));
  },
  markStopped(id) {
    set((state) => ({ subagents: updateStatus(state.subagents, id, "stopped") }));
  },
  applyLifecycleEvent(event) {
    switch (event.type) {
      case "started":
        get().start(event.subagent);
        return;
      case "running":
        get().markRunning(event.id);
        return;
      case "done":
        get().markDone(event.id);
        return;
      case "failed":
        get().markFailed(event.id);
        return;
      case "stopped":
        get().markStopped(event.id);
    }
  },
  reset() {
    set({ subagents: [] });
  },
}));

/** Connects an IPC implementation to the lifecycle store without involving shell components. */
export function connectSubagentsIpc(adapter: SubagentsIpcAdapter): () => void {
  return adapter.subscribe((event) => useSubagentsStore.getState().applyLifecycleEvent(event));
}
