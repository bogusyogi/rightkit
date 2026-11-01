// @vitest-environment happy-dom
import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  getExecutionProcessLogs,
  listExecutionProcesses,
} from "@rightkit/agent-ui-core/lib/daemon";
import type { DaemonConfig, ExecutionProcessRow } from "@rightkit/agent-ui-core/lib/types";
import { usePersistentTerminal } from "./usePersistentTerminal";

vi.mock("@rightkit/agent-ui-core/lib/daemon", () => ({
  createExecutionProcess: vi.fn(),
  getExecutionProcessLogs: vi.fn(),
  killExecutionProcess: vi.fn(),
  listExecutionProcesses: vi.fn(),
  writeExecutionProcessStdin: vi.fn(),
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

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

afterEach(() => vi.clearAllMocks());

describe("usePersistentTerminal", () => {
  it("hydrates the selected session and follows its process logs", async () => {
    vi.mocked(listExecutionProcesses).mockResolvedValue({ processes: [process] });
    vi.mocked(getExecutionProcessLogs).mockResolvedValue({
      process,
      logs: { processId: process.id, output: "ready", byteLength: 5, tailBytes: 5, truncated: false },
    });

    const { result } = renderHook(() =>
      usePersistentTerminal({ config, sessionId: "session-a", workspaceRoot: "/workspace" }),
    );

    await waitFor(() => expect(result.current.selectedProcessId).toBe(process.id));
    await waitFor(() => expect(result.current.logs?.output).toBe("ready"));
    expect(listExecutionProcesses).toHaveBeenCalledWith(config, "/workspace");
    expect(getExecutionProcessLogs).toHaveBeenCalledWith(config, process.id, 128 * 1024);
  });

  it("rejects a late process response after switching sessions", async () => {
    const first = deferred<{ processes: typeof process[] }>();
    vi.mocked(listExecutionProcesses).mockReturnValueOnce(first.promise).mockResolvedValue({ processes: [] });
    const { result, rerender } = renderHook(
      ({ sessionId }) => usePersistentTerminal({ config, sessionId, workspaceRoot: "/workspace" }),
      { initialProps: { sessionId: "session-a" } },
    );

    rerender({ sessionId: "session-b" });
    await act(async () => first.resolve({ processes: [process] }));
    expect(result.current.processes).toEqual([]);
  });

  it("records sent commands for the suggestion history and clears input", async () => {
    vi.mocked(listExecutionProcesses).mockResolvedValue({ processes: [process] });
    vi.mocked(getExecutionProcessLogs).mockResolvedValue({ process, logs: { processId: process.id, output: "", byteLength: 0, tailBytes: 0, truncated: false } });
    const { result } = renderHook(() => usePersistentTerminal({ config, sessionId: "session-a", workspaceRoot: "/workspace" }));

    await waitFor(() => expect(result.current.selectedProcessId).toBe(process.id));
    act(() => result.current.setInput("git status"));
    await act(async () => result.current.sendInput());
    expect(result.current.input).toBe("");
    expect(result.current.history).toContain("git status");
  });
});
