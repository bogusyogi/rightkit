import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import {
  ApprovalMiddlewareDetails,
  ApprovalOriginDetails,
  approvalActionLabel,
  approvalDisplayDetail,
} from "./ApprovalDetails.js";
import type { PendingApproval } from "@rightkit/agent-ui-hooks/hooks/useApprovals";
import type { SessionRow } from "@rightkit/agent-ui-core/lib/types";

function baseApproval(overrides: Partial<PendingApproval> = {}): PendingApproval {
  return {
    id: "approval-1",
    name: "shell.run",
    detail: "rm -rf build",
    middlewareDecisions: [],
    ...overrides,
  };
}

describe("approval display copy", () => {
  it("turns shell JSON into a plain command and human action label", () => {
    expect(approvalDisplayDetail("shell", '{"command":"git status && git diff --stat"}')).toBe(
      "git status && git diff --stat",
    );
    expect(approvalActionLabel("shell")).toBe("Run this command?");
  });
});

describe("ApprovalOriginDetails", () => {
  it("reports the tool as present in the effective session allow-list", () => {
    const approval = baseApproval({ policySource: "session-policy" });
    const selected = {
      allowedTools: ["shell.run"],
      disallowedTools: [],
      toolset: "core",
    } as unknown as SessionRow;

    const html = renderToStaticMarkup(
      <ApprovalOriginDetails pendingApproval={approval} selected={selected} />,
    );

    expect(html).toContain("Active toolset");
    expect(html).toContain("core");
    expect(html).toContain("session-policy");
    expect(html).toContain("is present in the effective session allow-list");
  });

  it("flags a tool blocked by the disallow-list", () => {
    const approval = baseApproval({ name: "network.fetch" });
    const selected = {
      allowedTools: [],
      disallowedTools: ["network.fetch"],
      toolset: "",
    } as unknown as SessionRow;

    const html = renderToStaticMarkup(
      <ApprovalOriginDetails pendingApproval={approval} selected={selected} />,
    );

    expect(html).toContain("No explicit session allow-list is active");
    expect(html).toContain("blocked by the effective session disallow-list");
    expect(html).toContain("default session tools");
  });

  it("falls back to the risk label when no policy source is set", () => {
    const approval = baseApproval({ policySource: undefined, riskLabel: "elevated" });
    const selected = null;

    const html = renderToStaticMarkup(
      <ApprovalOriginDetails pendingApproval={approval} selected={selected} />,
    );

    expect(html).toContain("elevated");
  });
});

describe("ApprovalMiddlewareDetails", () => {
  it("renders nothing when there are no middleware decisions", () => {
    const approval = baseApproval({ middlewareDecisions: [] });
    expect(renderToStaticMarkup(<ApprovalMiddlewareDetails pendingApproval={approval} />)).toBe("");
  });

  it("lists each middleware decision with its phase and reason", () => {
    const approval = baseApproval({
      middlewareDecisions: [
        {
          middlewareId: "mw-1",
          title: "Sandbox guard",
          phase: "pre_tool",
          effect: "block",
          decision: "blocked",
          reason: "workspace is untrusted",
        },
      ],
    });

    const html = renderToStaticMarkup(<ApprovalMiddlewareDetails pendingApproval={approval} />);

    expect(html).toContain("Runtime middleware");
    expect(html).toContain("Sandbox guard");
    expect(html).toContain("workspace is untrusted");
    expect(html).toContain("blocked");
    expect(html).toContain("pre_tool");
  });
});
