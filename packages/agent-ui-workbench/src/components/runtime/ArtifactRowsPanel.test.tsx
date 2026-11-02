import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { ArtifactRow } from "@rightkit/agent-ui-core/lib/types";
import ArtifactRowsPanel from "./ArtifactRowsPanel.js";

const artifact: ArtifactRow = {
  id: "artifact-1",
  path: "docs/report.md",
  kind: "markdown",
  contentHash: "sha256:abc",
  shippable: false,
  producer: "agent",
  provenance: "session-1",
  verdict: undefined,
  approvals: [],
  meta: "",
  createdAt: "2024-01-01T00:00:00Z",
};

afterEach(cleanup);

describe("ArtifactRowsPanel", () => {
  it("opens a row through the owning persistent artifact surface", () => {
    const onOpen = vi.fn();
    render(<ArtifactRowsPanel artifacts={[artifact]} onOpen={onOpen} />);

    fireEvent.click(screen.getByTestId("artifact-row-artifact-1-open"));
    expect(onOpen).toHaveBeenCalledWith(artifact);
  });

  it("keeps reveal separate from the artifact open action", () => {
    const onReveal = vi.fn();
    render(<ArtifactRowsPanel artifacts={[artifact]} onReveal={onReveal} />);

    fireEvent.click(screen.getByTestId("artifact-row-artifact-1-reveal"));
    expect(onReveal).toHaveBeenCalledWith("docs/report.md");
  });

  it("renders the empty state without inventing an artifact", () => {
    render(<ArtifactRowsPanel artifacts={[]} />);
    expect(screen.getByText("No artifacts yet")).toBeTruthy();
  });
});
