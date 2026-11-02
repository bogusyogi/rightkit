import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

import type { SubAgentState } from "@rightkit/agent-ui-core/hooks/useSubAgents";
import type { TaskRow } from "@rightkit/agent-ui-core/lib/types";
import { ExecutionGraphPane } from "./ExecutionGraphPane.js";

describe("ExecutionGraphPane", () => {
  it("joins daemon task & agent projections with dependency, route, & activity state", () => {
    const task = {
      id: "verify",
      title: "Verify result",
      deps: ["build"],
      doneWhen: "checks pass",
      doneWhenPredicates: [],
      state: "blocked",
      category: "verification",
      estimatedDuration: "2m",
      assignedRole: "reviewer",
      escalationPath: "root",
      openQuestions: [],
      createdAt: "2026-08-26T00:00:00Z",
      updatedAt: "2026-08-26T00:00:01Z",
      taskStorePath: "ignored",
    } satisfies TaskRow;
    const agent = {
      subSessionId: "worker-1",
      role: "coder",
      taskTitle: "Build change",
      status: "running",
      startedAt: 1,
      model: "provider/model",
      toolUses: 2,
      lastTools: ["apply_patch"],
      elapsedMs: 4_000,
    } satisfies SubAgentState;

    const html = renderToStaticMarkup(
      <ExecutionGraphPane
        tasks={[task]}
        agents={[agent]}
        costUsd={0.25}
        onClose={vi.fn()}
        onStopAgent={vi.fn()}
        onOpenAgentSession={vi.fn()}
      />,
    );

    expect(html).toContain("daemon truth");
    expect(html).toContain("Verify result");
    expect(html).toContain("blocked by build");
    expect(html).toContain("provider/model");
    expect(html).toContain("Latest: apply_patch");
    expect(html).toContain("$0.2500");
  });
});
