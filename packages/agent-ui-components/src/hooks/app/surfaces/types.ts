// AppSurfaceInputs — the single argument bag the surface assembly helpers read.
//
// Every field forwards a value that App.tsx already holds in scope. Object-shaped
// hook returns are forwarded whole and typed via ReturnType so the contract can
// never drift from the producing hook; loose useState values are typed against
// the same hook's field (indexed access) or their concrete declaration type.
//
// This is plain data forwarding — no effects, no async, no daemon calls, no
// canonical state ownership lives here (plan §"Lane C — Hard rule").

import type { Dispatch, SetStateAction } from "react";
import type { RuntimeTab } from "@rightkit/agent-ui-workbench/components/RuntimePane";
import type { ExternalDriverKind } from "@rightkit/agent-ui-core/lib/types";
import type { SettingsTab, ToolsetDraft } from "../../../components/settings/types";
import type { useShellUiState } from "../../useShellUiState";
import type { useDaemonConnection } from "@rightkit/agent-ui-hooks/hooks/useDaemonConnection";
import type { useLicenseState } from "@rightkit/agent-ui-core/hooks/useLicenseState";
import type { useSessionList } from "@rightkit/agent-ui-hooks/hooks/useSessionList";
import type { useSessionLifecycle } from "../../useSessionLifecycle";
import type { useOrchestrationSettings } from "../../useOrchestrationSettings";
import type { useSubAgents } from "@rightkit/agent-ui-core/hooks/useSubAgents";
import type { useSessionEvents } from "@rightkit/agent-ui-hooks/hooks/useSessionEvents";
import type { useQueuedMessages } from "@rightkit/agent-ui-hooks/hooks/useQueuedMessages";
import type { useApprovals } from "@rightkit/agent-ui-hooks/hooks/useApprovals";
import type { useGoalManagement } from "@rightkit/agent-ui-core/hooks/useGoalManagement";
import type { useRuntimePanels } from "@rightkit/agent-ui-hooks/hooks/useRuntimePanels";
import type { useTeamsRuntime } from "@rightkit/agent-ui-hooks/hooks/useTeamsRuntime";
import type { useRuntimeAggregates } from "@rightkit/agent-ui-hooks/hooks/useRuntimeAggregates";
import type { useSkillsRuntime } from "../../useSkillsRuntime";
import type { usePluginsRuntime } from "../../usePluginsRuntime";
import type { useLspSettings } from "@rightkit/agent-ui-core/hooks/useLspSettings";
import type { useAppShellActions } from "@rightkit/agent-ui-hooks/hooks/app/useAppShellActions";
import type { useAppInteractionActions } from "@rightkit/agent-ui-hooks/hooks/app/useAppInteractionActions";
import type { useAppAdminActions } from "../useAppAdminActions";
import type { useAppSessionActions } from "@rightkit/agent-ui-hooks/hooks/app/useAppSessionActions";
import type { useDiagnosticsStore } from "@rightkit/agent-ui-core/state/diagnosticsStore";
import type { useTelegramStore } from "@rightkit/agent-ui-core/state/telegramStore";
import type { useLocalModelStore } from "@rightkit/agent-ui-core/state/localModelStore";
import type { useExecutionStore } from "@rightkit/agent-ui-core/state/executionStore";
import type { useMcpStore } from "../../../state/mcpStore";
import type { usePermissionsStore } from "../../../state/permissionsStore";
import type { CommandInfo, PermissionMode } from "@rightkit/agent-ui-core/lib/daemon";
import type {
  AwaySummary,
  CoreSandboxSettings,
  DriverKind,
  ExecutionProcessLogs,
  ExecutionProcessRow,
  ModelCatalogEntry,
  SettingsToolsetsResponse,
  ToolGatewaySnapshot,
} from "@rightkit/agent-ui-core/lib/types";
import type { ConductorRuntimeModel } from "@rightkit/agent-ui-core/lib/conductorModels";
import type { ConductorOption } from "@rightkit/agent-ui-core/lib/conductorOptions";
import type { CatalogGroup, InferenceSelection } from "@rightkit/agent-ui-core/lib/types/inference";

