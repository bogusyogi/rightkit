import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import QueuedMessageRow from "./QueuedMessageRow";

describe("QueuedMessageRow references", () => {
  it("renders queued skill and repository metadata before delivery", () => {
    render(
      <QueuedMessageRow
        msg={{
          id: "queued-1",
          text: "compare these",
          references: [
            {
              id: "skill:audit",
              kind: "skill",
              destination: "@audit",
              filename: "Audit",
              provenance: {
                source: "composer",
                createdAt: "2026-07-21T00:00:00Z",
                trust: "workspaceTrusted",
              },
            },
            {
              id: "repo:stablyai/orca",
              kind: "repository",
              destination: "https://github.com/stablyai/orca",
              filename: "stablyai/orca",
              provenance: {
                source: "composer",
                createdAt: "2026-07-21T00:00:00Z",
                trust: "user",
              },
              actions: ["open"],
            },
          ],
        }}
        onSteer={vi.fn()}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
      />,
    );

    expect(screen.getByLabelText("Queued references")).toBeTruthy();
    expect(screen.getByText("Audit")).toBeTruthy();
    expect(screen.getByText("stablyai/orca")).toBeTruthy();
    expect(screen.getByText("2 references")).toBeTruthy();
  });
});
