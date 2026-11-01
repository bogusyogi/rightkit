// @vitest-environment happy-dom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import TerminalPane from "./TerminalPane.js";
import type { ExecutionProcessRow, SessionRow } from "@rightkit/agent-ui-core/lib/types";

const WIDTH_KEY = "cr.terminalPane.width.v1";

const session: SessionRow = {
  id: "session-1",
  title: "Harden the daemon boundary",
} as SessionRow;

const processes: ExecutionProcessRow[] = [
  {
    id: "proc-1",
    command: "npm run dev",
    kind: "terminal",
    backendId: "local",
    statusLabel: "Running",
    controlMode: "interactive",
  } as ExecutionProcessRow,
];

function renderPane(overrides: Partial<React.ComponentProps<typeof TerminalPane>> = {}) {
  return render(
    <TerminalPane
      selectedSession={session}
      workspaceRoot="/Users/adrian/coderight"
      processes={processes}
      selectedProcessId="proc-1"
      logs={{ output: "hello" } as never}
      input=""
      actionPending={null}
      loading={false}
      error={null}
      onSelectProcess={() => {}}
      onInput={() => {}}
      onCreateTerminal={() => {}}
      onRefresh={() => {}}
      onSendInput={() => {}}
      onStopProcess={() => {}}
      {...overrides}
    />,
  );
}

beforeEach(() => {
  localStorage.clear();
  Object.defineProperty(window, "innerWidth", { writable: true, configurable: true, value: 1280 });
});

afterEach(cleanup);

describe("TerminalPane resize", () => {
  it("renders the default width and a keyboard-accessible separator", () => {
    renderPane();
    const separator = screen.getByRole("separator", { name: "Resize terminal panel" });
    expect(separator.getAttribute("aria-orientation")).toBe("vertical");
    expect(separator.getAttribute("aria-valuenow")).toBe("320");
    expect(separator.getAttribute("aria-valuemin")).toBe("260");
    expect(separator.tabIndex).toBe(0);
  });

  it("drags the separator to change width and clamps at the min", () => {
    renderPane();
    const separator = screen.getByRole("separator", { name: "Resize terminal panel" });

    fireEvent.pointerDown(separator, { clientX: 320 });
    fireEvent.pointerMove(window, { clientX: 420 });
    fireEvent.pointerUp(window);
    expect(separator.getAttribute("aria-valuenow")).toBe("420");

    fireEvent.pointerDown(separator, { clientX: 420 });
    fireEvent.pointerMove(window, { clientX: -5000 });
    fireEvent.pointerUp(window);
    expect(separator.getAttribute("aria-valuenow")).toBe("260");
  });

  it("clamps the max width to a safe fraction of a 900px-wide window", () => {
    Object.defineProperty(window, "innerWidth", { writable: true, configurable: true, value: 900 });
    renderPane();
    const separator = screen.getByRole("separator", { name: "Resize terminal panel" });

    fireEvent.pointerDown(separator, { clientX: 320 });
    fireEvent.pointerMove(window, { clientX: 5000 });
    fireEvent.pointerUp(window);

    const valueNow = Number(separator.getAttribute("aria-valuenow"));
    const valueMax = Number(separator.getAttribute("aria-valuemax"));
    expect(valueMax).toBeLessThanOrEqual(405);
    expect(valueNow).toBe(valueMax);
    expect(valueNow).toBeLessThan(900 * 0.5);
  });

  it("resizes with ArrowLeft / ArrowRight when the separator is focused", () => {
    renderPane();
    const separator = screen.getByRole("separator", { name: "Resize terminal panel" });
    separator.focus();

    fireEvent.keyDown(separator, { key: "ArrowRight" });
    expect(separator.getAttribute("aria-valuenow")).toBe("336");

    fireEvent.keyDown(separator, { key: "ArrowLeft" });
    fireEvent.keyDown(separator, { key: "ArrowLeft" });
    expect(separator.getAttribute("aria-valuenow")).toBe("304");
  });

  it("persists the resized width across a remount", () => {
    const { unmount } = renderPane();
    const separator = screen.getByRole("separator", { name: "Resize terminal panel" });

    fireEvent.pointerDown(separator, { clientX: 320 });
    fireEvent.pointerMove(window, { clientX: 380 });
    fireEvent.pointerUp(window);
    expect(localStorage.getItem(WIDTH_KEY)).toBe("380");

    unmount();
    renderPane();
    const restored = screen.getByRole("separator", { name: "Resize terminal panel" });
    expect(restored.getAttribute("aria-valuenow")).toBe("380");
  });

  it("preserves workspace/session context, scrolling log, and the stop-process confirm flow", async () => {
    renderPane();
    expect(screen.getByText("/Users/adrian/coderight")).toBeTruthy();
    expect(screen.getByText("Session: Harden the daemon boundary")).toBeTruthy();
    expect(screen.getByText("hello")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /Stop/ }));
    expect(screen.getByText("Stop terminal process?")).toBeTruthy();
  });

  it("renders a named-action empty state in the output pane when no process is active", () => {
    renderPane({ processes: [], selectedProcessId: null, logs: null });

    const empty = screen.getByTestId("terminal-pane-empty");
    expect(empty.textContent).toMatch(/No terminal session yet/);

    const openAction = screen.getByTestId("terminal-empty-open");
    expect(openAction.textContent).toMatch(/Open terminal/);
    expect(openAction.tagName).toBe("BUTTON");
  });

  it("truncates a long process command in the detail header and exposes the full value via title", () => {
    const longCommand =
      "pnpm exec vitest run src/components/TerminalPane.test.tsx --reporter=verbose --coverage --watch=false";
    renderPane({
      processes: [{ ...processes[0], command: longCommand }],
    });

    const matches = screen.getAllByTitle(longCommand);
    const headerTitle = matches.find((node) => node.className.includes("truncate"));
    expect(headerTitle).toBeTruthy();
    expect(headerTitle?.textContent).toBe(longCommand);
  });

  it("keeps Stop, Retry, and Send action buttons non-shrinking so they stay visible next to truncated content", () => {
    renderPane({ error: "connection refused" });

    const stopButton = screen.getByRole("button", { name: /Stop/ });
    expect(stopButton.className).toContain("shrink-0");

    const retryButton = screen.getByRole("button", { name: "Retry" });
    expect(retryButton.className).toContain("shrink-0");

    const sendButton = screen.getByRole("button", { name: /Send/ });
    expect(sendButton.className).toContain("shrink-0");
  });
});
