import { create } from "zustand";

const STORAGE_KEY = "cr.sidePanel.open.v1";

function readOpen(): boolean {
  if (typeof localStorage === "undefined") return false;
  try {
    return localStorage.getItem(STORAGE_KEY) === "1";
  } catch {
    return false;
  }
}

function persistOpen(open: boolean) {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(STORAGE_KEY, open ? "1" : "0");
  } catch {
    // ignore
  }
}

export type SidePanelTab = "background-tasks" | "files";

export type SidePanelState = {
  open: boolean;
  tab: SidePanelTab;
  toggle: () => void;
  setOpen: (next: boolean) => void;
  setTab: (tab: SidePanelTab) => void;
};

export const useSidePanelStore = create<SidePanelState>((set) => ({
  open: readOpen(),
  tab: "background-tasks",
  toggle() {
    set((state) => {
      const next = !state.open;
      persistOpen(next);
      return { open: next };
    });
  },
  setOpen(next) {
    persistOpen(next);
    set({ open: next });
  },
  setTab(tab) {
    set({ tab });
  },
}));
