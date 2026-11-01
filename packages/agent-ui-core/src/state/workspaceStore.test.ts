// D1 — workspaceStore tests.

import { describe, expect, it } from "vitest";
import {
  normalizePath,
  pathBasename,
  pathJoin,
  pathParent,
  useWorkspaceStore,
} from "./workspaceStore";

describe("workspaceStore", () => {
  it("normalizes paths", () => {
    expect(normalizePath("")).toBe("");
    expect(normalizePath("/")).toBe("");
    expect(normalizePath("\\foo\\bar")).toBe("foo/bar");
    expect(normalizePath("foo/bar/")).toBe("foo/bar");
    expect(normalizePath("/foo/bar/")).toBe("foo/bar");
  });

  it("joins paths without producing double slashes", () => {
    expect(pathJoin("", "foo")).toBe("foo");
    expect(pathJoin("foo", "bar")).toBe("foo/bar");
    expect(pathJoin("foo/", "/bar")).toBe("foo/bar");
    expect(pathJoin("foo", "")).toBe("foo");
  });

  it("derives parent and basename", () => {
    expect(pathParent("foo/bar/baz")).toBe("foo/bar");
    expect(pathParent("baz")).toBe("");
    expect(pathBasename("foo/bar/baz")).toBe("baz");
    expect(pathBasename("")).toBe("");
  });

  it("remembers and forgets expanded paths", () => {
    const store = useWorkspaceStore.getState();
    store.resetForWorkspace("/root");
    store.rememberPath("foo", [{ name: "bar", path: "foo/bar", kind: "file" }], false);
    const after = useWorkspaceStore.getState().expanded;
    expect(after.foo.entries.length).toBe(1);
    store.forgetPath("foo");
    expect(useWorkspaceStore.getState().expanded.foo).toBeUndefined();
  });

  it("toggles expanded state via remember/forget", () => {
    const store = useWorkspaceStore.getState();
    store.resetForWorkspace("/root");
    store.toggleExpanded("a");
    expect(useWorkspaceStore.getState().expanded.a).toBeDefined();
    store.toggleExpanded("a");
    expect(useWorkspaceStore.getState().expanded.a).toBeUndefined();
  });

  it("resets when the workspace root changes", () => {
    const store = useWorkspaceStore.getState();
    store.resetForWorkspace("/a");
    store.rememberPath("x", [], false);
    expect(useWorkspaceStore.getState().expanded.x).toBeDefined();
    store.resetForWorkspace("/b");
    expect(useWorkspaceStore.getState().workspaceRoot).toBe("/b");
    expect(useWorkspaceStore.getState().expanded.x).toBeUndefined();
  });

  it("bumps the scroll token when the workspace root changes", () => {
    const store = useWorkspaceStore.getState();
    store.resetForWorkspace("/scroll-a");
    const token = useWorkspaceStore.getState().scrollToken;
    store.resetForWorkspace("/scroll-b");
    expect(useWorkspaceStore.getState().scrollToken).toBeGreaterThan(token);
  });
});
