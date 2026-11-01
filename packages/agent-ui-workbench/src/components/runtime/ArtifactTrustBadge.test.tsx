import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import ArtifactTrustBadge from "./ArtifactTrustBadge";
import type { ArtifactRow } from "@rightkit/agent-ui-core/lib/types";

function baseArtifact(overrides: Partial<ArtifactRow> = {}): ArtifactRow {
  return {
    id: "artifact-1",
    path: "out/report.md",
    kind: "markdown",
    contentHash: "hash-1",
    shippable: false,
    producer: "agent",
    provenance: "generated",
    approvals: [],
    meta: "{}",
    createdAt: "2026-07-01T00:00:00Z",
    ...overrides,
  };
}

describe("ArtifactTrustBadge", () => {
  it("labels a shippable artifact as Shippable with the approval count in the tooltip", () => {
    const artifact = baseArtifact({
      shippable: true,
      approvals: [{ approver: "adrian", boundHash: "hash-1", createdAt: "2026-07-01T00:00:00Z" }],
    });
    const html = renderToStaticMarkup(<ArtifactTrustBadge artifact={artifact} />);

    expect(html).toContain("Shippable");
    expect(html).toContain("Approval count: 1");
    expect(html).toContain("border-accent-2");
  });

  it("flags a verdict bound to a stale content hash as Stale verdict", () => {
    const artifact = baseArtifact({
      contentHash: "hash-2",
      verdict: { passed: true, juror: "sonnet", boundHash: "hash-1", summary: "ok" },
    });
    const html = renderToStaticMarkup(<ArtifactTrustBadge artifact={artifact} />);

    expect(html).toContain("Stale verdict");
    expect(html).toContain("border-warning");
  });

  it("shows Passed when the verdict is bound to the current content hash", () => {
    const artifact = baseArtifact({
      contentHash: "hash-1",
      verdict: { passed: true, juror: "sonnet", boundHash: "hash-1", summary: "looks good" },
    });
    const html = renderToStaticMarkup(<ArtifactTrustBadge artifact={artifact} />);

    expect(html).toContain("Passed");
  });
});
