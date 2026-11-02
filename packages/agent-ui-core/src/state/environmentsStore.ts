// D4 — Environments store: typed Local / Worktree / SSH / WSL / configured
// remote selection with setup state, latency, providers, and rerunnable
// versioned setup scripts.
//
// Reuses existing worktree / notification / remote seams; does not touch
// the generated client, or `AppBody`.

import { create } from "zustand";

import {
  cancelSetupScriptRun,
  getSetupScriptLogs,
  listEnvironments,
  listSetupScripts,
  pickRuntimeSupportedEnvironment,
  rerunSetupScript,
  runSetupScript,
  selectEnvironment,
  testEnvironment,
} from "../lib/daemon/delivery-environments-attention.js";
import type {
  EnvironmentListResponse,
  EnvironmentRecord,
  EnvironmentTestResult,
  SetupScript,
  SetupScriptRun,
} from "../lib/types.js";
import { messageOf } from "../lib/utils.js";

export type EnvironmentsStoreState = {
  list: EnvironmentListResponse | null;
  listPending: boolean;
  selectedId: string | null;
  /** The supported route offered when the saved choice is unavailable. */
  fallbackId: string | null;
  testResults: Record<string, EnvironmentTestResult>;
  scripts: SetupScript[];
  scriptsPending: boolean;
  scriptRuns: Record<string, SetupScriptRun>;
  actionPending: string | null;
  error: string | null;
  reset: () => void;
  loadList: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
  ) => Promise<void>;
  select: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
    environmentId: string,
  ) => Promise<boolean>;
  test: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
    environmentId: string,
  ) => Promise<EnvironmentTestResult | null>;
  loadScripts: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
  ) => Promise<void>;
  runScript: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
    scriptId: string,
  ) => Promise<SetupScriptRun | null>;
  cancelRun: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
    runId: string,
    scriptId: string,
  ) => Promise<boolean>;
  rerunRun: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
    runId: string,
    scriptId: string,
  ) => Promise<SetupScriptRun | null>;
  loadRunLogs: (
    config: import("../lib/types.js").DaemonConfig | null,
    workspaceRoot: string,
    runId: string,
  ) => Promise<SetupScriptRun | null>;
  getRecord: (environmentId: string) => EnvironmentRecord | null;
};

function firstRuntimeSupportedEnvironment(
  environments: EnvironmentRecord[],
): EnvironmentRecord | null {
  // Keep the store's user-facing fallback resilient if the daemon helper is
  // unavailable during startup or a mocked/client boundary returns null.
  return (
    pickRuntimeSupportedEnvironment(environments) ??
    environments.find((environment) => environment.runtimeSupported) ??
    null
  );
}

