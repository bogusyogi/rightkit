import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ReferenceMeta } from "@rightkit/agent-ui-core/lib/types/references";
import { useRightPanelStore } from "@rightkit/agent-ui-core/state/rightPanelStore";
import { useSidePanelStore } from "@rightkit/agent-ui-core/state/sidePanelStore";
import { useWorkspaceStore } from "@rightkit/agent-ui-core/state/workspaceStore";

vi.mock("@rightkit/agent-ui-core/lib/secureStore", () => ({ openExternal: vi.fn().mockResolvedValue(undefined) }));

import { openExternal } from "@rightkit/agent-ui-core/lib/secureStore";
import { performReferenceAction } from "./referenceActions";

function reference(overrides: Partial<ReferenceMeta>): ReferenceMeta {
  return {
    id: "reference-1",
    kind: "workspace_file",
    destination: "src/main.ts",
    provenance: {
      source: "test",
      createdAt: "2026-07-21T00:00:00Z",
      trust: "workspaceTrusted",
    },
    ...overrides,
  };
}

describe("performReferenceAction", () => {
  beforeEach(() => {
    vi.mocked(openExternal).mockClear();
    localStorage.clear();
    useWorkspaceStore.getState().resetForWorkspace("/work/project");
    useRightPanelStore.setState({ tab: "background-tasks" });
    useSidePanelStore.setState({ open: false });
  });

  it("routes repository links through the secure external boundary", async () => {
    await performReferenceAction(
      reference({ kind: "repository", destination: "https://github.com/stablyai/orca" }),
      "open",
    );
    expect(openExternal).toHaveBeenCalledWith("https://github.com/stablyai/orca");

    await performReferenceAction(
      reference({ kind: "repository", destination: "javascript:alert(1)" }),
      "open",
    );
    expect(openExternal).toHaveBeenCalledTimes(1);
  });

  it("routes artifacts and change sets to their bounded desktop surfaces", async () => {
    await performReferenceAction(reference({ kind: "artifact" }), "open");
    expect(useRightPanelStore.getState().tab).toBe("artifacts");
    expect(useSidePanelStore.getState().open).toBe(true);

    await performReferenceAction(reference({ kind: "change_set" }), "preview");
    expect(useRightPanelStore.getState().tab).toBe("review");
  });
});
