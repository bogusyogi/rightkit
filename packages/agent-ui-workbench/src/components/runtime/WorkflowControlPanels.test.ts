import { describe, expect, it } from "vitest";
import { inboxKindLabel, summarizeWorkflowVersion } from "./WorkflowControlPanels";

describe("workflow review inbox labels", () => {
  it("names each durable attention kind", () => {
    expect(inboxKindLabel("approval")).toBe("Approval required");
    expect(inboxKindLabel("uncertain_effect")).toBe("Effect outcome uncertain");
    expect(inboxKindLabel("deliverable")).toBe("Deliverable ready");
  });
  it("hydrates workspace, location, capabilities, budgets, trigger, and version", () => {
    const detail = summarizeWorkflowVersion(
      {
        id: "v1",
        versionNumber: 3,
        publicationState: "published",
        definitionJson: JSON.stringify({
          definition: { owner_scope: "workspace:/tmp/demo" },
          nodes: [
            {
              capabilities: ["fs.read"],
              retry: { max_attempts: 2 },
              config: { max_runtime_ms: 5000 },
            },
          ],
        }),
        compiledPlanJson: "{}",
        createdAt: "now",
      },
      { id: "t1", kind: "cron", enabled: false },
    );
    expect(detail).toMatchObject({
      version: "v3 · published",
      trigger: "cron · paused",
      workspace: "workspace:/tmp/demo",
      location: "/tmp/demo",
      capabilities: "fs.read",
    });
    expect(detail.budgets).toContain("max_runtime_ms=5000");
  });
});
