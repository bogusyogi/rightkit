import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import HarnessRadarPanel from "./HarnessRadarPanel";
import type { HarnessRadarSnapshot } from "@rightkit/agent-ui-core/lib/types";

function baseSnapshot(overrides: Partial<HarnessRadarSnapshot> = {}): HarnessRadarSnapshot {
  return {
    sources: [],
    entries: [],
    capabilities: [],
    capabilityMap: {},
    proposals: [],
    changes: [],
    validations: [],
    notes: [],
    ...overrides,
  };
}

const noop = () => {};

describe("HarnessRadarPanel", () => {
  it("shows the empty states when nothing has been imported yet", () => {
    const html = renderToStaticMarkup(
      <HarnessRadarPanel
        snapshot={baseSnapshot()}
        pendingPromotionId={null}
        schedulePending={false}
        onPromoteProposal={noop}
        onScheduleRefresh={noop}
      />,
    );

    expect(html).toContain("No capabilities mapped");
    expect(html).toContain("No radar entries");
  });

  it("tallies watched projects per capability in the capability map", () => {
    const snapshot = baseSnapshot({
      capabilities: ["codeReview"],
      capabilityMap: { codeReview: 3 },
      entries: [
        {
          sourceId: "src-1",
          projectName: "acme/tool",
          repoUrl: "https://github.com/acme/tool",
          category: "linter",
          tags: ["ci"],
          summary: "Runs static analysis in CI.",
        },
      ],
    });

    const html = renderToStaticMarkup(
      <HarnessRadarPanel
        snapshot={snapshot}
        pendingPromotionId={null}
        schedulePending={false}
        onPromoteProposal={noop}
        onScheduleRefresh={noop}
      />,
    );

    expect(html).toContain("3 watched projects");
    expect(html).toContain("acme/tool");
    expect(html).toContain("Runs static analysis in CI.");
  });

  it("shows the source validation status and schedule-refresh pending label", () => {
    const snapshot = baseSnapshot({
      validations: [
        {
          sourceId: "src-1",
          status: "warn",
          entryCount: 4,
          capabilityCount: 2,
          missingCapabilities: [],
          missingCategories: [],
          issues: [{ severity: "warn", code: "stale", message: "snapshot is 9 days old" }],
        },
      ],
    });

    const html = renderToStaticMarkup(
      <HarnessRadarPanel
        snapshot={snapshot}
        pendingPromotionId={null}
        schedulePending={true}
        onPromoteProposal={noop}
        onScheduleRefresh={noop}
      />,
    );

    expect(html).toContain("Source validation");
    expect(html).toContain("warn");
    expect(html).toContain("snapshot is 9 days old");
    expect(html).toContain("Scheduling");
  });

  it("shows Queuing on the promote button for the pending proposal only", () => {
    const snapshot = baseSnapshot({
      proposals: [
        {
          id: "prop-1",
          title: "Adopt Prism router",
          capability: "modelRouting",
          sourceId: "src-1",
          projectNames: ["augmentcode/augment"],
          rationale: "Turn-level cost/capability routing.",
        },
      ],
    });

    const html = renderToStaticMarkup(
      <HarnessRadarPanel
        snapshot={snapshot}
        pendingPromotionId="prop-1"
        schedulePending={false}
        onPromoteProposal={noop}
        onScheduleRefresh={noop}
      />,
    );

    expect(html).toContain("Adopt Prism router");
    expect(html).toContain("Queuing");
    expect(html).toContain("Turn-level cost/capability routing.");
  });
});
