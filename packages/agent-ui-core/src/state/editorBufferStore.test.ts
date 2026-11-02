import { afterEach, describe, expect, it } from "vitest";

import {
  __resetEditorBuffers,
  decideExternalChange,
  disposeEditorBuffer,
  getEditorBuffer,
  registerEditorBuffer,
  useEditorBufferStore,
} from "./editorBufferStore.js";

afterEach(() => {
  __resetEditorBuffers();
});

describe("editorBufferStore", () => {
  it("keeps a registered buffer and its view state across tab changes", () => {
    const buffer = registerEditorBuffer("tab-1", "/repo", "src/App.tsx");
    useEditorBufferStore.getState().updateContent("tab-1", "draft");
    useEditorBufferStore.getState().setViewState("tab-1", {
      cursorLine: 4,
      cursorColumn: 2,
      scrollTop: 80,
    });

    expect(getEditorBuffer("tab-1")).toMatchObject({
      ...buffer,
      content: "draft",
      dirty: true,
      viewState: { cursorLine: 4, cursorColumn: 2, scrollTop: 80 },
    });

    // A path update is the donor's rename seam; it must not discard the draft.
    registerEditorBuffer("tab-1", "/repo", "src/Renamed.tsx");
    expect(getEditorBuffer("tab-1")).toMatchObject({
      filePath: "src/Renamed.tsx",
      content: "draft",
      dirty: true,
    });
  });

  it("hydrates a clean buffer and makes explicit disposal idempotent", () => {
    registerEditorBuffer("tab-1", "/repo", "src/App.tsx");
    useEditorBufferStore.getState().hydrate("tab-1", {
      workspaceRoot: "/repo",
      path: "src/App.tsx",
      kind: "file",
      sizeBytes: 5,
      modifiedAt: "2026-08-29T00:00:00Z",
      fingerprint: "fp-1",
      content: "hello",
      permission: "0644",
      truncated: false,
      reason: null,
    });

    expect(getEditorBuffer("tab-1")).toMatchObject({
      content: "hello",
      baselineContent: "hello",
      loaded: true,
      dirty: false,
      fingerprint: "fp-1",
    });
    disposeEditorBuffer("tab-1");
    disposeEditorBuffer("tab-1");
    expect(getEditorBuffer("tab-1")).toBeUndefined();
  });
});

describe("decideExternalChange", () => {
  it("does nothing without a changed fingerprint", () => {
    expect(decideExternalChange("fp-1", "fp-1", false)).toBe("none");
    expect(decideExternalChange(null, "fp-2", true)).toBe("none");
  });

  it("reloads a clean buffer and flags a dirty buffer", () => {
    expect(decideExternalChange("fp-1", "fp-2", false)).toBe("reload");
    expect(decideExternalChange("fp-1", "fp-2", true)).toBe("flag");
  });
});
