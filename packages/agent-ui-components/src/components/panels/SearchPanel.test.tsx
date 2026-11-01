// @vitest-environment happy-dom
import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import SearchPanel, { debounce, formatResultCount, highlightMatch } from "./SearchPanel.js";
import type { WorkspaceSearchHit } from "@rightkit/agent-ui-core/lib/types/workspace-files";

const searchHookMock = vi.hoisted(() => vi.fn());
vi.mock("@rightkit/agent-ui-hooks/hooks/useWorkspaceSearch", () => ({ useWorkspaceSearch: searchHookMock }));

const CONFIG = { url: "http://127.0.0.1:9999", token: "token" };
const HIT: WorkspaceSearchHit = {
  path: "src/main.ts",
  name: "main.ts",
  line: 12,
  column: 7,
  snippet: "const answer = searchTerm;",
  kind: "content",
};

function readySearch(hits: WorkspaceSearchHit[] = [HIT]) {
  return {
    status: "ready" as const,
    hits,
    cancelled: false,
    truncated: false,
    reason: null,
    cancel: vi.fn(),
  };
}

afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

beforeEach(() => {
  vi.useRealTimers();
  searchHookMock.mockReturnValue({
    status: "idle",
    hits: [],
    cancelled: false,
    truncated: false,
    reason: null,
    cancel: vi.fn(),
  });
});

describe("SearchPanel pure helpers", () => {
  it("splits a basic match in the middle of a line", () => {
    expect(highlightMatch("hello world foo", 6, 11)).toEqual({
      before: "hello ",
      match: "world",
      after: " foo",
    });
  });

  it("handles match at the start, end, and full line", () => {
    expect(highlightMatch("hello world", 0, 5)).toEqual({
      before: "",
      match: "hello",
      after: " world",
    });
    expect(highlightMatch("hello world", 6, 11)).toEqual({
      before: "hello ",
      match: "world",
      after: "",
    });
    expect(highlightMatch("hello", 0, 5)).toEqual({ before: "", match: "hello", after: "" });
  });

  it("handles an empty line", () => {
    expect(highlightMatch("", 0, 0)).toEqual({ before: "", match: "", after: "" });
  });

  it("formats singular and plural result counts", () => {
    expect(formatResultCount(42, 7)).toBe("42 results in 7 files");
    expect(formatResultCount(1, 1)).toBe("1 result in 1 file");
    expect(formatResultCount(0, 0)).toBe("0 results in 0 files");
  });

  it("debounces calls and preserves the latest arguments", () => {
    vi.useFakeTimers();
    const fn = vi.fn();
    const delayed = debounce(fn, 100);
    delayed("a", "b");
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(100);
    expect(fn).toHaveBeenCalledWith("a", "b");
  });

  it("resets the timer on rapid calls", () => {
    vi.useFakeTimers();
    const fn = vi.fn();
    const delayed = debounce(fn, 100);
    delayed();
    vi.advanceTimersByTime(50);
    delayed();
    vi.advanceTimersByTime(50);
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(50);
    expect(fn).toHaveBeenCalledTimes(1);
  });
});

describe("SearchPanel", () => {
  it("renders an honest idle state and panel controls", () => {
    const close = vi.fn();
    const maximize = vi.fn();
    render(
      <SearchPanel
        config={null}
        workspaceRoot=""
        onClose={close}
        onMaximize={maximize}
      />,
    );

    expect(screen.getByTestId("search-panel")).toBeTruthy();
    expect(screen.getByText("Connect to a daemon to search the workspace.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Maximize search panel" }));
    fireEvent.click(screen.getByRole("button", { name: "Close search panel" }));
    expect(maximize).toHaveBeenCalledTimes(1);
    expect(close).toHaveBeenCalledTimes(1);
  });

  it("groups results by file, highlights the query, and preserves hit metadata", () => {
    vi.useFakeTimers();
    const openFile = vi.fn();
    searchHookMock.mockReturnValue(readySearch());
    render(
      <SearchPanel
        config={CONFIG}
        workspaceRoot="/workspace"
        onClose={vi.fn()}
        onOpenFile={openFile}
      />,
    );

    const input = screen.getByTestId("search-input");
    fireEvent.change(input, { target: { value: "searchTerm" } });
    act(() => vi.advanceTimersByTime(180));

    expect(screen.getByText("src/main.ts")).toBeTruthy();
    expect(screen.getByText("searchTerm")).toBeTruthy();
    fireEvent.click(screen.getByTestId("search-hit-src%2Fmain.ts-12-0"));
    expect(openFile).toHaveBeenCalledWith(HIT);
  });

  it("collapses and expands a file group", () => {
    vi.useFakeTimers();
    searchHookMock.mockReturnValue(readySearch());
    render(
      <SearchPanel
        config={CONFIG}
        workspaceRoot="/workspace"
        onClose={vi.fn()}
        onOpenFile={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("search-input"), { target: { value: "answer" } });
    act(() => vi.advanceTimersByTime(180));

    const group = screen.getByTestId("search-file-src%2Fmain.ts");
    expect(group.getAttribute("aria-expanded")).toBe("true");
    fireEvent.click(group);
    expect(group.getAttribute("aria-expanded")).toBe("false");
  });

  it("shows a retryable error state", () => {
    vi.useFakeTimers();
    searchHookMock.mockReturnValue({
      ...readySearch([]),
      status: "error" as const,
      reason: "Connection lost.",
    });
    render(
      <SearchPanel
        config={CONFIG}
        workspaceRoot="/workspace"
        onClose={vi.fn()}
        onOpenFile={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("search-input"), { target: { value: "needle" } });
    act(() => vi.advanceTimersByTime(180));
    expect(screen.getByTestId("search-status").textContent).toContain("Connection lost.");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(searchHookMock).toHaveBeenCalled();
  });

  it("exposes the hook cancellation control while searching", () => {
    vi.useFakeTimers();
    const cancel = vi.fn();
    searchHookMock.mockReturnValue({
      ...readySearch([]),
      status: "loading" as const,
      cancel,
    });
    render(
      <SearchPanel
        config={CONFIG}
        workspaceRoot="/workspace"
        onClose={vi.fn()}
        onOpenFile={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("search-input"), { target: { value: "needle" } });
    act(() => vi.advanceTimersByTime(180));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(cancel).toHaveBeenCalledTimes(1);
  });
});