export const useEnvironmentsStore = create<EnvironmentsStoreState>((set, get) => {
  let listRequest = 0;
  let scriptsRequest = 0;

  return {
    list: null,
    listPending: false,
    selectedId: null,
    fallbackId: null,
    testResults: {},
    scripts: [],
    scriptsPending: false,
    scriptRuns: {},
    actionPending: null,
    error: null,
  reset: () => {
    listRequest += 1;
    scriptsRequest += 1;
    set({
      list: null,
      listPending: false,
      selectedId: null,
      fallbackId: null,
      testResults: {},
      scripts: [],
      scriptsPending: false,
      scriptRuns: {},
      actionPending: null,
      error: null,
    });
  },
  async loadList(config, workspaceRoot) {
    if (!config) return;
    const requestId = ++listRequest;
    set({ listPending: true, error: null });
    try {
      const list = await listEnvironments(config, workspaceRoot);
      if (requestId !== listRequest) return;
      const selectedId = list.selectedId ?? null;
      const selected = list.environments.find((environment) => environment.id === selectedId);
      const fallback =
        !selected || !selected.runtimeSupported
          ? firstRuntimeSupportedEnvironment(list.environments)
          : null;
      set({
        list,
        selectedId,
        fallbackId: fallback?.id ?? null,
        listPending: false,
      });
    } catch (error) {
      if (requestId !== listRequest) return;
      set({ listPending: false, error: messageOf(error) });
    }
  },
  async select(config, workspaceRoot, environmentId) {
    if (!config) return false;
    set({ actionPending: `select:${environmentId}`, error: null });
    try {
      const result = await selectEnvironment(config, workspaceRoot, environmentId);
      const list = get().list;
      const selected = list?.environments.find((environment) => environment.id === result.environmentId);
      const fallback =
        !selected || !selected.runtimeSupported
          ? firstRuntimeSupportedEnvironment(list?.environments ?? [])
          : null;
      set({
        actionPending: null,
        selectedId: result.environmentId,
        fallbackId: fallback?.id ?? null,
        list: list ? { ...list, selectedId: result.environmentId } : list,
      });
      return true;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return false;
    }
  },
  async test(config, workspaceRoot, environmentId) {
    if (!config) return null;
    set({ actionPending: `test:${environmentId}`, error: null });
    try {
      const result = await testEnvironment(config, workspaceRoot, environmentId);
      const list = get().list;
      set({
        actionPending: null,
        testResults: {
          ...get().testResults,
          [environmentId]: result,
        },
        list: list
          ? {
              ...list,
              environments: list.environments.map((environment) =>
                environment.id === environmentId
                  ? {
                      ...environment,
                      lastLatencyMs: result.latencyMs ?? null,
                      lastTestedAt: result.testedAt,
                    }
                  : environment,
              ),
            }
          : list,
      });
      return result;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return null;
    }
  },
  async loadScripts(config, workspaceRoot) {
    if (!config) return;
    const requestId = ++scriptsRequest;
    set({ scriptsPending: true, error: null });
    try {
      const list = await listSetupScripts(config, workspaceRoot);
      if (requestId !== scriptsRequest) return;
      set({ scripts: list.scripts, scriptsPending: false });
    } catch (error) {
      if (requestId !== scriptsRequest) return;
      set({ scriptsPending: false, error: messageOf(error) });
    }
  },
  async runScript(config, workspaceRoot, scriptId) {
    if (!config) return null;
    set({ actionPending: `run:${scriptId}`, error: null });
    try {
      const run = await runSetupScript(config, {
        workspaceRoot,
        scriptId,
      });
      set({
        actionPending: null,
        scriptRuns: { ...get().scriptRuns, [run.runId]: run },
      });
      return run;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return null;
    }
  },
  async cancelRun(config, workspaceRoot, runId, scriptId) {
    if (!config) return false;
    set({ actionPending: `cancel:${runId}`, error: null });
    try {
      const result = await cancelSetupScriptRun(config, workspaceRoot, runId, scriptId);
      const run = get().scriptRuns[runId];
      set({
        actionPending: null,
        scriptRuns:
          run && result.cancelled
            ? { ...get().scriptRuns, [runId]: { ...run, state: "cancelled", cancellable: false } }
            : get().scriptRuns,
      });
      return result.cancelled;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return false;
    }
  },
  async rerunRun(config, workspaceRoot, runId, scriptId) {
    if (!config) return null;
    set({ actionPending: `rerun:${runId}`, error: null });
    try {
      const run = await rerunSetupScript(config, workspaceRoot, runId, {
        workspaceRoot,
        scriptId,
      });
      set({
        actionPending: null,
        scriptRuns: { ...get().scriptRuns, [run.runId]: run },
      });
      return run;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return null;
    }
  },
  async loadRunLogs(config, workspaceRoot, runId) {
    if (!config) return null;
    set({ actionPending: `logs:${runId}`, error: null });
    try {
      const run = await getSetupScriptLogs(config, workspaceRoot, runId);
      set({
        actionPending: null,
        scriptRuns: { ...get().scriptRuns, [run.runId]: run },
      });
      return run;
    } catch (error) {
      set({ actionPending: null, error: messageOf(error) });
      return null;
    }
  },
  getRecord(environmentId) {
    const list = get().list;
    if (!list) return null;
    return list.environments.find((e) => e.id === environmentId) ?? null;
    },
  };
});
