import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import WorkspaceTrustGate from "@rightkit/agent-ui-core/components/app/WorkspaceTrustGate";
import type { WorkspaceTrustStatus } from "@rightkit/agent-ui-core/lib/types";

function baseTrust(overrides: Partial<WorkspaceTrustStatus> = {}): WorkspaceTrustStatus {
  return {
    workspaceRoot: "/repo",
    trust: "sandboxed",
    trusted: false,
    requiresTrust: true,
    ...overrides,
  };
}

describe("WorkspaceTrustGate", () => {
  it("shows the workspace root and current trust level", () => {
    const html = renderToStaticMarkup(
      <WorkspaceTrustGate
        workspaceRoot="/repo"
        trust={baseTrust({ trust: "locked" })}
        pending={null}
        onSetWorkspaceTrust={() => {}}
      />,
    );

    expect(html).toContain("/repo");
    expect(html).toContain('data-testid="workspace-trust-gate"');
    expect(html).toContain("This project is currently locked");
    expect(html).toContain("Trust");
    expect(html).toContain("Sandbox");
    expect(html).toContain("Lock");
    expect((html.match(/aria-pressed="false"/g) ?? []).length).toBe(3);
    expect(html).not.toContain("border-accent");
  });

  it("disables all trust action buttons while a change is pending", () => {
    const html = renderToStaticMarkup(
      <WorkspaceTrustGate
        workspaceRoot="/repo"
        trust={baseTrust()}
        pending="trusted"
        onSetWorkspaceTrust={() => {}}
      />,
    );

    expect((html.match(/disabled=""/g) ?? []).length).toBe(3);
    expect(html).toContain("Saving…");
  });

  it("leaves the action buttons enabled when nothing is pending", () => {
    const html = renderToStaticMarkup(
      <WorkspaceTrustGate
        workspaceRoot="/repo"
        trust={baseTrust()}
        pending={null}
        onSetWorkspaceTrust={() => {}}
      />,
    );

    expect(html).not.toContain('disabled=""');
  });
});
