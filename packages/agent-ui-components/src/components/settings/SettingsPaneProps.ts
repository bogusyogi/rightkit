import type RuntimePane from "@rightkit/agent-ui-workbench/components/RuntimePane";
import type TerminalPane from "@rightkit/terminal-ui/components/TerminalPane";
import type TasksPane from "@rightkit/agent-ui-workbench/components/workbench/TasksPane";
import type { PermissionMode } from "@rightkit/agent-ui-core/lib/daemon";
import type { CliAccountProfileView } from "@rightkit/agent-ui-core/lib/daemon/config-host";
import type { LicenseView } from "@rightkit/agent-ui-core/state/license";
import type { UpdateState } from "@rightkit/agent-ui-core/state/updates";
import type { NewSessionControls } from "../Sidebar";
import type { ConductorRuntimeModel } from "@rightkit/agent-ui-core/lib/conductorModels";
import type { CliProviderId } from "@rightkit/agent-ui-core/lib/cliProviders";
import type {
  BackendDoctorReport,
  CoreSandboxSettings,
  CoreSandboxTier,
  DaemonConfig,
  DiagnosticsReport,
  ExecutionBackendRegistry,
  ExecutionNetworkDefault,
  ExecutionWorkspaceRecord,
  InstructionStatus,
  LocalModelCatalog,
  LocalModelChatSmokeResult,
  LocalModelProbeResult,
  LocalProviderRouteSuggestion,
  LspStatusView,
  LspWorkspaceServer,
  McpProbeResult,
  McpServerModeStatus,
  McpServerRow,
  MiddlewareRegistrySnapshot,
  OrchestrationSettings,
  PermissionDenialRow,
  PermissionRuleRow,
  PersistentPermissionRuleRow,
  PluginManifest,
  PluginMarketplaceEntry,
  RemoteChannelsSnapshot,
  SessionRow,
  SettingsConfigStatus,
  SettingsToolsetsResponse,
  SkillGovernanceResponse,
  SkillItem,
  SkillOptimizationRun,
  SkillProvenanceReviewResponse,
  SupportBundle,
  TelegramRemoteStatus,
  TelegramRemoteTest,
  TelegramRemoteUsers,
  ToolGatewaySnapshot,
} from "@rightkit/agent-ui-core/lib/types";
import type {
  McpDraft,
  PermissionRuleDraft,
  PersistentPermissionRuleDraft,
  PluginInstallDraft,
  PluginMarketplaceInstallDraft,
  PluginMarketplaceSyncDraft,
  SettingsDraft,
  SettingsTab,
  SkillInstallDraft,
  SkillTrainingEpochDraft,
  SkillTrainingRunDraft,
  ToolsetDraft,
} from "./types";
import type { McpDiscoverPanelProps } from "@rightkit/agent-ui-settings/components/settings/marketplace/McpDiscoverPanel";
import type { SkillMarketplaceProps } from "@rightkit/agent-ui-settings/components/settings/marketplace/SkillMarketplace";

