// I2 — layout store regression tests. Authored for the final integration
// lane; do not run here.

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { projectLayoutOrDefault, readPersistedLayout, useLayoutStore } from "./layoutStore.js";
import { useSidePanelStore } from "./sidePanelStore.js";
import { useRightPanelStore } from "./rightPanelStore.js";

function resetStores() {
  localStorage.clear();
  useLayoutStore.setState({ byProject: {} });
  useSidePanelStore.setState({ open: false, tab: "background-tasks" });
  useRightPanelStore.setState({ tab: "background-tasks" });
}

describe("layoutStore", () => {
  beforeEach(() => resetStores());
  afterEach(() => resetStores());

  it("returns a fresh default dock for a new project", () => {
    const dock = useLayoutStore.getState().byProject.fresh ?? null;
    expect(dock).toBeNull();
    const out = useLayoutStore.getState().setVisible("fresh", ["files", "terminal"]);
    expect(out).toBeUndefined();
    expect(useLayoutStore.getState().byProject.fresh?.visible).toEqual(["files", "terminal"]);
  });

  it("keeps utility panes closed before a project's first persisted mutation", () => {
    expect(projectLayoutOrDefault({}, "fresh").visible).toEqual([]);
    expect(projectLayoutOrDefault({}, "fresh").rightTab).toBe("files");
  });

  it("persists a per-project rightTab without disturbing others", () => {
    useLayoutStore.getState().setRightTab("alpha", "files");
    useLayoutStore.getState().setRightTab("beta", "artifacts");
    expect(useLayoutStore.getState().byProject.alpha?.rightTab).toBe("files");
    expect(useLayoutStore.getState().byProject.beta?.rightTab).toBe("artifacts");
    expect(useLayoutStore.getState().byProject.alpha?.sizes.sidebar).toBe(280);
  });

  it("migrates legacy cr.sidePanel.open / cr.rightPanel.tab keys", () => {
    localStorage.setItem("cr.sidePanel.open.v1", "1");
    localStorage.setItem("cr.rightPanel.tab.v1", "files");
    useLayoutStore.setState({ byProject: readPersistedLayout() });
    const state = useLayoutStore.getState();
    expect(state.byProject.default?.visible).toContain("files");
    expect(state.byProject.default?.rightTab).toBe("files");
  });

  it("toggles a surface and mirrors the right-pane tab", () => {
    useLayoutStore.getState().toggleSurface("p1", "artifacts");
    const dock = useLayoutStore.getState().byProject.p1;
    expect(dock?.visible).toContain("artifacts");
    expect(useRightPanelStore.getState().tab).toBe("artifacts");
    expect(useSidePanelStore.getState().open).toBe(true);

    useLayoutStore.getState().toggleSurface("p1", "artifacts");
    expect(useLayoutStore.getState().byProject.p1?.visible).not.toContain("artifacts");
  });

  it("toggles an active right panel closed and switches panels without stale visibility", () => {
    const store = useLayoutStore.getState();
    store.toggleRightPanel("p1", "files");
    expect(useSidePanelStore.getState().open).toBe(true);
    expect(useRightPanelStore.getState().tab).toBe("files");

    store.toggleRightPanel("p1", "artifacts");
    expect(useLayoutStore.getState().byProject.p1?.visible).toEqual(["artifacts"]);
    expect(useRightPanelStore.getState().tab).toBe("artifacts");

    store.toggleRightPanel("p1", "artifacts");
    expect(useSidePanelStore.getState().open).toBe(false);
    expect(useLayoutStore.getState().byProject.p1?.visible).toEqual([]);
  });

  it("derives toggle state from visible panels instead of the legacy side-panel mirror", () => {
    const store = useLayoutStore.getState();
    store.toggleRightPanel("p1", "files");
    useSidePanelStore.setState({ open: false });
    store.toggleRightPanel("p1", "files");
    expect(useLayoutStore.getState().byProject.p1?.visible).toEqual([]);
  });

  it("can update a review-tab project without duplicate right-panel dispatch", () => {
    useLayoutStore.getState().setRightTab("review-tab-project", "review", false);
    expect(useLayoutStore.getState().byProject["review-tab-project"]?.rightTab).toBe("review");
    expect(useRightPanelStore.getState().tab).toBe("background-tasks");
  });

  it("resets a project to defaults without touching other projects", () => {
    useLayoutStore.getState().setRightTab("p1", "files");
    useLayoutStore.getState().setRightTab("p2", "artifacts");
    useLayoutStore.getState().resetToDefault("p1");
    expect(useLayoutStore.getState().byProject.p1?.rightTab).toBe("files");
    expect(useLayoutStore.getState().byProject.p2?.rightTab).toBe("artifacts");
  });

  it("clamps and stores a recorded pane size", () => {
    useLayoutStore.getState().setSize("p1", "sidebar", 360);
    expect(useLayoutStore.getState().byProject.p1?.sizes.sidebar).toBe(360);
  });

  it("persists maximize state and reset restores the composed workspace", () => {
    useLayoutStore.getState().setMaximized("p1", "terminal");
    expect(useLayoutStore.getState().byProject.p1?.maximized).toBe("terminal");
    useLayoutStore.getState().resetToDefault("p1");
    expect(useLayoutStore.getState().byProject.p1?.maximized).toBeNull();
    expect(useLayoutStore.getState().byProject.p1?.visible).toEqual([]);
  });

  it("drops a corrupt maximized pane during migration", () => {
    localStorage.setItem(
      "cr.layout.v1",
      JSON.stringify({
        p1: { maximized: "not-a-pane", visible: ["browser"] },
      }),
    );
    const restored = readPersistedLayout();
    expect(restored.p1?.maximized).toBeNull();
    expect(restored.p1?.visible).toEqual(["browser"]);
  });
});
