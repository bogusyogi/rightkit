// Per-project shell layout store. Owns tab/visibility/size state for
// every dock group so chat streaming and side panels survive a tab swap
// or a pane resize. Backed by localStorage under one key per project,
// with a migration shim that accepts the old per-surface keys
// (`cr.sidePanel.open.v1`, `cr.rightPanel.tab.v1`) so a stale install
// does not lose its panel state.

import { create } from "zustand";

import { useSidePanelStore } from "./sidePanelStore.js";
import { useRightPanelStore, type RightPanelTab } from "./rightPanelStore.js";

const LEGACY_OPEN_KEY = "cr.sidePanel.open.v1";
const LEGACY_TAB_KEY = "cr.rightPanel.tab.v1";
const STORAGE_KEY = "cr.layout.v1";

export type DockSurface =
  | "files"
  | "agents"
  | "artifacts"
  | "review"
  | "execution"
  | "bots"
  | "terminal"
  | "diagnostics"
  | "browser"
  | "delivery"
  | "environments"
  | "attention";

export type WorkspaceRightPanelTab = Exclude<RightPanelTab, "background-tasks">;

const SURFACES: ReadonlyArray<DockSurface> = [
  "files",
  "agents",
  "artifacts",
  "review",
  "execution",
  "bots",
  "terminal",
  "diagnostics",
  "browser",
  "delivery",
  "environments",
  "attention",
] as const;

export type LayoutPane =
  | "sidebar"
  | "chat"
  | "right"
  | "files"
  | "review"
  | "agents"
  | "artifacts"
  | "terminal"
  | "diagnostics"
  | "browser";

export interface DockGroupState {
  /** Which surfaces are currently visible. */
  visible: ReadonlyArray<DockSurface>;
  /** Right-pane tab — also written to `rightPanelStore` so legacy wiring keeps working. */
  rightTab: RightPanelTab;
  /** Free-form map of pane id → pixel width/height. The lead integrates this
   *  with the actual `WorkbenchContent` `sidebarWidth` and the right rail. */
  sizes: Record<LayoutPane, number>;
  /** Last command-palette query the user accepted in this project. */
  lastPaletteQuery: string;
  /** One temporarily expanded pane; null keeps the normal dock composition. */
  maximized: LayoutPane | null;
}

export interface LayoutState {
  byProject: Record<string, DockGroupState>;
  setVisible: (projectId: string, surfaces: ReadonlyArray<DockSurface>) => void;
  toggleSurface: (projectId: string, surface: DockSurface) => void;
  toggleRightPanel: (projectId: string, tab: WorkspaceRightPanelTab) => void;
  setSize: (projectId: string, pane: LayoutPane, size: number) => void;
  setRightTab: (projectId: string, tab: RightPanelTab, mirror?: boolean) => void;
  setLastPaletteQuery: (projectId: string, query: string) => void;
  setMaximized: (projectId: string, pane: LayoutPane | null) => void;
  resetToDefault: (projectId: string) => void;
}

const DEFAULT_DOCK: DockGroupState = {
  // A new session is a writing surface. Utility panes remain one click away
  // but must not consume a quarter of the window before the user asks for one.
  visible: [],
  rightTab: "files",
  sizes: {
    sidebar: 280,
    chat: 0,
    right: 380,
    files: 320,
    review: 0,
    agents: 320,
    artifacts: 320,
    terminal: 240,
    diagnostics: 220,
    browser: 0,
  },
  lastPaletteQuery: "",
  maximized: null,
};

/** Read a project's effective layout without forcing a persistence write.
 * Fresh workspaces must render the same quiet default as a workspace that has
 * been explicitly reset. */
export function projectLayoutOrDefault(
  state: Record<string, DockGroupState>,
  projectId: string,
): DockGroupState {
  return state[projectId] ?? DEFAULT_DOCK;
}

const DEFAULT_PROJECT = "default";

export function readPersistedLayout(): Record<string, DockGroupState> {
  if (typeof localStorage === "undefined") return {};
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Record<string, Partial<DockGroupState>> | null;
      if (parsed && typeof parsed === "object") {
        return migrate(parsed);
      }
    }
  } catch {
    // fall through to legacy keys
  }
  // Legacy: a single global side/right panel state. Surface as a per-project
  // state under the default project id so the new layout store reads it
  // without losing the user's previous session.
  const legacy: Record<string, DockGroupState> = {};
  if (typeof localStorage === "undefined") return legacy;
  try {
    const open = localStorage.getItem(LEGACY_OPEN_KEY) === "1";
    const tabRaw = localStorage.getItem(LEGACY_TAB_KEY);
    const tab: RightPanelTab =
      tabRaw === "files" || tabRaw === "artifacts" || tabRaw === "background-tasks"
        ? tabRaw
        : "background-tasks";
    legacy[DEFAULT_PROJECT] = {
      ...DEFAULT_DOCK,
      visible: open ? ["files"] : [],
      rightTab: tab,
    };
  } catch {
    // ignore
  }
  return legacy;
}

