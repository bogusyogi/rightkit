import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import EditedFilesInline from "./EditedFilesInline";
import { INITIAL_RUN_CHANGES_STATE } from "./runChangesStore";
import { resetStore } from "./useRunChangesState";

vi.mock("@rightkit/agent-ui-core/components/Icon", () => ({ default: () => <span aria-hidden="true" /> }));

beforeEach(() => resetStore(INITIAL_RUN_CHANGES_STATE));

describe("EditedFilesInline", () => {
  it("summarizes saved feedback and stale anchors without inventing a second review surface", () => {
    resetStore({
      ...INITIAL_RUN_CHANGES_STATE,
      inventory: {
        runId: "run-1",
        workspaceRoot: "/workspace",
        entries: [
          {
            mutationId: "mutation-1",
            workspaceRoot: "/workspace",
            path: "src/app.tsx",
            originalPath: null,
            classification: "tracked",
            stage: "unstaged",
            additions: 1,
            deletions: 0,
            diff: "",
            truncated: false,
            anchorStatus: "stale",
            anchorOffset: 1,
            capturedAt: "2026-07-21T00:00:00Z",
            fingerprint: "sha256:test",
            sizeBytes: 1,
            modifiedAt: null,
            undo: null,
            lastError: null,
          },
        ],
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
      comments: [
        {
          commentId: "comment-1",
          mutationId: "mutation-1",
          path: "src/app.tsx",
          anchorOffset: 1,
          anchorStatus: "stale",
          kind: "revision",
          body: "Review this line",
          author: "reviewer",
          createdAt: "2026-07-21T00:00:00Z",
          resolvedAt: null,
        },
      ],
      staleAnchors: [
        {
          path: "src/app.tsx",
          mutationId: "mutation-1",
          anchorOffset: 1,
          anchorStatus: "stale",
          observedAt: "2026-07-21T00:00:00Z",
        },
      ],
    });

    render(<EditedFilesInline onOpenReview={vi.fn()} />);

    expect(screen.getByTestId("edited-files-inline-feedback-count")).toHaveTextContent("1 note");
    expect(screen.getByTestId("edited-files-inline-stale-count")).toHaveTextContent("1 stale");
  });
});
