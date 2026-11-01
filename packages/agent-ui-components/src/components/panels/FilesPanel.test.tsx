// @vitest-environment happy-dom
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import FilesPanel, { type FileEntry } from "./FilesPanel";
import { useWorkspaceStore } from "@rightkit/agent-ui-core/state/workspaceStore";
import { postWorkspaceTreeExpand } from "@rightkit/agent-ui-core/lib/daemon/workspace-files";

const workspaceTreeMock = vi.hoisted(() => ({
  entries: [{ name: "src", path: "src", kind: "directory" as const }],
  refresh: vi.fn().mockResolvedValue(undefined),
}));
const workspaceWatchMock = vi.hoisted(() => ({
  open: vi.fn().mockResolvedValue({ close: vi.fn() }),
}));

vi.mock("@rightkit/agent-ui-hooks/hooks/useWorkspaceTree", () => ({
  useWorkspaceTree: () => ({
    status: "ready",
    entries: workspaceTreeMock.entries,
    truncated: false,
    lastKnown: false,
    reason: null,
    refresh: workspaceTreeMock.refresh,
  }),
}));

vi.mock("@rightkit/agent-ui-core/lib/daemon/workspace-files", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@rightkit/agent-ui-core/lib/daemon/workspace-files")>()),
  openWorkspaceWatch: workspaceWatchMock.open,
  postWorkspaceTreeExpand: vi.fn().mockResolvedValue({
    workspaceRoot: "/workspace",
    path: "src",
    entries: [{ name: "index.ts", path: "src/index.ts", kind: "file", suffix: ".ts" }],
    truncated: false,
    lastKnown: false,
  }),
}));

// React.StrictMode renders trees twice under happy-dom. The LAST committed
// copy is the live one; helpers grab the tail of each getAllByTestId call.
function pickLast<T extends HTMLElement>(list: T[]) {
  return list[list.length - 1];
}

const READY_TREE: FileEntry[] = [
  { name: "src", children: [{ name: "index.ts", suffix: ".ts" }] },
  { name: "README.md", suffix: ".md" },
];

