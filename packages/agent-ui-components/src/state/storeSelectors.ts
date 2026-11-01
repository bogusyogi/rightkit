// storeSelectors — narrow per-field selectors for the Zustand stores used at
// the App.tsx layer. Lane C-2 render isolation: subscribing to whole stores
// re-renders the entire tree whenever ANY field changes. These selectors
// return a stable object reference (via useShallow) and only notify on the
// fields App.tsx actually threads through useAppSurfaces.

import { useShallow } from "zustand/react/shallow";

import { useComposerStore } from "@rightkit/agent-ui-core/state/composerStore";
import { useDiagnosticsStore } from "@rightkit/agent-ui-core/state/diagnosticsStore";
import { useExecutionStore } from "@rightkit/agent-ui-core/state/executionStore";
import { useLocalModelStore } from "@rightkit/agent-ui-core/state/localModelStore";
import { useMcpStore } from "./mcpStore.js";
import { usePermissionsStore } from "./permissionsStore.js";
import { useTelegramStore } from "@rightkit/agent-ui-core/state/telegramStore";

/** Composer fields that App.tsx forwards. History navigation also needs the
 * `promptHistoryIndex` flag because the composer surfaces it as `historyActive`. */
export function useComposerState() {
  return useComposerStore(
    useShallow((state) => ({
      composer: state.composer,
      attachments: state.attachments,
      promptHistoryIndex: state.promptHistoryIndex,
      setComposer: state.setComposer,
      setAttachments: state.setAttachments,
      addAttachments: state.addAttachments,
      rememberPrompt: state.rememberPrompt,
      clear: state.clear,
      changeComposer: state.changeComposer,
      navigateHistory: state.navigateHistory,
    })),
  );
}

export function useDiagnosticsState() {
  return useDiagnosticsStore(
    useShallow((state) => ({
      diagnostics: state.diagnostics,
      supportBundle: state.supportBundle,
      supportBundlePending: state.supportBundlePending,
      instructionStatus: state.instructionStatus,
      loading: state.loading,
      error: state.error,
      downloadBundle: state.downloadBundle,
      createBundle: state.createBundle,
      load: state.load,
    })),
  );
}

export function useExecutionState() {
  return useExecutionStore(
    useShallow((state) => ({
      backendRegistry: state.backendRegistry,
      workspaces: state.workspaces,
      workspacePending: state.workspacePending,
      doctorReports: state.doctorReports,
      actionPending: state.actionPending,
      error: state.error,
      saveCoreSandboxTier: state.saveCoreSandboxTier,
      saveNetworkProfile: state.saveNetworkProfile,
      saveNetworkProfileDefinition: state.saveNetworkProfileDefinition,
      deleteNetworkProfileDefinition: state.deleteNetworkProfileDefinition,
      saveResourceLimits: state.saveResourceLimits,
      setBackendEnabled: state.setBackendEnabled,
      setDefaultBackend: state.setDefaultBackend,
      doctorBackend: state.doctorBackend,
      createWorkspace: state.createWorkspace,
      loadBackends: state.loadBackends,
      loadWorkspaces: state.loadWorkspaces,
      reset: state.reset,
    })),
  );
}

export function useLocalModelState() {
  return useLocalModelStore(
    useShallow((state) => ({
      catalog: state.catalog,
      probeBaseUrl: state.probeBaseUrl,
      probeResult: state.probeResult,
      chatSmokeResult: state.chatSmokeResult,
      providerSuggestion: state.providerSuggestion,
      actionPending: state.actionPending,
      error: state.error,
      loadCatalog: state.loadCatalog,
      setProbeBaseUrl: state.setProbeBaseUrl,
      probe: state.probe,
      chatSmoke: state.chatSmoke,
      suggestRoute: state.suggestRoute,
    })),
  );
}

export function useMcpState() {
  return useMcpStore(
    useShallow((state) => ({
      servers: state.servers,
      probeResults: state.probeResults,
      serverModeStatus: state.serverModeStatus,
      userConfigPath: state.userConfigPath,
      workspaceConfigPath: state.workspaceConfigPath,
      draft: state.draft,
      actionPending: state.actionPending,
      error: state.error,
      setDraft: state.setDraft,
      submit: state.submit,
      remove: state.remove,
      probe: state.probe,
      load: state.load,
    })),
  );
}

export function usePermissionsState() {
  return usePermissionsStore(
    useShallow((state) => ({
      rules: state.rules,
      ruleDraft: state.ruleDraft,
      ruleActionPending: state.ruleActionPending,
      persistentDraft: state.persistentDraft,
      persistentActionPending: state.persistentActionPending,
      persistentUserRules: state.persistentUserRules,
      persistentProjectRules: state.persistentProjectRules,
      persistentStorePath: state.persistentStorePath,
      persistentError: state.persistentError,
      workspaceTrust: state.workspaceTrust,
      workspaceTrustRoot: state.workspaceTrustRoot,
      workspaceTrustPending: state.workspaceTrustPending,
      denials: state.denials,
      denialsError: state.denialsError,
      denialsLoading: state.denialsLoading,
      denialStorePath: state.denialStorePath,
      jsonDraft: state.jsonDraft,
      jsonPending: state.jsonPending,
      applyWorkspaceTrust: state.applyWorkspaceTrust,
      resetWorkspaceTrust: state.resetWorkspaceTrust,
      loadWorkspaceTrust: state.loadWorkspaceTrust,
      loadAll: state.loadAll,
      loadRules: state.loadRules,
      loadPersistentRules: state.loadPersistentRules,
      loadDenials: state.loadDenials,
      setRuleDraft: state.setRuleDraft,
      setPersistentDraft: state.setPersistentDraft,
      setJsonDraft: state.setJsonDraft,
      submitRule: state.submitRule,
      removeRule: state.removeRule,
      submitPersistentRule: state.submitPersistentRule,
      removePersistentRule: state.removePersistentRule,
      exportJson: state.exportJson,
      importJson: state.importJson,
    })),
  );
}

export function useTelegramState() {
  return useTelegramStore(
    useShallow((state) => ({
      status: state.status,
      test: state.test,
      users: state.users,
      remoteChannels: state.remoteChannels,
      statusLoading: state.statusLoading,
      usersLoading: state.usersLoading,
      statusError: state.statusError,
      usersError: state.usersError,
      loadStatus: state.loadStatus,
      loadUsers: state.loadUsers,
    })),
  );
}
