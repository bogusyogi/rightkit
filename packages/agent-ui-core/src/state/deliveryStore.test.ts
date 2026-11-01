// D4 — Delivery store tests.
//
// The store mutates state through the typed daemon client; tests stub the
// client to exercise action transitions, history accumulation, and
// capability-aware error surfaces. Authored for Agent 4 to run during final
// integration.

import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../lib/daemon/delivery-environments-attention", () => {
  const calls: Array<{ name: string; args: unknown }> = [];
  const enqueue =
    (name: string) =>
    (...args: unknown[]) => {
      calls.push({ name, args });
      return undefined;
    };
  const handler: ProxyHandler<Record<string, unknown>> = {
    get(_target, prop) {
      return enqueue(String(prop));
    },
  };
  const target: Record<string, unknown> = new Proxy({}, handler);
  return {
    getDeliveryStatus: target.getDeliveryStatus,
    listBranches: target.listBranches,
    listWorktrees: target.listWorktrees,
    createWorktree: target.createWorktree,
    removeWorktree: target.removeWorktree,
    stagePaths: target.stagePaths,
    unstagePaths: target.unstagePaths,
    commitChanges: target.commitChanges,
    pushBranch: target.pushBranch,
    pullBranch: target.pullBranch,
    createPullRequest: target.createPullRequest,
    getPullRequest: target.getPullRequest,
    listPullRequestChecks: target.listPullRequestChecks,
    rerunCheck: target.rerunCheck,
    getCheckLogs: target.getCheckLogs,
    listPullRequestThreads: target.listPullRequestThreads,
    runBackgroundCommand: target.runBackgroundCommand,
    cancelBackgroundCommand: target.cancelBackgroundCommand,
    pickRuntimeSupportedEnvironment: () => null,
    groupAttentionByState: () => ({}),
    hasUrgentAttention: () => false,
    deliveryCapabilityKey: (kind: string) => `capability:${kind}`,
    environmentKindLabel: (kind: string) => kind,
    __calls: calls,
  };
});

import { useDeliveryStore } from "./deliveryStore";
import * as client from "../lib/daemon/delivery-environments-attention";

const calls = (client as unknown as { __calls: Array<{ name: string; args: unknown }> }).__calls;

const baseConfig = { url: "http://127.0.0.1:1", token: "x" };
const workspaceRoot = "/Users/me/code";

beforeEach(() => {
  calls.length = 0;
  useDeliveryStore.getState().reset();
});

describe("delivery store", () => {
  it("loads the delivery snapshot and clears pending state", async () => {
    (client as unknown as { getDeliveryStatus: () => Promise<unknown> }).getDeliveryStatus = () =>
      Promise.resolve({
        workspaceRoot,
        hostKind: "local",
        hostAvailable: true,
        capabilities: [
          { kind: "git", available: true, reason: null },
          { kind: "pullRequest", available: false, reason: "gh missing" },
        ],
        authority: {
          trusted: true,
          source: "tauri-webview",
          workspaceRoot,
          trustLabel: "trusted",
        },
        gitBinary: "git",
        ghBinary: null,
        gitVersion: "git version 2.45",
        ghVersion: null,
        checkedAt: "unix:1",
      });

    await useDeliveryStore.getState().loadSnapshot(baseConfig, workspaceRoot, "local");
    const state = useDeliveryStore.getState();
    expect(state.snapshotPending).toBe(false);
    expect(state.snapshot?.gitBinary).toBe("git");
    expect(state.snapshot?.capabilities.find((c) => c.kind === "pullRequest")?.available).toBe(
      false,
    );
  });

  it("records command history when listing branches", async () => {
    (client as unknown as { listBranches: () => Promise<unknown> }).listBranches = () =>
      Promise.resolve({
        commandId: "delivery-cmd-1",
        command: "git",
        args: ["branch", "--all"],
        cwd: workspaceRoot,
        startedAt: "unix:1",
        finishedAt: "unix:2",
        exitCode: 0,
        status: "exited",
        stdoutTail: "* main",
        stderrTail: "",
        hostKind: "local",
      });

    await useDeliveryStore.getState().loadBranches(baseConfig, workspaceRoot);
    const state = useDeliveryStore.getState();
    expect(state.history).toHaveLength(1);
    expect(state.history[0].command).toBe("git");
    expect(state.history[0].actionLabel).toBe("list branches");
    expect(state.error).toBeNull();
  });

  it("surfaces stderr as error on commit failure", async () => {
    (client as unknown as { commitChanges: () => Promise<unknown> }).commitChanges = () =>
      Promise.resolve({
        command: 'git commit -m "x"',
        exitCode: 128,
        stdout: "",
        stderr: "fatal: not a git repository",
        status: "exited",
      });

    const ok = await useDeliveryStore.getState().commitChanges(baseConfig, {
      workspaceRoot,
      message: "x",
    });
    const state = useDeliveryStore.getState();
    expect(ok).toBe(false);
    expect(state.error).toContain("not a git repository");
  });

  it("records unknown errors when the client throws", async () => {
    (client as unknown as { pullBranch: () => Promise<unknown> }).pullBranch = () =>
      Promise.reject(new Error("network down"));

    const ok = await useDeliveryStore.getState().pullBranch(baseConfig, workspaceRoot);
    expect(ok).toBe(false);
    expect(useDeliveryStore.getState().error).toBe("network down");
  });

  it("clears history on demand", async () => {
    (client as unknown as { listBranches: () => Promise<unknown> }).listBranches = () =>
      Promise.resolve({
        commandId: "delivery-cmd-1",
        command: "git",
        args: ["branch"],
        cwd: workspaceRoot,
        startedAt: "unix:1",
        finishedAt: "unix:2",
        exitCode: 0,
        status: "exited",
        stdoutTail: "",
        stderrTail: "",
        hostKind: "local",
      });
    await useDeliveryStore.getState().loadBranches(baseConfig, workspaceRoot);
    expect(useDeliveryStore.getState().history).toHaveLength(1);
    useDeliveryStore.getState().clearHistory();
    expect(useDeliveryStore.getState().history).toHaveLength(0);
  });
});
