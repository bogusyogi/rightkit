// D4 — Helper unit tests for the delivery / environments / attention client.

import { describe, expect, it } from "vitest";

import {
  environmentKindLabel,
  groupAttentionByState,
  hasUrgentAttention,
  pickRuntimeSupportedEnvironment,
} from "./delivery-environments-attention";
import type { AttentionItem, AttentionState, EnvironmentRecord } from "../types";

const baseEnv = (overrides: Partial<EnvironmentRecord>): EnvironmentRecord => ({
  id: "local",
  label: "Local",
  kind: "local",
  workspaceRoot: "/Users/me/code",
  trust: "trusted",
  setupState: "ready",
  runtimeSupported: true,
  runtimeUnavailableReason: null,
  providers: [],
  ...overrides,
});

describe("delivery / environments / attention helpers", () => {
  it("picks the first runtime-supported environment", () => {
    const environments: EnvironmentRecord[] = [
      baseEnv({
        id: "ssh",
        kind: "ssh",
        runtimeSupported: false,
        runtimeUnavailableReason: "ssh executor missing",
      }),
      baseEnv({ id: "local", kind: "local" }),
      baseEnv({ id: "worktree", kind: "worktree" }),
    ];
    expect(pickRuntimeSupportedEnvironment(environments)?.id).toBe("local");
    expect(pickRuntimeSupportedEnvironment([environments[0]])).toBeNull();
  });

  it("returns null when no environment is runtime-supported", () => {
    const envs: EnvironmentRecord[] = [
      baseEnv({ id: "ssh", kind: "ssh", runtimeSupported: false }),
      baseEnv({
        id: "wsl",
        kind: "wsl",
        runtimeSupported: false,
        runtimeUnavailableReason: "wsl not wired",
      }),
    ];
    expect(pickRuntimeSupportedEnvironment(envs)).toBeNull();
  });

  it("labels all known environment kinds", () => {
    expect(environmentKindLabel("local")).toBe("Local");
    expect(environmentKindLabel("worktree")).toBe("Worktree");
    expect(environmentKindLabel("ssh")).toBe("SSH");
    expect(environmentKindLabel("wsl")).toBe("WSL");
    expect(environmentKindLabel("configuredRemote")).toBe("Configured remote");
  });

  it("groups attention items by typed state", () => {
    const items: AttentionItem[] = [
      makeItem("a", "needsInput"),
      makeItem("b", "failed"),
      makeItem("c", "unread"),
      makeItem("d", "snoozed"),
      makeItem("e", "needsInput"),
    ];
    const grouped = groupAttentionByState(items);
    expect(grouped.needsInput).toHaveLength(2);
    expect(grouped.failed).toHaveLength(1);
    expect(grouped.unread).toHaveLength(1);
    expect(grouped.snoozed).toHaveLength(1);
  });

  it("flags urgent attention when needsInput or failed appear", () => {
    expect(hasUrgentAttention([makeItem("x", "unread")])).toBe(false);
    expect(hasUrgentAttention([makeItem("x", "needsInput")])).toBe(true);
    expect(hasUrgentAttention([makeItem("x", "failed")])).toBe(true);
  });
});

function makeItem(id: string, state: AttentionState): AttentionItem {
  return {
    id,
    kind: "k",
    title: "",
    body: "",
    source: "s",
    createdAt: "u",
    updatedAt: "u",
    state,
    read: false,
  };
}
