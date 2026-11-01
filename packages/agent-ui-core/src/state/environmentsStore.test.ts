// D4 — Environments store tests.

import { beforeEach, describe, expect, it, vi } from "vitest";

const { calls, enqueue } = vi.hoisted(() => {
  const hoistedCalls: Array<{ name: string; args: unknown }> = [];
  return {
    calls: hoistedCalls,
    enqueue:
      (name: string) =>
      (...args: unknown[]) => {
        hoistedCalls.push({ name, args });
        return undefined;
      },
  };
});

vi.mock("../lib/daemon/delivery-environments-attention", () => {
  const handler: ProxyHandler<Record<string, unknown>> = {
    get(_target, prop) {
      return enqueue(String(prop));
    },
  };
  const target: Record<string, unknown> = new Proxy({}, handler);
  return {
    listEnvironments: target.listEnvironments,
    selectEnvironment: target.selectEnvironment,
    testEnvironment: target.testEnvironment,
    listSetupScripts: target.listSetupScripts,
    runSetupScript: target.runSetupScript,
    cancelSetupScriptRun: target.cancelSetupScriptRun,
    rerunSetupScript: target.rerunSetupScript,
    getSetupScriptLogs: target.getSetupScriptLogs,
    pickRuntimeSupportedEnvironment: () => null,
    groupAttentionByState: () => ({}),
    hasUrgentAttention: () => false,
    deliveryCapabilityKey: (kind: string) => `capability:${kind}`,
    environmentKindLabel: (kind: string) => kind,
  };
});

import { useEnvironmentsStore } from "./environmentsStore";
import * as client from "../lib/daemon/delivery-environments-attention";

const baseConfig = { url: "http://127.0.0.1:1", token: "x" };
const workspaceRoot = "/Users/me/code";

beforeEach(() => {
  calls.length = 0;
  useEnvironmentsStore.getState().reset();
});

describe("environments store", () => {
  it("records the selected id from the list response", async () => {
    (client as unknown as { listEnvironments: () => Promise<unknown> }).listEnvironments = () =>
      Promise.resolve({
        selectedId: "local",
        environments: [
          {
            id: "local",
            label: "Local",
            kind: "local",
            workspaceRoot,
            trust: "trusted",
            setupState: "ready",
            runtimeSupported: true,
            providers: [],
          },
          {
            id: "ssh",
            label: "SSH",
            kind: "ssh",
            workspaceRoot,
            trust: "trusted",
            setupState: "unsupported",
            runtimeSupported: false,
            providers: [],
          },
        ],
        generatedAt: "unix:1",
      });

    await useEnvironmentsStore.getState().loadList(baseConfig, workspaceRoot);
    const state = useEnvironmentsStore.getState();
    expect(state.listPending).toBe(false);
    expect(state.selectedId).toBe("local");
    expect(state.list?.environments.find((e) => e.id === "ssh")?.runtimeSupported).toBe(false);
  });

  it("offers the first supported environment when the saved route is unavailable", async () => {
    (client as unknown as { listEnvironments: () => Promise<unknown> }).listEnvironments = () =>
      Promise.resolve({
        selectedId: "ssh",
        environments: [
          {
            id: "ssh",
            label: "SSH",
            kind: "ssh",
            workspaceRoot,
            trust: "trusted",
            setupState: "unsupported",
            runtimeSupported: false,
            runtimeUnavailableReason: "SSH executor not wired",
            providers: [],
          },
          {
            id: "local",
            label: "Local",
            kind: "local",
            workspaceRoot,
            trust: "trusted",
            setupState: "ready",
            runtimeSupported: true,
            providers: [],
          },
        ],
        generatedAt: "unix:1",
      });

    await useEnvironmentsStore.getState().loadList(baseConfig, workspaceRoot);
    expect(useEnvironmentsStore.getState().selectedId).toBe("ssh");
    expect(useEnvironmentsStore.getState().fallbackId).toBe("local");
  });

  it("persists selection on success", async () => {
    (client as unknown as { selectEnvironment: () => Promise<unknown> }).selectEnvironment = () =>
      Promise.resolve({
        environmentId: "worktree",
        saved: true,
        savedAt: "unix:1",
      });

    const ok = await useEnvironmentsStore.getState().select(baseConfig, workspaceRoot, "worktree");
    expect(ok).toBe(true);
    expect(useEnvironmentsStore.getState().selectedId).toBe("worktree");
  });

  it("captures test results keyed by environment id", async () => {
    (client as unknown as { testEnvironment: () => Promise<unknown> }).testEnvironment = () =>
      Promise.resolve({
        environmentId: "local",
        reachable: true,
        latencyMs: 42,
        testedAt: "unix:2",
        command: "git status --porcelain",
        exitCode: 0,
        stdoutTail: "",
        stderrTail: "",
      });

    await useEnvironmentsStore.getState().test(baseConfig, workspaceRoot, "local");
    const state = useEnvironmentsStore.getState();
    expect(state.testResults.local?.reachable).toBe(true);
    expect(state.testResults.local?.latencyMs).toBe(42);
  });

  it("stores setup script runs keyed by run id", async () => {
    (client as unknown as { runSetupScript: () => Promise<unknown> }).runSetupScript = () =>
      Promise.resolve({
        runId: "setup-run-1",
        scriptId: "local.bootstrap",
        scriptVersion: 1,
        workspaceRoot,
        environmentKind: "local",
        state: "completed",
        startedAt: "unix:1",
        finishedAt: "unix:2",
        exitCode: 0,
        logPath: "/Users/me/code/.coderight/environments/runs/setup-run-1.log",
        logTail: [
          { ts: "unix:1", stream: "system", line: "step git-status → git status --porcelain" },
        ],
        cancellable: false,
      });

    const run = await useEnvironmentsStore
      .getState()
      .runScript(baseConfig, workspaceRoot, "local.bootstrap");
    expect(run?.runId).toBe("setup-run-1");
    expect(useEnvironmentsStore.getState().scriptRuns["setup-run-1"]?.state).toBe("completed");
  });

  it("surfaces errors when the client rejects", async () => {
    (client as unknown as { listEnvironments: () => Promise<unknown> }).listEnvironments = () =>
      Promise.reject(new Error("store offline"));
    await useEnvironmentsStore.getState().loadList(baseConfig, workspaceRoot);
    expect(useEnvironmentsStore.getState().error).toBe("store offline");
  });

  it("getRecord returns the matching environment", async () => {
    (client as unknown as { listEnvironments: () => Promise<unknown> }).listEnvironments = () =>
      Promise.resolve({
        selectedId: null,
        environments: [
          {
            id: "local",
            label: "Local",
            kind: "local",
            workspaceRoot,
            trust: "trusted",
            setupState: "ready",
            runtimeSupported: true,
            providers: [],
          },
        ],
        generatedAt: "unix:1",
      });
    await useEnvironmentsStore.getState().loadList(baseConfig, workspaceRoot);
    expect(useEnvironmentsStore.getState().getRecord("local")?.label).toBe("Local");
    expect(useEnvironmentsStore.getState().getRecord("missing")).toBeNull();
  });
});
