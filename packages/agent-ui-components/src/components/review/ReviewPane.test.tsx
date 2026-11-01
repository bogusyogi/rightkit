import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import ReviewPane from "./ReviewPane.js";
import { INITIAL_RUN_CHANGES_STATE, reducer } from "./runChangesStore.js";
import { resetStore } from "./useRunChangesState.js";
import { getDiffBody, recordRevisionComment } from "./runChangesClient.js";
import { stagePaths, unstagePaths } from "@rightkit/agent-ui-core/lib/daemon/delivery-environments-attention";

vi.mock("./runChangesClient.js", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./runChangesClient.js")>()),
  getDiffBody: vi.fn().mockResolvedValue({
    path: "src/app.tsx",
    rows: [
      { kind: "add", text: "new line", oldLine: null, newLine: 2 },
    ],
    truncated: false,
    nextCursor: null,
    hunkDecisions: [],
    search: null,
  }),
  recordRevisionComment: vi.fn().mockResolvedValue({
    commentId: "comment-1",
    anchorStatus: "live",
    stale: false,
  }),
}));

vi.mock("@rightkit/agent-ui-core/lib/daemon/delivery-environments-attention", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@rightkit/agent-ui-core/lib/daemon/delivery-environments-attention")>()),
  stagePaths: vi.fn().mockResolvedValue({ ok: true }),
  unstagePaths: vi.fn().mockResolvedValue({ ok: true }),
}));

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
  anchorOffset: 12,
  capturedAt: "2026-07-21T00:00:00Z",
  fingerprint: "sha256:test",
  sizeBytes: 42,
  modifiedAt: null,
  undo: { fingerprint: "sha256:test", body: "before", note: "restore" },
  lastError: null,
};

const inventory = {
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
};

beforeEach(() => {
  vi.clearAllMocks();
  resetStore({
    ...INITIAL_RUN_CHANGES_STATE,
    runId: "run-1",
    workspaceRoot: "/workspace",
    inventory,
    selectedPath: entry.path,
  });
});

