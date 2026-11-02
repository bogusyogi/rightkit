import type SettingsPane from "../../components/settings/SettingsModal.js";
import type { SettingsTab, ToolsetDraft } from "../../components/settings/types.js";
import type { NewSessionControls } from "../../components/Sidebar.js";
import type {
  CoreSandboxSettings,
  DaemonConfig,
  ExecutionBackendRegistry,
  ExecutionWorkspaceRecord,
  SettingsToolsetsResponse,
  ToolGatewaySnapshot,
} from "@rightkit/agent-ui-core/lib/types";
import type { ConductorRuntimeModel } from "@rightkit/agent-ui-core/lib/conductorModels";
import type { PermissionMode } from "@rightkit/agent-ui-core/lib/daemon";
import type { UseOrchestrationSettingsReturn } from "../../hooks/useOrchestrationSettings.js";
import type { UseSkillsRuntimeReturn } from "../../hooks/useSkillsRuntime.js";
import type { UsePluginsRuntimeReturn } from "../../hooks/usePluginsRuntime.js";
import type { UseLspSettingsReturn } from "@rightkit/agent-ui-core/hooks/useLspSettings";
import type { UseLicenseStateReturn } from "@rightkit/agent-ui-core/hooks/useLicenseState";
import type { LocalModelStoreState } from "@rightkit/agent-ui-core/state/localModelStore";
import type { DiagnosticsStoreState } from "@rightkit/agent-ui-core/state/diagnosticsStore";
import type { ExecutionStoreState } from "@rightkit/agent-ui-core/state/executionStore";
import type { TelegramStoreState } from "@rightkit/agent-ui-core/state/telegramStore";
import type { McpStoreState } from "../../state/mcpStore.js";
import type { PermissionsStoreState } from "../../state/permissionsStore.js";

type SettingsPaneProps = Parameters<typeof SettingsPane>[0];

export type BuildSettingsPanePropsParams = {
  config: DaemonConfig | null;
  workspaceRoot: string;
  engineReady: boolean;
  settingsTab: SettingsTab;
  setSettingsTab: (tab: SettingsTab) => void;
  selected: SettingsPaneProps["selectedSession"];
  permissionMode: PermissionMode;
  permissionModeCycle: () => Promise<void>;
  orchestration: Pick<
    UseOrchestrationSettingsReturn,
    | "settings"
    | "settingsDraft"
    | "settingsDirty"
    | "settingsConfigStatus"
    | "settingsConfigPending"
    | "setSettingsDraft"
    | "reloadSettingsConfigNow"
    | "saveSettings"
    | "discardSettings"
  >;
  skills: UseSkillsRuntimeReturn;
  pluginsRuntime: UsePluginsRuntimeReturn;
  mcp: Pick<
    McpStoreState,
    | "servers"
    | "probeResults"
    | "serverModeStatus"
    | "userConfigPath"
    | "workspaceConfigPath"
    | "draft"
    | "actionPending"
    | "error"
    | "setDraft"
    | "submit"
    | "remove"
    | "probe"
    | "load"
  >;
  lsp: UseLspSettingsReturn;
  permissions: PermissionsStoreState;
  localModel: LocalModelStoreState;
  diagnosticsState: DiagnosticsStoreState;
  execution: ExecutionStoreState;
  telegram: TelegramStoreState;
  license: UseLicenseStateReturn;
  settingsToolsets: SettingsToolsetsResponse | null;
  toolsetDraft: ToolsetDraft;
  toolsetActionPending: string | null;
  toolsetError: string | null;
  toolGatewaySnapshot: ToolGatewaySnapshot | null;
  runtimeMiddlewareSnapshot: SettingsPaneProps["middlewareSnapshot"];
  runtimeMiddlewarePending: string | null;
  runtimeMiddlewareError: string | null;
  coreSandboxSettings: CoreSandboxSettings | null;
  setCoreSandboxSettings: (next: CoreSandboxSettings | null) => void;
  loadDiagnostics: () => Promise<void>;
  loadSettingsToolsets: () => Promise<void>;
  submitSettingsToolset: () => Promise<void>;
  removeSettingsToolset: (id: string) => Promise<void>;
  refreshRuntimeMiddleware: () => Promise<void>;
  toggleRuntimeMiddleware: (id: string, enabled: boolean) => Promise<void>;
  pickWorkspace: () => Promise<void>;
  downloadSupportBundle: () => Promise<void>;
  setStatus: (value: string) => void;
  setToolsetDraft: (draft: ToolsetDraft) => void;
  // New Session tab — moved out of the sidebar so the chrome stays at the
  // icon rail on first launch. Settings owns the per-new-session defaults now.
  newSessionControls: NewSessionControls;
  setNewSessionControls: (next: NewSessionControls) => void;
  modelOptions: ConductorRuntimeModel[];
  newSessionBackendRegistry: ExecutionBackendRegistry | null;
  newSessionWorkspaces: ExecutionWorkspaceRecord[];
  newSessionWorkspacePending: boolean;
  createNewSessionWorkspace: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    backendId: string,
    name: string,
    setStatus: (value: string) => void,
    onCreated: (workspaceId: string) => void,
  ) => Promise<void>;
  newSessionWorkspaceRoot: string;
};

