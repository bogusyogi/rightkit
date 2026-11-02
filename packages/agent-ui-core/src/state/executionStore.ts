import { create } from "zustand";

import {
  createExecutionWorkspace,
  deleteExecutionNetworkProfile,
  doctorExecutionBackend,
  getCoreSandboxSettings,
  listExecutionBackends,
  listExecutionWorkspaces,
  saveCoreSandboxSettings,
  saveExecutionNetworkProfile,
  setDefaultExecutionBackend,
  updateExecutionBackend,
} from "../lib/daemon.js";
import type { SaveExecutionNetworkProfileRequest } from "../lib/daemon.js";
import type {
  BackendDoctorReport,
  CoreSandboxTier,
  DaemonConfig,
  ExecutionBackendRegistry,
  ExecutionWorkspaceRecord,
} from "../lib/types.js";
import { messageOf, type SetCoreSandboxSettings, type SetStatus } from "../lib/utils.js";

/** Execution-backend settings: backend registry, workspaces, doctor reports.
 * `coreSandboxSettings` (shared with diagnostics) is threaded into actions via
 * `setCoreSandboxSettings`; the new-session binding and diagnostics reload come
 * in as callbacks. */
export type ExecutionStoreState = {
  backendRegistry: ExecutionBackendRegistry | null;
  workspaces: ExecutionWorkspaceRecord[];
  workspacePending: boolean;
  doctorReports: Record<string, BackendDoctorReport>;
  actionPending: string | null;
  error: string | null;
  reset: () => void;
  loadBackends: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    setCoreSandboxSettings: SetCoreSandboxSettings,
  ) => Promise<void>;
  saveCoreSandboxTier: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    tier: CoreSandboxTier,
    setStatus: SetStatus,
    setCoreSandboxSettings: SetCoreSandboxSettings,
    onSaved: () => Promise<void> | void,
  ) => Promise<void>;
  loadWorkspaces: (config: DaemonConfig | null, workspaceRoot: string) => Promise<void>;
  createWorkspace: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    backendId: string,
    name: string,
    setStatus: SetStatus,
    onCreated: (workspaceId: string) => void,
  ) => Promise<void>;
  doctorBackend: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    backendId: string,
    setStatus: SetStatus,
  ) => Promise<void>;
  setDefaultBackend: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    backendId: string,
    setStatus: SetStatus,
  ) => Promise<void>;
  setBackendEnabled: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    backendId: string,
    enabled: boolean,
    setStatus: SetStatus,
  ) => Promise<void>;
  saveResourceLimits: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    backendId: string,
    limits: { cpu?: number | null; memoryMb?: number | null; diskMb?: number | null },
    setStatus: SetStatus,
  ) => Promise<void>;
  saveNetworkProfile: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    backendId: string,
    networkProfile: string | null,
    setStatus: SetStatus,
  ) => Promise<void>;
  saveNetworkProfileDefinition: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    profileId: string,
    body: SaveExecutionNetworkProfileRequest,
    setStatus: SetStatus,
  ) => Promise<void>;
  deleteNetworkProfileDefinition: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    profileId: string,
    setStatus: SetStatus,
  ) => Promise<void>;
};