type ShellUi = ReturnType<typeof useShellUiState>;
type Daemon = ReturnType<typeof useDaemonConnection>;
type License = ReturnType<typeof useLicenseState>;
type SessionList = ReturnType<typeof useSessionList>;
type Lifecycle = ReturnType<typeof useSessionLifecycle>;
type Orchestration = ReturnType<typeof useOrchestrationSettings>;
type SubAgentsRuntime = ReturnType<typeof useSubAgents>;
type SessionEvents = ReturnType<typeof useSessionEvents>;
type Queued = ReturnType<typeof useQueuedMessages>;
type Approvals = ReturnType<typeof useApprovals>;
type SessionActions = ReturnType<typeof useAppSessionActions>;
type AdminActions = ReturnType<typeof useAppAdminActions>;
type ShellActions = ReturnType<typeof useAppShellActions>;
type InteractionActions = ReturnType<typeof useAppInteractionActions>;

// Lane C-2: narrow per-slice selectors are forwarded from App.tsx via
// storeSelectors WHERE THEY HELP. Composer is the only store where the
// surface reads a small slice (typing/composer state) — the others
// (diagnostics/localModel/execution/telegram/permissions/mcp) feed
// buildSettingsPaneProps which reads most fields, so we keep the full
// store shape forwarded for them and let the surface helpers stay stable.
import type { useComposerState } from "../../../state/storeSelectors";
type ComposerSel = ReturnType<typeof useComposerState>;

export interface AppSurfaceInputs {
  // Forwarded whole (object-shaped hook returns; fields read inside helpers).
  goalState: ReturnType<typeof useGoalManagement>;
  runtimePanels: ReturnType<typeof useRuntimePanels>;
  teamsRuntime: ReturnType<typeof useTeamsRuntime>;
  runtime: ReturnType<typeof useRuntimeAggregates>;
  skillsRuntime: ReturnType<typeof useSkillsRuntime>;
  pluginsRuntime: ReturnType<typeof usePluginsRuntime>;
  lsp: ReturnType<typeof useLspSettings>;
  composerState: ComposerSel;
  diagnosticsState: ReturnType<typeof useDiagnosticsStore.getState>;
  telegram: ReturnType<typeof useTelegramStore.getState>;
  localModel: ReturnType<typeof useLocalModelStore.getState>;
  execution: ReturnType<typeof useExecutionStore.getState>;
  mcp: ReturnType<typeof useMcpStore.getState>;
  permissions: ReturnType<typeof usePermissionsStore.getState>;

  // Shell / workspace.
  config: Daemon["config"];
  health: Daemon["health"];
  status: Daemon["status"];
  setStatus: Daemon["setStatus"];
  engineReady: boolean;
  workspaceRoot: string;
  setWorkspace: (path: string) => void;
  isWorking: boolean;

  // Shell UI menu + pane state.
  pane: ShellUi["pane"];
  setPane: ShellUi["setPane"];
  openSettings: (tab?: SettingsTab) => void;
  runTarget: ShellUi["runTarget"];
  setRunTarget: ShellUi["setRunTarget"];
  settingsTab: SettingsTab;
  setSettingsTab: ShellUi["setSettingsTab"];
  showWorkspaceMenu: boolean;
  setShowWorkspaceMenu: ShellUi["setShowWorkspaceMenu"];
  showRunMenu: boolean;
  setShowRunMenu: ShellUi["setShowRunMenu"];
  showBranchMenu: boolean;
  setShowBranchMenu: ShellUi["setShowBranchMenu"];

  // Daemon connection drafts (onboarding panel).
  urlDraft: Daemon["urlDraft"];
  setUrlDraft: Daemon["setUrlDraft"];
  tokenDraft: Daemon["tokenDraft"];
  setTokenDraft: Daemon["setTokenDraft"];
  showAdvancedConnect: Daemon["showAdvancedConnect"];
  setShowAdvancedConnect: Daemon["setShowAdvancedConnect"];
  connect: Daemon["connect"];
  launchDaemon: Daemon["launchDaemon"];