export function buildSettingsPaneProps(params: BuildSettingsPanePropsParams): SettingsPaneProps {
  const {
    config,
    workspaceRoot,
    engineReady,
    settingsTab,
    setSettingsTab,
    selected,
    permissionMode,
    permissionModeCycle,
    orchestration,
    skills,
    pluginsRuntime,
    mcp,
    lsp,
    permissions,
    localModel,
    diagnosticsState,
    execution,
    telegram,
    license,
    settingsToolsets,
    toolsetDraft,
    toolsetActionPending,
    toolsetError,
    toolGatewaySnapshot,
    runtimeMiddlewareSnapshot,
    runtimeMiddlewarePending,
    runtimeMiddlewareError,
    coreSandboxSettings,
    setCoreSandboxSettings,
    loadDiagnostics,
    loadSettingsToolsets,
    submitSettingsToolset,
    removeSettingsToolset,
    refreshRuntimeMiddleware,
    toggleRuntimeMiddleware,
    pickWorkspace,
    downloadSupportBundle,
    setStatus,
    setToolsetDraft,
  } = params;

  return {
    settings: orchestration.settings,
    settingsDraft: orchestration.settingsDraft,
    settingsDirty: orchestration.settingsDirty,
    settingsConfigStatus: orchestration.settingsConfigStatus,
    settingsConfigPending: orchestration.settingsConfigPending,
    localModelCatalog: localModel.catalog,
    localModelProbeBaseUrl: localModel.probeBaseUrl,
    localModelProbeResult: localModel.probeResult,
    localModelChatSmokeResult: localModel.chatSmokeResult,
    localProviderSuggestion: localModel.providerSuggestion,
    localModelActionPending: localModel.actionPending,
    localModelError: localModel.error,
    diagnostics: diagnosticsState.diagnostics,
    coreSandboxSettings,
    supportBundle: diagnosticsState.supportBundle,
    supportBundlePending: diagnosticsState.supportBundlePending,
    instructionStatus: diagnosticsState.instructionStatus,
    diagnosticsLoading: diagnosticsState.loading,
    diagnosticsError: diagnosticsState.error,
    executionBackendRegistry: execution.backendRegistry,
    executionDoctorReports: execution.doctorReports,
    executionActionPending: execution.actionPending,
    executionError: execution.error,
    telegramStatus: telegram.status,
    telegramTest: telegram.test,
    telegramUsers: telegram.users,
    remoteChannels: telegram.remoteChannels,
    telegramStatusLoading: telegram.statusLoading,
    telegramUsersLoading: telegram.usersLoading,
    telegramStatusError: telegram.statusError,
    telegramUsersError: telegram.usersError,
    pluginDir: pluginsRuntime.pluginDir,
    plugins: pluginsRuntime.plugins,
    pluginActionPending: pluginsRuntime.pluginActionPending,
    pluginInstallDraft: pluginsRuntime.pluginInstallDraft,
    pluginMarketplacePath: pluginsRuntime.pluginMarketplacePath,
    pluginMarketplaceEntries: pluginsRuntime.pluginMarketplaceEntries,
    pluginMarketplaceQuery: pluginsRuntime.pluginMarketplaceQuery,
    pluginMarketplaceSyncDraft: pluginsRuntime.pluginMarketplaceSyncDraft,
    pluginMarketplaceInstallDraft: pluginsRuntime.pluginMarketplaceInstallDraft,
    pluginMarketplaceError: pluginsRuntime.pluginMarketplaceError,
    mcpServers: mcp.servers,
    mcpProbeResults: mcp.probeResults,
    mcpServerModeStatus: mcp.serverModeStatus,
    mcpUserConfigPath: mcp.userConfigPath,
    mcpWorkspaceConfigPath: mcp.workspaceConfigPath,
    mcpDraft: mcp.draft,
    mcpActionPending: mcp.actionPending,
    mcpError: mcp.error,
    lspStatus: lsp.lspStatus,
    lspDraft: lsp.lspDraft,
    lspActionPending: lsp.lspActionPending,
    lspError: lsp.lspError,
    selectedSession: selected,
    permissionMode,
    permissionRules: permissions.rules,
    permissionRuleDraft: permissions.ruleDraft,
    permissionRuleActionPending: permissions.ruleActionPending,
    persistentPermissionRuleDraft: permissions.persistentDraft,
    persistentPermissionRuleActionPending: permissions.persistentActionPending,
    persistentPermissionRuleStorePath: permissions.persistentStorePath,
    persistentUserPermissionRules: permissions.persistentUserRules,
    persistentProjectPermissionRules: permissions.persistentProjectRules,
    persistentPermissionRulesError: permissions.persistentError,
    permissionRuleJsonDraft: permissions.jsonDraft,
    permissionRuleJsonPending: permissions.jsonPending,
    permissionDenials: permissions.denials,
    permissionDenialStorePath: permissions.denialStorePath,
    permissionDenialsLoading: permissions.denialsLoading,
    permissionDenialsError: permissions.denialsError,
    skillDirs: skills.skillDirs,
    skills: skills.skills,
    selectedSkillSlug: skills.selectedSkillSlug,
    selectedSkillContent: skills.selectedSkillContent,
    skillGovernance: skills.skillGovernance,
    skillOptimizationRuns: skills.skillOptimizationRuns,
    skillDraftProvenanceReview: skills.skillDraftProvenanceReview,
    selectedSkillOptimizationRunId: skills.selectedSkillOptimizationRunId,
    skillTrainingRunDraft: skills.skillTrainingRunDraft,
    skillTrainingEpochDraft: skills.skillTrainingEpochDraft,
    selectedSkillProposalId: skills.selectedSkillProposalId,
    skillDraft: skills.skillDraft,
    skillActionPending: skills.skillActionPending,
    skillError: skills.skillError,
    settingsToolsets,
    gatewaySnapshot: toolGatewaySnapshot,
    toolsetDraft,
    toolsetActionPending,
    toolsetError,
    middlewareSnapshot: runtimeMiddlewareSnapshot,
    runtimeMiddlewarePending,
    runtimeMiddlewareError,
    workspaceRoot,
    engineReady,
    config,
    activeTab: settingsTab,
    onTab: setSettingsTab,
    onOpenWorkspace: () => void pickWorkspace(),
    onSettingsDraft: orchestration.setSettingsDraft,
    onReloadSettingsConfig: () => void orchestration.reloadSettingsConfigNow(),
    onSaveSettings: () => void orchestration.saveSettings(),
    onDiscardSettings: orchestration.discardSettings,
    onRefreshLocalModels: () => void localModel.loadCatalog(config, setStatus),
    onLocalModelProbeBaseUrl: localModel.setProbeBaseUrl,
    onProbeLocalModelEndpoint: () => void localModel.probe(config, setStatus),
    onSmokeLocalModelEndpoint: (model?: string) =>
      void localModel.chatSmoke(config, setStatus, model),
    onSuggestLocalProvider: (baseUrl: string, model: string) =>
      void localModel.suggestRoute(
        config,
        setStatus,
        baseUrl,
        model,
        workspaceRoot,
        () => void orchestration.reloadSettingsConfigNow(),
      ),
    onRefreshDiagnostics: () => void loadDiagnostics(),
    onCreateSupportBundle: () =>
      void diagnosticsState.createBundle(config, selected?.id ?? null, setStatus),
    onDownloadSupportBundle: () => void downloadSupportBundle(),
    onRefreshExecutionBackends: () =>
      void execution.loadBackends(config, workspaceRoot, setCoreSandboxSettings),
    onDoctorExecutionBackend: (backendId: string) =>
      void execution.doctorBackend(config, workspaceRoot, backendId, setStatus),
    onSetDefaultExecutionBackend: (backendId: string) =>
      void execution.setDefaultBackend(config, workspaceRoot, backendId, setStatus),
    onSetExecutionBackendEnabled: (backendId: string, enabled: boolean) =>
      void execution.setBackendEnabled(config, workspaceRoot, backendId, enabled, setStatus),
    onSaveExecutionBackendResourceLimits: (backendId, limits) =>
      void execution.saveResourceLimits(config, workspaceRoot, backendId, limits, setStatus),
    onSaveExecutionBackendNetworkProfile: (backendId: string, networkProfile: string | null) =>
      void execution.saveNetworkProfile(
        config,
        workspaceRoot,
        backendId,
        networkProfile,
        setStatus,
      ),
    onSaveExecutionNetworkProfileDefinition: (profileId, body) =>
      void execution.saveNetworkProfileDefinition(
        config,
        workspaceRoot,
        profileId,
        body,
        setStatus,
      ),
    onDeleteExecutionNetworkProfileDefinition: (profileId: string) =>
      void execution.deleteNetworkProfileDefinition(config, workspaceRoot, profileId, setStatus),
    onSaveCoreSandboxTier: (tier) =>
      void execution.saveCoreSandboxTier(
        config,
        workspaceRoot,
        tier,
        setStatus,
        setCoreSandboxSettings,
        loadDiagnostics,
      ),
    onRefreshTelegramStatus: () => void telegram.loadStatus(config),
    onDiscoverTelegramUsers: () => void telegram.loadUsers(config),
    onRefreshPlugins: () => void pluginsRuntime.refreshPlugins(),
    onTogglePlugin: (plugin) => void pluginsRuntime.togglePlugin(plugin),
    onPluginInstallDraft: pluginsRuntime.setPluginInstallDraft,
    onPickPluginSource: () => void pluginsRuntime.pickPluginSource(),
    onInstallPlugin: () => void pluginsRuntime.submitPluginInstall(),
    onPluginMarketplaceQuery: pluginsRuntime.setPluginMarketplaceQuery,
    onRefreshPluginMarketplace: () => void pluginsRuntime.loadPluginMarketplace(),
    onPluginMarketplaceSyncDraft: pluginsRuntime.setPluginMarketplaceSyncDraft,
    onSyncPluginMarketplace: () => void pluginsRuntime.submitPluginMarketplaceSync(),
    onPluginMarketplaceInstallDraft: pluginsRuntime.setPluginMarketplaceInstallDraft,
    onInstallMarketplacePlugin: (entry) =>
      void pluginsRuntime.submitMarketplacePluginInstall(entry),
    onMcpDraft: mcp.setDraft,
    onSaveMcpServer: () => void mcp.submit(config, workspaceRoot, setStatus),
    onRemoveMcpServer: (server) => void mcp.remove(config, workspaceRoot, server, setStatus),
    onProbeMcpServer: (server) => void mcp.probe(config, workspaceRoot, server, setStatus),
    onRefreshMcp: () => void mcp.load(config, workspaceRoot, setStatus),
    onRefreshLsp: () => void lsp.loadLsp(),
    onLspDraft: lsp.setLspDraft,
    onSaveLsp: () => void lsp.saveLsp(),
    onRefreshSkills: () => void skills.loadSkills(),
    onRefreshSkillGovernance: () => void skills.loadSkillGovernance(),
    onRefreshSkillOptimizationRuns: () => void skills.loadSkillOptimizationRuns(),
    onSelectSkill: (slug: string) => void skills.inspectSkill(slug),
    onSelectSkillOptimizationRun: skills.setSelectedSkillOptimizationRunId,
    onSelectSkillProposal: skills.setSelectedSkillProposalId,
    onSelectedSkillContent: skills.setSelectedSkillContent,
    onSkillDraft: skills.setSkillDraft,
    onSkillTrainingRunDraft: skills.setSkillTrainingRunDraft,
    onSkillTrainingEpochDraft: skills.setSkillTrainingEpochDraft,
    onInstallSkill: () => void skills.submitSkillInstall(),
    onSaveSkill: () => void skills.saveSelectedSkill(),
    onToggleSkill: () => void skills.toggleSelectedSkill(),
    onDeleteSkill: () => void skills.deleteSelectedSkill(),
    onQuarantineSkillProposal: () => void skills.quarantineSelectedSkillProposal(),
    onMarkSkillProposalBenchmark: (passed: boolean) =>
      void skills.markSelectedSkillProposalBenchmark(passed),
    onReviewSkillDraftProvenance: () => void skills.reviewSkillDraftProvenance(),
    onCreateSkillTrainingRun: () => void skills.createSkillTrainingRun(),
    onProposeSkillTrainingCandidate: () =>
      void skills.proposeCandidateForSelectedSkillOptimizationRun(),
    onExecuteSkillTrainingEpoch: () => void skills.executeSelectedSkillOptimizationEpoch(),
    onTransitionSkill: (action, operatorOverride?: boolean) =>
      void skills.transitionSelectedSkill(action, operatorOverride),
    onToolsetDraft: setToolsetDraft,
    onRefreshToolsets: () => void loadSettingsToolsets(),
    onSaveToolset: () => void submitSettingsToolset(),
    onDeleteToolset: (id: string) => void removeSettingsToolset(id),
    onRefreshRuntimeMiddleware: () => void refreshRuntimeMiddleware(),
    onToggleRuntimeMiddleware: (id: string, enabled: boolean) =>
      void toggleRuntimeMiddleware(id, enabled),
    licenseView: license.licenseView,
    licenseActionPending: license.licenseActionPending,
    licenseLatestVersion: license.licenseLatestVersion,
    licenseUpdateState: license.licenseUpdateState,
    licenseError: license.licenseError,
    onCheckForUpdate: () => void license.checkLicenseUpdate(),
    onInstallUpdate: () => void license.installLicenseUpdate(),
    onPermissionRuleDraft: permissions.setRuleDraft,
    onAddPermissionRule: () => void permissions.submitRule(config, selected?.id ?? null, setStatus),
    onRemovePermissionRule: (rule) =>
      void permissions.removeRule(config, selected?.id ?? null, rule, setStatus),
    onPersistentPermissionRuleDraft: permissions.setPersistentDraft,
    onAddPersistentPermissionRule: () =>
      void permissions.submitPersistentRule(
        config,
        selected?.workspaceRoot || workspaceRoot,
        setStatus,
      ),
    onRemovePersistentPermissionRule: (rule) =>
      void permissions.removePersistentRule(
        config,
        selected?.workspaceRoot || workspaceRoot,
        rule,
        setStatus,
      ),
    onPermissionRuleJsonDraft: permissions.setJsonDraft,
    onExportPermissionRules: () => void permissions.exportJson(config, setStatus),
    onImportPermissionRules: () =>
      void permissions.importJson(config, selected?.workspaceRoot || workspaceRoot, setStatus),
    onRefreshPermissions: () =>
      void permissions.loadAll(
        config,
        selected?.id ?? null,
        selected?.workspaceRoot || workspaceRoot,
        setStatus,
      ),
    onCyclePermissionMode: () => void permissionModeCycle(),
    newSessionControls: params.newSessionControls,
    modelOptions: params.modelOptions,
    newSessionWorkspaces: params.newSessionWorkspaces,
    newSessionWorkspacePending: params.newSessionWorkspacePending,
    onNewSessionControls: params.setNewSessionControls,
    onCreateExecutionWorkspace: () => {
      const backendId =
        params.newSessionControls.executionBackend.trim() ||
        params.newSessionBackendRegistry?.defaultBackend ||
        "local";
      void params.createNewSessionWorkspace(
        params.config,
        params.newSessionWorkspaceRoot,
        backendId,
        backendId,
        params.setStatus,
        (workspaceId: string) =>
          params.setNewSessionControls({
            ...params.newSessionControls,
            executionWorkspaceId: workspaceId,
          }),
      );
    },
  };
}