describe("FilesPanel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useWorkspaceStore.getState().resetForWorkspace("");
  });

  it("loads a directory's real children when it is clicked", async () => {
    render(
      <FilesPanel
        onClose={vi.fn()}
        config={{ url: "http://127.0.0.1:9999", token: "token" }}
        workspaceRoot="/workspace"
        onActivate={vi.fn()}
      />,
    );

    fireEvent.click(await screen.findByText("src"));
    await waitFor(() =>
      expect(postWorkspaceTreeExpand).toHaveBeenCalledWith(
        { url: "http://127.0.0.1:9999", token: "token" },
        "/workspace",
        "src",
      ),
    );
    await waitFor(() => expect(screen.getByText("index.ts")).toBeTruthy());
  });

  it("renders the Files header with maximize + close controls", () => {
    render(<FilesPanel onClose={vi.fn()} status="ready" entries={READY_TREE} />);
    expect(pickLast(screen.getAllByLabelText("Close files panel")) ?? null).toBeTruthy();
    expect(pickLast(screen.getAllByLabelText("Maximize")) ?? null).toBeTruthy();
    expect(screen.getAllByText("Files").length).toBeGreaterThan(0);
  });

  it("renders the Filter files input with the right placeholder", () => {
    render(<FilesPanel onClose={vi.fn()} status="ready" entries={READY_TREE} />);
    const filter = pickLast(screen.getAllByTestId("files-filter"));
    expect(filter?.getAttribute("placeholder")).toBe("Filter files...");
    expect(filter?.getAttribute("aria-label")).toBe("Filter files");
  });

  it("renders the workspace tree when status is ready", () => {
    render(
      <FilesPanel
        onClose={vi.fn()}
        status="ready"
        workspaceRoot="/Users/me/workspace"
        entries={READY_TREE}
      />,
    );
    const tree = pickLast(screen.getAllByTestId("files-tree"));
    expect(tree).toBeTruthy();
    expect(tree?.textContent).toContain("src");
    expect(tree?.textContent).toContain("README.md");
    // Workspace path should be visible — replaces the placeholder hint
    expect(tree?.textContent).not.toContain("No workspace selected");
  });

  it("fires the close callback when the X button is clicked", () => {
    const close = vi.fn();
    render(<FilesPanel onClose={close} status="ready" entries={READY_TREE} />);
    const x = pickLast(screen.getAllByLabelText("Close files panel"));
    expect(x).toBeTruthy();
    fireEvent.click(x as HTMLElement);
    expect(close).toHaveBeenCalledTimes(1);
  });

  it("filters the file list as the user types", () => {
    render(<FilesPanel onClose={vi.fn()} status="ready" entries={READY_TREE} />);
    const filter = pickLast(screen.getAllByTestId("files-filter")) as HTMLInputElement | undefined;
    expect(filter).toBeTruthy();
    if (!filter) return;
    fireEvent.change(filter, { target: { value: "src" } });
    const tree = pickLast(screen.getAllByTestId("files-tree"));
    expect(tree?.textContent).toContain("src");
    expect(tree?.textContent).not.toContain("README.md");
  });

  it("renders an honest loading state and never shows the static placeholder tree", () => {
    render(<FilesPanel onClose={vi.fn()} status="loading" />);
    const loading = screen.getByTestId("files-status-loading");
    expect(loading).toBeTruthy();
    expect(loading.getAttribute("role")).toBe("status");
    expect(loading.textContent).toContain("Loading workspace");
    // The tree must NEVER render the legacy static placeholder names.
    const tree = screen.getByTestId("files-tree");
    expect(tree.textContent).not.toContain(".wake_train");
    expect(tree.textContent).not.toContain(".worktrees");
  });

  it("renders a retry control on error with no static tree", () => {
    const retry = vi.fn();
    render(
      <FilesPanel
        onClose={vi.fn()}
        status="error"
        statusMessage="Connection lost."
        onRetry={retry}
      />,
    );
    const errorBox = screen.getByTestId("files-status-error");
    expect(errorBox.getAttribute("role")).toBe("alert");
    expect(errorBox.textContent).toContain("Connection lost.");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(retry).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId("files-tree").textContent).not.toContain(".wake_train");
  });

  it("renders an open-workspace affordance from the idle empty state", () => {
    const openWorkspace = vi.fn();
    render(<FilesPanel onClose={vi.fn()} status="idle" onOpenWorkspace={openWorkspace} />);
    fireEvent.click(screen.getByRole("button", { name: "Open workspace" }));
    expect(openWorkspace).toHaveBeenCalledTimes(1);
  });

  it("renders an honest unavailable state without exposing retry when none is provided", () => {
    render(<FilesPanel onClose={vi.fn()} status="unavailable" />);
    const unavailable = screen.getByTestId("files-status-unavailable");
    expect(unavailable).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
  });

  it("shows the workspace is empty state when ready but tree is empty", () => {
    render(
      <FilesPanel onClose={vi.fn()} status="ready" workspaceRoot="/Users/me/empty" entries={[]} />,
    );
    const empty = screen.getByTestId("files-empty");
    expect(empty.textContent).toContain("Workspace is empty");
  });

  it("has visible focus rings on chrome controls", () => {
    const { container } = render(
      <FilesPanel onClose={vi.fn()} status="ready" entries={READY_TREE} />,
    );
    const closeBtn = pickLast(screen.getAllByLabelText("Close files panel"));
    expect(closeBtn?.className).toContain("focus-visible:ring-2");
    const filter = pickLast(screen.getAllByTestId("files-filter"));
    expect(filter?.className).toContain("focus-visible:ring-2");
    expect(container.querySelector("[data-testid='files-tree']")).toBeTruthy();
  });

  it("shows the workspace root path under the filter", () => {
    render(
      <FilesPanel
        onClose={vi.fn()}
        status="ready"
        workspaceRoot="/Users/me/workspace"
        entries={READY_TREE}
      />,
    );
    expect(screen.getByText("/Users/me/workspace")).toBeTruthy();
  });
});