  // License.
  licenseView: License["licenseView"];
  licenseActionPending: License["licenseActionPending"];
  licenseLatestVersion: License["licenseLatestVersion"];
  licenseUpdateState: License["licenseUpdateState"];
  licenseError: License["licenseError"];
  checkLicenseUpdate: License["checkLicenseUpdate"];
  downloadLicenseUpdate: License["downloadLicenseUpdate"];
  installLicenseUpdate: License["installLicenseUpdate"];

  // Session list / lifecycle.
  selected: SessionList["selected"];
  setSelectedId: SessionList["setSelectedId"];
  sessionSearch: SessionList["sessionSearch"];
  setSessionSearch: SessionList["setSessionSearch"];
  sessionSearchPending: SessionList["sessionSearchPending"];
  sessionSearchSnippets: SessionList["sessionSearchSnippets"];
  sidebarSessions: SessionList["sidebarSessions"];
  uniqueWorkspaces: SessionList["uniqueWorkspaces"];
  newSessionControls: Lifecycle["newSessionControls"];
  setNewSessionControls: Lifecycle["setNewSessionControls"];
  refresh: Lifecycle["refresh"];
  newSession: Lifecycle["newSession"];
  deleteSessionById: Lifecycle["deleteSessionById"];
  resumeDormant: Lifecycle["resumeDormant"];

  // Orchestration settings.
  settings: Orchestration["settings"];
  settingsDraft: Orchestration["settingsDraft"];
  settingsDirty: Orchestration["settingsDirty"];
  settingsConfigStatus: Orchestration["settingsConfigStatus"];
  settingsConfigPending: Orchestration["settingsConfigPending"];
  setSettingsDraft: Orchestration["setSettingsDraft"];
  reloadSettingsConfigNow: Orchestration["reloadSettingsConfigNow"];
  saveSettings: Orchestration["saveSettings"];
  discardSettings: Orchestration["discardSettings"];
  enabledModelOptions: ModelCatalogEntry[];
  primaryModelLabel: string | null;
  modelUnavailable: boolean;
  conductorRuntimeModels: ConductorRuntimeModel[];
  setConductorRuntimeModel: (modelId: string) => Promise<void>;
  conductorModelPending: boolean;
  conductorOptions: ConductorOption[];
  catalogGroups: CatalogGroup[];
  activeInferenceSelection: InferenceSelection | null;
  setConductorRuntimeSelection: (selection: InferenceSelection) => Promise<void>;
  activeConductor: DriverKind;
  /** Live conductor state for the composer pill; null for a native session. */
  driverModelLabel: string | null;
  onConductorChange: (kind: DriverKind) => void;
  onConductorModelSelect: (kind: ExternalDriverKind, modelId: string) => void;

  // Sub-agents / session events / queue / approvals.
  subAgents: SubAgentsRuntime["subAgents"];
  cancelSubAgent: SubAgentsRuntime["cancelSubAgent"];
  cancelAllSubAgents: SubAgentsRuntime["cancelAllSubAgents"];
  messages: SessionEvents["messages"];
  // streamingText/streamingReasoning/reasoningExpanded live in useStreamingStore
  // and are read directly by WorkbenchContent (Lane C-2 perf isolation).
  setReasoningExpanded: SessionEvents["setReasoningExpanded"];
  dispatchDecision: SessionEvents["dispatchDecision"];
  pendingQuestion: SessionEvents["pendingQuestion"];
  pendingPlan: SessionEvents["pendingPlan"];
  streamingToolName: SessionEvents["streamingToolName"];
  queuedMessages: Queued["queuedMessages"];
  steerQueued: Queued["steerQueued"];
  editQueued: Queued["editQueued"];
  deleteQueued: Queued["deleteQueued"];
  pendingApproval: Approvals["pendingApproval"];
  decideApproval: Approvals["decideApproval"];
  stageApprovalHunk: Approvals["stageApprovalHunk"];

