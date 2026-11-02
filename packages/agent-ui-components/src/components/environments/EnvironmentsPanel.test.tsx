// D4 — EnvironmentsPanel component tests.

import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";

vi.mock("@rightkit/agent-ui-core/state/environmentsStore", () => {
  const state = {
    list: {
      selectedId: "local",
      environments: [
        {
          id: "local",
          label: "Local",
          kind: "local",
          workspaceRoot: "/Users/me/code",
          trust: "trusted",
          setupState: "ready",
          runtimeSupported: true,
          runtimeUnavailableReason: null,
          providers: [],
        },
        {
          id: "ssh",
          label: "SSH",
          kind: "ssh",
          workspaceRoot: "/Users/me/code",
          trust: "trusted",
          setupState: "unsupported",
          runtimeSupported: false,
          runtimeUnavailableReason: "SSH executor not wired",
          providers: [],
        },
      ],
      generatedAt: "unix:1",
    },
    listPending: false,
    selectedId: "local",
    fallbackId: null,
    testResults: {},
    scripts: [
      {
        id: "local.bootstrap",
        version: 1,
        label: "Local bootstrap",
        environmentKind: "local",
        projectPolicy: "default",
        steps: [
          {
            id: "git-status",
            label: "Confirm a clean git working tree",
            command: "git",
            args: ["status", "--porcelain"],
          },
        ],
      },
    ],
    scriptsPending: false,
    scriptRuns: {},
    actionPending: null,
    error: null,
    reset: vi.fn(),
    loadList: vi.fn(),
    select: vi.fn(() => Promise.resolve(true)),
    test: vi.fn(() =>
      Promise.resolve({
        environmentId: "local",
        reachable: true,
        latencyMs: 12,
        testedAt: "unix:1",
        command: "git status --porcelain",
        exitCode: 0,
        stdoutTail: "",
        stderrTail: "",
      }),
    ),
    loadScripts: vi.fn(),
    runScript: vi.fn(() =>
      Promise.resolve({
        runId: "setup-run-1",
        scriptId: "local.bootstrap",
        scriptVersion: 1,
        workspaceRoot: "/Users/me/code",
        environmentKind: "local",
        state: "completed",
        startedAt: "unix:1",
        finishedAt: "unix:2",
        exitCode: 0,
        logPath: "/Users/me/code/.coderight/environments/runs/setup-run-1.log",
        logTail: [{ ts: "unix:1", stream: "system", line: "step git-status" }],
        cancellable: false,
      }),
    ),
    cancelRun: vi.fn(() => Promise.resolve(true)),
    rerunRun: vi.fn(() => Promise.resolve(null)),
    loadRunLogs: vi.fn(() => Promise.resolve(null)),
    getRecord: () => null,
  };
  const useEnvironmentsStore = (selector: (s: typeof state) => unknown) => selector(state);
  useEnvironmentsStore.getState = () => state;
  return { useEnvironmentsStore };
});

import EnvironmentsPanel from "./EnvironmentsPanel.js";

const baseConfig = { url: "http://127.0.0.1:1", token: "x" };

describe("EnvironmentsPanel", () => {
  it("renders the typed environment list and honest runtime-support badges", async () => {
    render(<EnvironmentsPanel config={baseConfig} workspaceRoot="/Users/me/code" />);
    expect(screen.getByTestId("environment-local").textContent).toContain("Local");
    expect(screen.getByTestId("runtime-supported-local").textContent).toContain("ready");
    expect(screen.getByTestId("runtime-supported-ssh").textContent).toContain("unavailable");
    expect(screen.getByText(/SSH executor not wired/)).toBeTruthy();
  });

  it("keeps setup detail behind a disclosure while preserving the active runtime", () => {
    render(<EnvironmentsPanel config={baseConfig} workspaceRoot="/Users/me/code" />);
    expect(screen.getByText(/Local is the active workspace runtime/i)).toBeTruthy();
    expect((screen.getByTestId("environment-setup") as HTMLDetailsElement).open).toBe(false);
  });

  it("renders versioned setup scripts and surfaces their steps", async () => {
    render(<EnvironmentsPanel config={baseConfig} workspaceRoot="/Users/me/code" />);
    expect(screen.getByText(/Local bootstrap/)).toBeTruthy();
    expect(screen.getByText(/v1 · local\.bootstrap/)).toBeTruthy();
    expect(screen.getByText(/git status --porcelain/)).toBeTruthy();
  });

  it("disables the select button for runtime-unsupported environments", async () => {
    render(<EnvironmentsPanel config={baseConfig} workspaceRoot="/Users/me/code" />);
    const ssh = screen.getByTestId("environment-ssh");
    const select = ssh.querySelector("button");
    expect(select?.textContent).toBe("Use this environment");
    expect(select?.disabled).toBe(true);
  });
});