describe("ReviewPane", () => {
  it("renders an inert empty state before a run exists", () => {
    resetStore();
    render(<ReviewPane config={null} workspaceRoot="/workspace" />);
    expect(screen.getByText("No completed run to review yet.")).toBeInTheDocument();
  });

  it("clears the previous run's review cache when a new run starts", () => {
    const next = reducer(
      {
        ...INITIAL_RUN_CHANGES_STATE,
        runId: "run-old",
        workspaceRoot: "/workspace",
        inventory,
        selectedPath: entry.path,
        search: "stale",
        comments: [
          {
            commentId: "old-comment",
            mutationId: entry.mutationId,
            path: entry.path,
            anchorOffset: 12,
            anchorStatus: "live",
            kind: "revision",
            body: "old",
            author: "reviewer",
            createdAt: "2026-07-21T00:00:00Z",
            resolvedAt: null,
          },
        ],
      },
      { type: "capture/start", runId: "run-new", workspaceRoot: "/workspace" },
    );

    expect(next.runId).toBe("run-new");
    expect(next.inventory).toBeNull();
    expect(next.selectedPath).toBeNull();
    expect(next.search).toBe("");
    expect(next.comments).toEqual([]);
  });

  it("loads the initial diff and refreshes it with the current search query", async () => {
    resetStore({
      ...INITIAL_RUN_CHANGES_STATE,
      runId: "run-1",
      workspaceRoot: "/workspace",
      inventory,
      selectedPath: null,
    });
    render(
      <ReviewPane
        config={{ url: "http://127.0.0.1:9999", token: "token" }}
        workspaceRoot="/workspace"
      />,
    );

    await waitFor(() =>
      expect(getDiffBody).toHaveBeenCalledWith(
        expect.anything(),
        expect.objectContaining({ path: entry.path, search: null }),
      ),
    );
    fireEvent.change(screen.getByTestId("review-pane-search"), {
      target: { value: "needle" },
    });
    await waitFor(() =>
      expect(getDiffBody).toHaveBeenCalledWith(
        expect.anything(),
        expect.objectContaining({ path: entry.path, search: "needle" }),
      ),
    );
  });

  it("anchors line feedback to the displayed file line", async () => {
    render(
      <ReviewPane
        config={{ url: "http://127.0.0.1:9999", token: "token" }}
        workspaceRoot="/workspace"
      />,
    );

    await waitFor(() => expect(screen.getByTestId("review-pane-line-feedback")).toBeInTheDocument());
    fireEvent.click(screen.getByTestId("review-pane-line-feedback"));
    expect(screen.getByTestId("review-pane-feedback-quote")).toHaveTextContent("new line");
    fireEvent.change(screen.getByTestId("review-pane-comment-input"), {
      target: { value: "Please keep this behavior." },
    });
    fireEvent.click(screen.getByTestId("review-pane-comment-submit"));

    await waitFor(() =>
      expect(recordRevisionComment).toHaveBeenCalledWith(
        { url: "http://127.0.0.1:9999", token: "token" },
        expect.objectContaining({ path: "src/app.tsx", anchorOffset: 1 }),
      ),
    );
  });

  it("executes real stage and unstage operations before changing visible state", async () => {
    render(
      <ReviewPane
        config={{ url: "http://127.0.0.1:9999", token: "token" }}
        workspaceRoot="/workspace"
      />,
    );

    fireEvent.click(screen.getByTestId("review-pane-stage"));
    await waitFor(() =>
      expect(stagePaths).toHaveBeenCalledWith(
        { url: "http://127.0.0.1:9999", token: "token" },
        "/workspace",
        ["src/app.tsx"],
      ),
    );
    await waitFor(() => expect(getDiffBody).toHaveBeenCalled());
    expect(screen.getByText("staged")).toBeInTheDocument();

    fireEvent.click(screen.getByTestId("review-pane-unstage"));
    await waitFor(() => expect(unstagePaths).toHaveBeenCalled());
    expect(screen.getByText("unstaged")).toBeInTheDocument();
  });

  it("turns durable line notes into one composer-ready revision request", () => {
    const onDraft = vi.fn();
    const onClose = vi.fn();
    resetStore({
      ...INITIAL_RUN_CHANGES_STATE,
      runId: "run-1",
      workspaceRoot: "/workspace",
      inventory,
      comments: [
        {
          commentId: "comment-1",
          mutationId: entry.mutationId,
          path: entry.path,
          anchorOffset: 12,
          anchorStatus: "live",
          kind: "revision",
          body: "Keep the previous fallback.",
          author: "reviewer",
          createdAt: "2026-07-21T00:00:00Z",
          resolvedAt: null,
        },
      ],
    });

    render(
      <ReviewPane
        config={null}
        workspaceRoot="/workspace"
        onDraftRevisionRequest={onDraft}
        onRequestClose={onClose}
      />,
    );
    fireEvent.click(screen.getByTestId("review-pane-draft-revision"));

    expect(onDraft).toHaveBeenCalledWith(
      expect.stringContaining("src/app.tsx (anchor 12, live): Keep the previous fallback."),
    );
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("windows large file inventories instead of mounting every row", () => {
    const entries = Array.from({ length: 1_000 }, (_, index) => ({
      ...entry,
      mutationId: `mutation-${index}`,
      path: `src/file-${index}.tsx`,
    }));
    resetStore({
      ...INITIAL_RUN_CHANGES_STATE,
      runId: "run-large",
      workspaceRoot: "/workspace",
      inventory: { ...inventory, runId: "run-large", entries },
    });

    render(<ReviewPane config={null} workspaceRoot="/workspace" virtualizationThreshold={100} />);

    expect(screen.getByTestId("review-pane-virtualized-tree")).toBeInTheDocument();
    expect(screen.getAllByTestId("review-pane-entry").length).toBeLessThan(1_000);
  });
});
