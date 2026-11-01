// @vitest-environment happy-dom
import { beforeEach, describe, expect, it } from "vitest";
import {
  groupProjectRecents,
  normalizeProjectPath,
  projectLabel,
  useProjectRecentsStore,
} from "./projectRecentsStore";

beforeEach(() => {
  localStorage.clear();
  useProjectRecentsStore.getState().clear();
});

describe("projectRecentsStore", () => {
  it("normalizes paths and derives the final folder label", () => {
    expect(normalizeProjectPath("C:\\work\\demo\\")).toBe("C:/work/demo");
    expect(projectLabel("/work/demo")).toBe("demo");
    expect(projectLabel("C:/")).toBe("C:/");
  });

  it("orders pinned projects before recent unpinned projects", () => {
    const store = useProjectRecentsStore.getState();
    store.addOrTouch("/work/old", "Old");
    store.addOrTouch("/work/new", "New");
    store.togglePinned("/work/old");

    expect(useProjectRecentsStore.getState().projects.map((project) => project.label)).toEqual([
      "Old",
      "New",
    ]);
  });

  it("persists grouping and restores it through hydrate", () => {
    const store = useProjectRecentsStore.getState();
    store.addOrTouch("/work/demo", "Demo");
    store.setGroup("/work/demo", "Clients");
    expect(groupProjectRecents(useProjectRecentsStore.getState().projects)).toEqual([
      { name: "Clients", projects: [expect.objectContaining({ label: "Demo", group: "Clients" })] },
    ]);

    useProjectRecentsStore.setState({ projects: [], activePath: null });
    useProjectRecentsStore.getState().hydrate();
    expect(useProjectRecentsStore.getState().projects[0]).toMatchObject({
      label: "Demo",
      group: "Clients",
    });
  });
});
