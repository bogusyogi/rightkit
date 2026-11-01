import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import ChangedFilesCard from "./ChangedFilesCard.js";
import { INITIAL_RUN_CHANGES_STATE } from "./runChangesStore.js";
import { resetStore } from "./useRunChangesState.js";

const entry = {
  mutationId: "mutation-1",
  workspaceRoot: "/workspace",
  path: "src/app.tsx",
  originalPath: null,
  classification: "tracked" as const,
  stage: "unstaged" as const,
  additions: 7,
  deletions: 2,
  diff: "@@ -1 +1 @@",
  truncated: false,
  anchorStatus: "live" as const,
  anchorOffset: 0,
  capturedAt: "2026-07-21T00:00:00Z",
  fingerprint: "sha256:test",
  sizeBytes: 42,
  modifiedAt: null,
  undo: { fingerprint: "sha256:test", body: "before", note: "restore" },
  lastError: null,
};

beforeEach(() => {
  resetStore({
    ...INITIAL_RUN_CHANGES_STATE,
    runId: "run-1",
    workspaceRoot: "/workspace",
    inventory: {
      runId: "run-1",
      workspaceRoot: "/workspace",
      entries: [entry],
      truncated: false,
      git: {
        present: true,
        headSha: "abc123",
        branch: "main",
        indexFingerprint: null,
        untrackedCount: 0,
      },
      lastCaptureAt: "2026-07-21T00:00:00Z",
    },
    summary: {
      runId: "run-1",
      workspaceRoot: "/workspace",
      entryCount: 1,
      truncated: false,
      countsByClassification: { tracked: 1 },
      firstPaths: ["src/app.tsx"],
      lastCaptureAt: "2026-07-21T00:00:00Z",
      git: {
        present: true,
        headSha: "abc123",
        branch: "main",
        indexFingerprint: null,
        untrackedCount: 0,
      },
    },
  });
});

describe("ChangedFilesCard", () => {
  it("stays a single compact summary row until explicitly expanded", () => {
    render(<ChangedFilesCard config={null} workspaceRoot="/workspace" onOpenReview={() => {}} />);

    expect(screen.getByText("Edited 1 file")).toBeInTheDocument();
    expect(screen.getByText("+7")).toBeInTheDocument();
    expect(screen.getByText("-2")).toBeInTheDocument();
    expect(screen.queryByText("src/app.tsx")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Review" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Show 1 edited file" }));
    expect(screen.getByText("src/app.tsx")).toBeInTheDocument();
  });
});
