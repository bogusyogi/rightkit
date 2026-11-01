import { create } from "zustand";

// Right-panel tab union. The store mirrors `useSidePanelStore` semantics so
// the existing toggle (cmd+.) keeps working, and adds an explicit
// "files" + "artifacts" tab set behind the same open/toggle. The Files
// panel slides in from the right when opened, replacing BackgroundTasksPanel
// when its tab is selected. Each panel can be open independent of the tab
// state so the floating "Files" entry in the kebab menu opens the Files
// panel directly without altering the existing background-tasks default.

const STORAGE_KEY = "cr.rightPanel.tab.v1";

export type RightPanelTab =
  | "background-tasks"
  | "files"
  | "artifacts"
  | "review"
  | "execution"
  | "bots"
  | "browser"
  | "delivery"
  | "environments"
  | "attention";

type PersistedState = { tab: RightPanelTab };

function readPersisted(): PersistedState {
  if (typeof localStorage === "undefined") return { tab: "background-tasks" };
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { tab: "background-tasks" };
    const parsed = JSON.parse(raw) as Partial<PersistedState> | null;
    const tab = parsed?.tab;
    if (
      tab === "background-tasks" ||
      tab === "files" ||
      tab === "artifacts" ||
      tab === "review" ||
      tab === "execution" ||
      tab === "bots" ||
      tab === "browser" ||
      tab === "delivery" ||
      tab === "environments" ||
      tab === "attention"
    ) {
      return { tab };
    }
    return { tab: "background-tasks" };
  } catch {
    return { tab: "background-tasks" };
  }
}

function persist(state: PersistedState) {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    // ignore; we still own the in-memory copy for this session
  }
}

export interface RightPanelState {
  tab: RightPanelTab;
  setTab: (tab: RightPanelTab) => void;
  openTab: (tab: RightPanelTab) => void;
}

export const useRightPanelStore = create<RightPanelState>((set) => {
  const initial = readPersisted();
  return {
    tab: initial.tab,
    setTab(tab) {
      persist({ tab });
      set({ tab });
    },
    openTab(tab) {
      persist({ tab });
      set({ tab });
    },
  };
});
