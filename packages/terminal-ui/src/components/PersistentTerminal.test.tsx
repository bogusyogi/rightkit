// @vitest-environment happy-dom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import PersistentTerminal, { TerminalSlot } from "./PersistentTerminal.js";
import usePersistentTerminal from "../hooks/usePersistentTerminal.js";
import type { DaemonConfig, ExecutionProcessRow } from "@rightkit/agent-ui-core/lib/types";

vi.mock("../hooks/usePersistentTerminal.js", () => ({
  default: vi.fn(),
}));

const config = { url: "http://daemon.test", token: "test-token" } as DaemonConfig;
const process: ExecutionProcessRow = {
  id: "process-a",
  command: "sh",
  sessionId: "session-a",
  workspaceRoot: "/workspace",
  backendId: "local",
  kind: "backgroundShell",
  status: "running",
  statusLabel: "Running",
  live: true,
  controlMode: "liveProcess",
};

function terminalState() {
  return {
    processes: [process],
    selectedProcessId: process.id,
    logs: { processId: process.id, output: "persistent output", byteLength: 17, tailBytes: 17, truncated: false },
    input: "",
    loading: false,
    error: null,
    actionPending: null,
    history: [],
    setInput: vi.fn(),
    selectProcess: vi.fn(),
    refresh: vi.fn(),
    createTerminal: vi.fn(),
    sendInput: vi.fn(),
    stopProcess: vi.fn(),
  };
}

function Harness({ hidden = false }: { hidden?: boolean }) {
  return (
    <>
      <div data-terminal-hidden={hidden ? "true" : undefined}>
        <TerminalSlot className="slot" />
      </div>
      <PersistentTerminal config={config} sessionId="session-a" workspaceRoot="/workspace" />
    </>
  );
}

describe("PersistentTerminal", () => {
  beforeEach(() => {
    vi.mocked(usePersistentTerminal).mockReturnValue(terminalState());
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      top: 10,
      left: 20,
      right: 420,
      bottom: 210,
      width: 400,
      height: 200,
      x: 20,
      y: 10,
      toJSON: () => ({}),
    } as DOMRect);
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("follows the slot rect without moving the slot or owning its process", async () => {
    render(<Harness />);
    const overlay = document.querySelector("[data-persistent-terminal]") as HTMLElement;
    await waitFor(() => expect(screen.getByTestId("persistent-terminal-surface")).toBeTruthy());
    expect(overlay.style.position).toBe("fixed");
    expect(overlay.style.top).toBe("10px");
    expect(overlay.style.left).toBe("20px");
    expect(screen.getByText("persistent output")).toBeTruthy();
  });

  it("hides the overlay while retaining its mounted terminal workspace", async () => {
    const view = render(<Harness />);
    await waitFor(() => expect(screen.getByTestId("persistent-terminal-surface")).toBeTruthy());
    const overlay = document.querySelector("[data-persistent-terminal]") as HTMLElement;

    act(() => view.rerender(<Harness hidden />));
    await waitFor(() => expect(overlay.style.visibility).toBe("hidden"));
    expect(overlay.style.opacity).toBe("0");
    expect(screen.getByTestId("persistent-terminal-surface")).toBeTruthy();
  });
});