  // Session / admin / shell / interaction actions.
  compact: SessionActions["compact"];
  exportSessionJson: SessionActions["exportSessionJson"];
  exportSessionMarkdown: SessionActions["exportSessionMarkdown"];
  submitQuestionAnswers: SessionActions["submitQuestionAnswers"];
  submitPlanDecision: SessionActions["submitPlanDecision"];
  loadAwaySummary: AdminActions["loadAwaySummary"];
  loadTerminalProcesses: AdminActions["loadTerminalProcesses"];
  loadTerminalLogs: AdminActions["loadTerminalLogs"];
  loadSettingsToolsets: AdminActions["loadSettingsToolsets"];
  submitSettingsToolset: AdminActions["submitSettingsToolset"];
  removeSettingsToolset: AdminActions["removeSettingsToolset"];
  refreshRuntimeMiddleware: AdminActions["refreshRuntimeMiddleware"];
  toggleRuntimeMiddleware: AdminActions["toggleRuntimeMiddleware"];
  loadDiagnostics: AdminActions["loadDiagnostics"];
  switchBranch: ShellActions["switchBranch"];
  pickWorkspace: ShellActions["pickWorkspace"];
  pickRecipeFile: ShellActions["pickRecipeFile"];
  cyclePermissionMode: ShellActions["cyclePermissionMode"];
  selectPermissionMode: ShellActions["selectPermissionMode"];
  pasteComposerImages: ShellActions["pasteComposerImages"];
  submit: InteractionActions["submit"];
  createTerminalSession: InteractionActions["createTerminalSession"];
  sendTerminalInput: InteractionActions["sendTerminalInput"];
  stopTerminalProcess: InteractionActions["stopTerminalProcess"];
  interrupt: InteractionActions["interrupt"];
  editMessage: InteractionActions["editMessage"];

  // Spec binding (App.tsx callbacks).
  bindSelectedSpecToSession: () => Promise<void>;
  clearSelectedSessionSpec: () => Promise<void>;

  // Loose useState values owned by App.tsx.
  permissionMode: PermissionMode;
  commands: CommandInfo[];
  runtimeTab: RuntimeTab;
  setRuntimeTab: Dispatch<SetStateAction<RuntimeTab>>;
  systemUser: string | null;
  gitInfo: { current: string; branches: string[] } | null;
  branchSwitchPending: string | null;
  sessionCostUsd: number;
  sessionExportPending: boolean;
  contextPercent: number;
  usagePromptTokens: number;
  usageCompletionTokens: number;
  usageContextTokens: number;
  cacheReadTokens: number;
  usageCacheCreationTokens: number;
  awaySummary: AwaySummary | null;
  awaySummaryPending: boolean;
  awaySummaryCursor: number;
  coreSandboxSettings: CoreSandboxSettings | null;
  setCoreSandboxSettings: Dispatch<SetStateAction<CoreSandboxSettings | null>>;
  settingsToolsets: SettingsToolsetsResponse | null;
  toolGatewaySnapshot: ToolGatewaySnapshot | null;
  toolsetDraft: ToolsetDraft;
  setToolsetDraft: Dispatch<SetStateAction<ToolsetDraft>>;
  toolsetActionPending: string | null;
  toolsetError: string | null;
  runtimeMiddlewarePending: string | null;
  runtimeMiddlewareError: string | null;
  terminalProcesses: ExecutionProcessRow[];
  selectedTerminalProcessId: string | null;
  setSelectedTerminalProcessId: Dispatch<SetStateAction<string | null>>;
  terminalLogs: ExecutionProcessLogs | null;
  terminalInput: string;
  setTerminalInput: Dispatch<SetStateAction<string>>;
  terminalLoading: boolean;
  terminalError: string | null;
  terminalActionPending: string | null;
}