function migrate(raw: Record<string, Partial<DockGroupState>>): Record<string, DockGroupState> {
  const out: Record<string, DockGroupState> = {};
  for (const [projectId, partial] of Object.entries(raw)) {
    const merged: DockGroupState = {
      visible: Array.isArray(partial.visible)
        ? (partial.visible.filter((s): s is DockSurface =>
            (SURFACES as ReadonlyArray<string>).includes(s),
          ) as DockSurface[])
        : DEFAULT_DOCK.visible,
      rightTab:
        partial.rightTab === "files" ||
        partial.rightTab === "artifacts" ||
        partial.rightTab === "background-tasks" ||
        partial.rightTab === "review" ||
        partial.rightTab === "browser" ||
        partial.rightTab === "delivery" ||
        partial.rightTab === "environments" ||
        partial.rightTab === "attention"
          ? partial.rightTab
          : DEFAULT_DOCK.rightTab,
      sizes: { ...DEFAULT_DOCK.sizes, ...(partial.sizes ?? {}) },
      lastPaletteQuery: partial.lastPaletteQuery ?? "",
      maximized:
        typeof partial.maximized === "string" &&
        [
          "sidebar",
          "chat",
          "right",
          "files",
          "review",
          "agents",
          "artifacts",
          "terminal",
          "diagnostics",
          "browser",
        ].includes(partial.maximized)
          ? (partial.maximized as LayoutPane)
          : null,
    };
    out[projectId] = merged;
  }
  return out;
}

function persist(state: Record<string, DockGroupState>): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    // ignore; the in-memory copy is still authoritative for this session
  }
}

function getProject(state: Record<string, DockGroupState>, projectId: string): DockGroupState {
  return projectLayoutOrDefault(state, projectId);
}

export const useLayoutStore = create<LayoutState>((set, get) => {
  const initial = readPersistedLayout();
  return {
    byProject: initial,
    setVisible(projectId, surfaces) {
      const next = { ...get().byProject };
      next[projectId] = {
        ...getProject(next, projectId),
        visible: [...surfaces],
      };
      persist(next);
      set({ byProject: next });
    },
    toggleSurface(projectId, surface) {
      const current = getProject(get().byProject, projectId);
      const visible = current.visible.includes(surface)
        ? current.visible.filter((s) => s !== surface)
        : [...current.visible, surface];
      const next = { ...get().byProject };
      next[projectId] = { ...current, visible };
      persist(next);
      set({ byProject: next });
      // Mirror the right-panel tab into the existing store so the existing
      // `AppBody` right-pane renderers keep working without a refactor.
      if (
        surface === "files" ||
        surface === "agents" ||
        surface === "artifacts" ||
        surface === "review" ||
        surface === "browser" ||
        surface === "delivery" ||
        surface === "environments" ||
        surface === "attention"
      ) {
        useSidePanelStore.getState().setOpen(true);
        const tab: RightPanelTab = surface === "agents" ? "background-tasks" : surface;
        useRightPanelStore.getState().setTab(tab);
        next[projectId] = { ...next[projectId], rightTab: tab };
        persist(next);
        set({ byProject: next });
      }
    },
    toggleRightPanel(projectId, tab) {
      const current = getProject(get().byProject, projectId);
      const isActive = current.visible.includes(tab);
      const rightSurfaces: ReadonlyArray<DockSurface> = [
        "files",
        "artifacts",
        "review",
        "browser",
        "delivery",
        "environments",
        "attention",
      ];
      const visible = isActive
        ? current.visible.filter((surface) => surface !== tab)
        : [...current.visible.filter((surface) => !rightSurfaces.includes(surface)), tab];
      const next = {
        ...get().byProject,
        [projectId]: { ...current, visible, rightTab: tab },
      };
      persist(next);
      set({ byProject: next });
      useSidePanelStore.getState().setOpen(!isActive);
      if (!isActive) useRightPanelStore.getState().setTab(tab);
    },
    setSize(projectId, pane, size) {
      const current = getProject(get().byProject, projectId);
      const next = { ...get().byProject };
      next[projectId] = {
        ...current,
        sizes: { ...current.sizes, [pane]: size },
      };
      persist(next);
      set({ byProject: next });
    },
    setRightTab(projectId, tab, mirror = true) {
      const current = getProject(get().byProject, projectId);
      const next = { ...get().byProject };
      next[projectId] = { ...current, rightTab: tab };
      persist(next);
      set({ byProject: next });
      if (mirror) useRightPanelStore.getState().setTab(tab);
    },
    setLastPaletteQuery(projectId, query) {
      const current = getProject(get().byProject, projectId);
      const next = { ...get().byProject };
      next[projectId] = { ...current, lastPaletteQuery: query };
      persist(next);
      set({ byProject: next });
    },
    setMaximized(projectId, pane) {
      const current = getProject(get().byProject, projectId);
      const next = { ...get().byProject };
      next[projectId] = { ...current, maximized: pane };
      persist(next);
      set({ byProject: next });
    },
    resetToDefault(projectId) {
      const next = { ...get().byProject };
      next[projectId] = { ...DEFAULT_DOCK };
      persist(next);
      set({ byProject: next });
    },
  };
});