export interface SettingsPaneProps {
  open?: boolean;
  onClose?: () => void;
  settings: OrchestrationSettings | null;
  settingsDraft: SettingsDraft | null;
  settingsDirty: boolean;
  settingsConfigStatus: SettingsConfigStatus | null;
  settingsConfigPending: boolean;
  localModelCatalog: LocalModelCatalog | null;
  localModelProbeBaseUrl: string;
  localModelProbeResult: LocalModelProbeResult | null;
  localModelChatSmokeResult: LocalModelChatSmokeResult | null;
  localProviderSuggestion: LocalProviderRouteSuggestion | null;
  localModelActionPending: string | null;
  localModelError: string | null;
  diagnostics: DiagnosticsReport | null;
  coreSandboxSettings: CoreSandboxSettings | null;
  supportBundle: SupportBundle | null;
  supportBundlePending: string | null;
  instructionStatus: InstructionStatus | null;
  diagnosticsLoading: boolean;
  diagnosticsError: string | null;
  executionBackendRegistry: ExecutionBackendRegistry | null;
  executionDoctorReports: Record<string, BackendDoctorReport>;
  executionActionPending: string | null;
  executionError: string | null;
  telegramStatus: TelegramRemoteStatus | null;
  telegramTest: TelegramRemoteTest | null;
  telegramUsers: TelegramRemoteUsers | null;
  remoteChannels: RemoteChannelsSnapshot | null;
  telegramStatusLoading: boolean;
  telegramUsersLoading: boolean;
  telegramStatusError: string | null;
  telegramUsersError: string | null;
  pluginDir: string;
  plugins: PluginManifest[];
  pluginActionPending: string | null;
  pluginInstallDraft: PluginInstallDraft;
  pluginMarketplacePath: string;
  pluginMarketplaceEntries: PluginMarketplaceEntry[];
  pluginMarketplaceQuery: string;
  pluginMarketplaceSyncDraft: PluginMarketplaceSyncDraft;
  pluginMarketplaceInstallDraft: PluginMarketplaceInstallDraft;
  pluginMarketplaceError: string | null;
  mcpServers: McpServerRow[];
  mcpProbeResults: Record<string, McpProbeResult>;
  mcpServerModeStatus: McpServerModeStatus | null;
  mcpUserConfigPath: string;
  mcpWorkspaceConfigPath: string | null;
  mcpDraft: McpDraft;
  mcpActionPending: string | null;
  mcpError: string | null;
  lspStatus: LspStatusView | null;
  lspDraft: LspWorkspaceServer[];
  lspActionPending: string | null;
  lspError: string | null;
  selectedSession: SessionRow | null;
  permissionMode: PermissionMode;
  permissionRules: PermissionRuleRow[];
  permissionRuleDraft: PermissionRuleDraft;
  permissionRuleActionPending: string | null;
  persistentPermissionRuleDraft: PersistentPermissionRuleDraft;
  persistentPermissionRuleActionPending: string | null;
  persistentPermissionRuleStorePath: string;
  persistentUserPermissionRules: PersistentPermissionRuleRow[];
  persistentProjectPermissionRules: PersistentPermissionRuleRow[];
  persistentPermissionRulesError: string | null;
  permissionRuleJsonDraft: string;
  permissionRuleJsonPending: string | null;
  permissionDenials: PermissionDenialRow[];
  permissionDenialStorePath: string;
  permissionDenialsLoading: boolean;
  permissionDenialsError: string | null;
  skillDirs: string[];
  skills: SkillItem[];
  selectedSkillSlug: string | null;
  selectedSkillContent: string;
  skillGovernance: SkillGovernanceResponse | null;
  skillOptimizationRuns: SkillOptimizationRun[];
  skillDraftProvenanceReview: SkillProvenanceReviewResponse | null;
  selectedSkillOptimizationRunId: string | null;
  skillTrainingRunDraft: SkillTrainingRunDraft;
  skillTrainingEpochDraft: SkillTrainingEpochDraft;
  selectedSkillProposalId: string | null;
  skillDraft: SkillInstallDraft;
  skillActionPending: string | null;
  skillError: string | null;
  settingsToolsets: SettingsToolsetsResponse | null;
  gatewaySnapshot: ToolGatewaySnapshot | null;
  toolsetDraft: ToolsetDraft;
  toolsetActionPending: string | null;
  toolsetError: string | null;
  middlewareSnapshot: MiddlewareRegistrySnapshot;
  runtimeMiddlewarePending: string | null;
  runtimeMiddlewareError: string | null;
  workspaceRoot: string;
  /** Optional daemon-backed marketplace projections mounted in Settings. */
  mcpMarketplace?: McpDiscoverPanelProps;
  skillMarketplace?: SkillMarketplaceProps;
  engineReady: boolean;
  activeTab: SettingsTab;
  newSessionControls: NewSessionControls;
  modelOptions: ConductorRuntimeModel[];
  newSessionWorkspaces: ExecutionWorkspaceRecord[];
  newSessionWorkspacePending: boolean;
  onNewSessionControls: (controls: NewSessionControls) => void;
  onCreateExecutionWorkspace: () => void;
  runtimePaneProps?: Parameters<typeof RuntimePane>[0];
  terminalPaneProps?: Parameters<typeof TerminalPane>[0];
  tasksPaneProps?: Parameters<typeof TasksPane>[0];
  config: DaemonConfig | null;
  cliAccountProfiles?: CliAccountProfileView[];
  currentCliProviderId?: CliProviderId | null;
  onRepairAction?: (action: string) => void;
  onTab: (tab: SettingsTab) => void;
  onOpenWorkspace: () => void;
  onSettingsDraft: (draft: SettingsDraft) => void;
  onLoginCliProvider?: (provider: string) => void;
  onReloadSettingsConfig: () => void;
  onSaveSettings: () => void;
  onDiscardSettings: () => void;
  onRefreshLocalModels: () => void;
  onLocalModelProbeBaseUrl: (value: string) => void;
  onProbeLocalModelEndpoint: () => void;
  onSmokeLocalModelEndpoint: (model?: string) => void;
  onSuggestLocalProvider: (baseUrl: string, model: string) => void;
  onRefreshDiagnostics: () => void;
  onCreateSupportBundle: () => void;
  onDownloadSupportBundle: () => void;
  onRefreshExecutionBackends: () => void;
  onDoctorExecutionBackend: (backendId: string) => void;
  onSetDefaultExecutionBackend: (backendId: string) => void;
  onSetExecutionBackendEnabled: (backendId: string, enabled: boolean) => void;
  onSaveExecutionBackendResourceLimits: (
    backendId: string,
    limits: { cpu?: number | null; memoryMb?: number | null; diskMb?: number | null },
  ) => void;
  onSaveExecutionBackendNetworkProfile: (backendId: string, networkProfile: string | null) => void;
  onSaveCoreSandboxTier: (tier: CoreSandboxTier) => void;
  onSaveExecutionNetworkProfileDefinition: (
    profileId: string,
    body: {
      name?: string;
      default?: ExecutionNetworkDefault;
      allowHosts?: string[];
      allowPrivateNetworks?: boolean;
      denyPrivateNetworks?: boolean;
    },
  ) => void;
  onDeleteExecutionNetworkProfileDefinition: (profileId: string) => void;
  onRefreshTelegramStatus: () => void;
  onDiscoverTelegramUsers: () => void;
  onRefreshPlugins: () => void;
  onTogglePlugin: (plugin: PluginManifest) => void;
  onPluginInstallDraft: (draft: PluginInstallDraft) => void;
  onPickPluginSource: () => void;
  onInstallPlugin: () => void;
  onPluginMarketplaceQuery: (query: string) => void;
  onRefreshPluginMarketplace: () => void;
  onPluginMarketplaceSyncDraft: (draft: PluginMarketplaceSyncDraft) => void;
  onSyncPluginMarketplace: () => void;
  onPluginMarketplaceInstallDraft: (draft: PluginMarketplaceInstallDraft) => void;
  onInstallMarketplacePlugin: (entry: PluginMarketplaceEntry) => void;
  onMcpDraft: (draft: McpDraft) => void;
  onSaveMcpServer: () => void;
  onRemoveMcpServer: (server: McpServerRow) => void;
  onProbeMcpServer: (server: McpServerRow) => void;
  onRefreshMcp: () => void;
  onRefreshLsp: () => void;
  onLspDraft: (draft: LspWorkspaceServer[]) => void;
  onSaveLsp: () => void;
  onRefreshSkills: () => void;
  onRefreshSkillGovernance: () => void;
  onRefreshSkillOptimizationRuns: () => void;
  onSelectSkill: (slug: string) => void;
  onSelectSkillOptimizationRun: (id: string) => void;
  onSelectSkillProposal: (id: string) => void;
  onSelectedSkillContent: (content: string) => void;
  onSkillDraft: (draft: SkillInstallDraft) => void;
  onSkillTrainingRunDraft: (draft: SkillTrainingRunDraft) => void;
  onSkillTrainingEpochDraft: (draft: SkillTrainingEpochDraft) => void;
  onInstallSkill: () => void;
  onSaveSkill: () => void;
  onToggleSkill: () => void;
  onDeleteSkill: () => void;
  onQuarantineSkillProposal: () => void;
  onMarkSkillProposalBenchmark: (passed: boolean) => void;
  onReviewSkillDraftProvenance: () => void;
  onCreateSkillTrainingRun: () => void;
  onProposeSkillTrainingCandidate: () => void;
  onExecuteSkillTrainingEpoch: () => void;
  onTransitionSkill: (
    action: "quarantine" | "activate" | "retire",
    operatorOverride?: boolean,
  ) => void;
  onToolsetDraft: (draft: ToolsetDraft) => void;
  onRefreshToolsets: () => void;
  onSaveToolset: () => void;
  onDeleteToolset: (id: string) => void;
  onRefreshRuntimeMiddleware: () => void;
  onToggleRuntimeMiddleware: (id: string, enabled: boolean) => void;
  licenseView: LicenseView | null;
  licenseActionPending: "check" | "install" | null;
  licenseLatestVersion: string;
  licenseUpdateState: UpdateState;
  licenseError: string | null;
  onCheckForUpdate: () => void;
  onInstallUpdate: () => void;
  onPermissionRuleDraft: (draft: PermissionRuleDraft) => void;
  onAddPermissionRule: () => void;
  onRemovePermissionRule: (rule: PermissionRuleRow) => void;
  onPersistentPermissionRuleDraft: (draft: PersistentPermissionRuleDraft) => void;
  onAddPersistentPermissionRule: () => void;
  onRemovePersistentPermissionRule: (rule: PersistentPermissionRuleRow) => void;
  onPermissionRuleJsonDraft: (json: string) => void;
  onExportPermissionRules: () => void;
  onImportPermissionRules: () => void;
  onRefreshPermissions: () => void;
  onCyclePermissionMode: () => void;
}
