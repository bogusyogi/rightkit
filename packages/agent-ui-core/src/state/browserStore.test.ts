// BrowserWorkspace store tests — owned by the D3 workspace fragment.
// Co-located; vitest-style assertions.

import { describe, expect, it } from "vitest";
import { useBrowserWorkspaceStore } from "./browserStore";

describe("browserWorkspaceStore", () => {
  it("starts empty", () => {
    const s = useBrowserWorkspaceStore.getState();
    expect(s.summary).toBeNull();
    expect(s.console).toEqual([]);
    expect(s.errors).toEqual([]);
    expect(s.screenshot).toBeNull();
    expect(s.annotations).toEqual([]);
    expect(s.loading).toBe(false);
    expect(s.lastError).toBeNull();
  });

  it("upserts annotations by id", () => {
    useBrowserWorkspaceStore.getState().reset();
    useBrowserWorkspaceStore.getState().upsertAnnotation({
      id: "ann-1",
      atMs: 0,
      note: "first",
      dispatched: false,
    });
    useBrowserWorkspaceStore.getState().upsertAnnotation({
      id: "ann-1",
      atMs: 1,
      note: "second",
      dispatched: true,
    });
    useBrowserWorkspaceStore.getState().upsertAnnotation({
      id: "ann-2",
      atMs: 2,
      note: "third",
      dispatched: false,
    });
    const annotations = useBrowserWorkspaceStore.getState().annotations;
    expect(annotations).toHaveLength(2);
    expect(annotations.find((a) => a.id === "ann-1")?.note).toBe("second");
    expect(annotations.find((a) => a.id === "ann-1")?.dispatched).toBe(true);
  });

  it("reset clears all slices", () => {
    useBrowserWorkspaceStore.getState().setLoading(true);
    useBrowserWorkspaceStore.getState().setError("oops");
    useBrowserWorkspaceStore
      .getState()
      .setAnnotations([{ id: "a", atMs: 0, note: "x", dispatched: false }]);
    useBrowserWorkspaceStore.getState().reset();
    const s = useBrowserWorkspaceStore.getState();
    expect(s.loading).toBe(false);
    expect(s.lastError).toBeNull();
    expect(s.annotations).toEqual([]);
  });
});