export const useExecutionStore = create<ExecutionStoreState>((set) => ({
  backendRegistry: null,
  workspaces: [],
  workspacePending: false,
  doctorReports: {},
  actionPending: null,
  error: null,

  reset() {
    set({ backendRegistry: null, workspaces: [] });
  },

  async loadBackends(config, workspaceRoot, setCoreSandboxSettings) {
    if (!config) return;
    set({ actionPending: "list", error: null });
    try {
      const [registry, sandboxSettings] = await Promise.all([
        listExecutionBackends(config, workspaceRoot || undefined),
        getCoreSandboxSettings(config, workspaceRoot || undefined),
      ]);
      set({ backendRegistry: registry });
      setCoreSandboxSettings(sandboxSettings);
    } catch (err) {
      set({ error: messageOf(err, "Execution backends failed") });
    } finally {
      set({ actionPending: null });
    }
  },

  async saveCoreSandboxTier(
    config,
    workspaceRoot,
    tier,
    setStatus,
    setCoreSandboxSettings,
    onSaved,
  ) {
    if (!config) return;
    set({ actionPending: "sandbox-tier", error: null });
    try {
      const settings = await saveCoreSandboxSettings(config, tier, workspaceRoot || undefined);
      setCoreSandboxSettings(settings);
      setStatus(
        settings.tier === tier
          ? `Core sandbox tier set to ${tier}`
          : `Workspace tier saved as ${tier}; operator floor keeps ${settings.tier} effective`,
      );
      await onSaved();
    } catch (err) {
      set({ error: messageOf(err, "Core sandbox settings save failed") });
    } finally {
      set({ actionPending: null });
    }
  },

  async loadWorkspaces(config, workspaceRoot) {
    if (!config || !workspaceRoot.trim()) return;
    try {
      const response = await listExecutionWorkspaces(config, workspaceRoot);
      set({ workspaces: response.workspaces });
    } catch (err) {
      set({ error: messageOf(err, "Execution workspaces failed") });
    }
  },

  async createWorkspace(config, workspaceRoot, backendId, name, setStatus, onCreated) {
    if (!config || !workspaceRoot.trim()) return;
    set({ workspacePending: true, error: null });
    try {
      const response = await createExecutionWorkspace(config, { backendId, name }, workspaceRoot);
      set((state) => ({
        workspaces: [
          response.workspace,
          ...state.workspaces.filter((workspace) => workspace.id !== response.workspace.id),
        ],
      }));
      onCreated(response.workspace.id);
      setStatus(`Execution workspace ${response.workspace.id} created`);
    } catch (err) {
      setStatus(messageOf(err, "Execution workspace create failed"));
    } finally {
      set({ workspacePending: false });
    }
  },

  async doctorBackend(config, workspaceRoot, backendId, setStatus) {
    if (!config) return;
    set({ actionPending: `doctor:${backendId}`, error: null });
    try {
      const report = await doctorExecutionBackend(config, backendId, workspaceRoot || undefined);
      set((state) => ({ doctorReports: { ...state.doctorReports, [backendId]: report } }));
      setStatus(`Execution backend ${backendId} doctor: ${report.verdict}`);
    } catch (err) {
      set({ error: messageOf(err, "Execution backend doctor failed") });
    } finally {
      set({ actionPending: null });
    }
  },

  async setDefaultBackend(config, workspaceRoot, backendId, setStatus) {
    if (!config) return;
    set({ actionPending: `default:${backendId}`, error: null });
    try {
      const registry = await setDefaultExecutionBackend(
        config,
        backendId,
        workspaceRoot || undefined,
      );
      set({ backendRegistry: registry });
      setStatus(`Default execution backend set to ${backendId}`);
    } catch (err) {
      set({ error: messageOf(err, "Execution backend default failed") });
    } finally {
      set({ actionPending: null });
    }
  },

  async setBackendEnabled(config, workspaceRoot, backendId, enabled, setStatus) {
    if (!config) return;
    set({ actionPending: `enabled:${backendId}`, error: null });
    try {
      const registry = await updateExecutionBackend(
        config,
        backendId,
        { enabled },
        workspaceRoot || undefined,
      );
      set({ backendRegistry: registry });
      setStatus(`Execution backend ${backendId} ${enabled ? "enabled" : "disabled"}`);
    } catch (err) {
      set({ error: messageOf(err, "Execution backend update failed") });
    } finally {
      set({ actionPending: null });
    }
  },

  async saveResourceLimits(config, workspaceRoot, backendId, limits, setStatus) {
    if (!config) return;
    set({ actionPending: `limits:${backendId}`, error: null });
    try {
      const registry = await updateExecutionBackend(
        config,
        backendId,
        { resourceLimits: limits },
        workspaceRoot || undefined,
      );
      set({ backendRegistry: registry });
      setStatus(`Execution backend ${backendId} resource caps saved`);
    } catch (err) {
      set({ error: messageOf(err, "Execution backend resource caps failed") });
    } finally {
      set({ actionPending: null });
    }
  },

  async saveNetworkProfile(config, workspaceRoot, backendId, networkProfile, setStatus) {
    if (!config) return;
    set({ actionPending: `network:${backendId}`, error: null });
    try {
      const registry = await updateExecutionBackend(
        config,
        backendId,
        { networkProfile },
        workspaceRoot || undefined,
      );
      set({ backendRegistry: registry });
      setStatus(`Execution backend ${backendId} network profile saved`);
    } catch (err) {
      set({ error: messageOf(err, "Execution backend network profile failed") });
    } finally {
      set({ actionPending: null });
    }
  },

  async saveNetworkProfileDefinition(config, workspaceRoot, profileId, body, setStatus) {
    if (!config) return;
    set({ actionPending: `profile:${profileId}`, error: null });
    try {
      const registry = await saveExecutionNetworkProfile(
        config,
        profileId,
        body,
        workspaceRoot || undefined,
      );
      set({ backendRegistry: registry });
      setStatus(`Execution network profile ${profileId} saved`);
    } catch (err) {
      set({ error: messageOf(err, "Execution network profile save failed") });
    } finally {
      set({ actionPending: null });
    }
  },

  async deleteNetworkProfileDefinition(config, workspaceRoot, profileId, setStatus) {
    if (!config) return;
    set({ actionPending: `profile-delete:${profileId}`, error: null });
    try {
      const registry = await deleteExecutionNetworkProfile(
        config,
        profileId,
        workspaceRoot || undefined,
      );
      set({ backendRegistry: registry });
      setStatus(`Execution network profile ${profileId} deleted`);
    } catch (err) {
      set({ error: messageOf(err, "Execution network profile delete failed") });
    } finally {
      set({ actionPending: null });
    }
  },
}));
