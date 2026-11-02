import type ComposerIsland from "../../components/app/ComposerIsland.js";
import type { CommandInfo, PermissionMode } from "@rightkit/agent-ui-core/lib/daemon";
import type { SkillItem } from "@rightkit/agent-ui-core/lib/types";
import type { ConductorRuntimeModel } from "@rightkit/agent-ui-core/lib/conductorModels";
import type { ConductorOption } from "@rightkit/agent-ui-core/lib/conductorOptions";
import type { DriverKind, ExternalDriverKind } from "@rightkit/agent-ui-core/lib/types";
import type { RunTarget } from "../../hooks/useShellUiState.js";
import type { UseGoalManagementReturn } from "@rightkit/agent-ui-core/hooks/useGoalManagement";
import type { CatalogGroup, InferenceSelection } from "@rightkit/agent-ui-core/lib/types/inference";

type ComposerIslandProps = Parameters<typeof ComposerIsland>[0];

export type BuildComposerPropsParams = {
  commands: CommandInfo[];
  skills: SkillItem[];
  disabled: boolean;
  workspaceRoot: string;
  workspaces: string[];
  primaryModelLabel: string | null;
  modelUnavailable: boolean;
  modelOptions: ConductorRuntimeModel[];
  conductorOptions: ConductorOption[];
  catalogGroups: CatalogGroup[];
  activeSelection: InferenceSelection | null;
  onSelectionChange: (selection: InferenceSelection) => void;
  selectionSwitchDisabled: boolean;
  selectionSwitchUnavailableReason: string | null;
  activeConductor: DriverKind;
  driverModelLabel: string | null;
  onConductorChange: (kind: DriverKind) => void;
  onConductorModelSelect: (kind: ExternalDriverKind, modelId: string) => void;
  supportsThinkingLevels: boolean;
  gitBranch: string | null;
  gitBranches: string[];
  branchSwitchPending: string | null;
  costUsd: number;
  contextPercent: number;
  usagePromptTokens: number;
  usageCompletionTokens: number;
  usageContextTokens: number;
  cacheReadTokens: number;
  usageCacheCreationTokens: number;
  permissionMode: PermissionMode;
  runTarget: RunTarget;
  isWorking: boolean;
  showWorkspaceMenu: boolean;
  showRunMenu: boolean;
  showBranchMenu: boolean;
  historyActive: boolean;
  queueLength: number;
  hasGoal: boolean;
  goal: UseGoalManagementReturn;
  onSubmit: () => void;
  onInterrupt: () => void;
  onCyclePermissionMode: () => void;
  permissionModes: PermissionMode[];
  onSelectPermissionMode: (mode: PermissionMode) => void;
  onLoadSkills: () => void;
  onPasteImages: (event: React.ClipboardEvent<HTMLTextAreaElement>) => void;
  onSetWorkspace: (path: string) => void;
  onRunTarget: (target: RunTarget) => void;
  onToggleWorkspaceMenu: () => void;
  onToggleRunMenu: () => void;
  onToggleBranchMenu: () => void;
  onDismissMenus: () => void;
  onSwitchBranch: (branch: string) => void;
  onConductorModelChange: (modelId: string) => void;
  onOpenSkillsSettings: () => void;
  onOpenModelSettings: () => void;
  onOpenProviderSettings: () => void;
  onOpenWorkspace: () => void;
};

export function buildComposerProps(params: BuildComposerPropsParams): ComposerIslandProps {
  return {
    commands: params.commands,
    skills: params.skills,
    disabled: params.disabled,
    isWorking: params.isWorking,
    queueLength: params.queueLength,
    activeGoal: params.goal.activeGoal,
    showGoalComposer: params.goal.showGoalComposer,
    workspaceRoot: params.workspaceRoot,
    workspaces: params.workspaces,
    primaryModelLabel: params.primaryModelLabel,
    modelUnavailable: params.modelUnavailable,
    modelOptions: params.modelOptions,
    conductorOptions: params.conductorOptions,
    catalogGroups: params.catalogGroups,
    activeSelection: params.activeSelection,
    onSelectionChange: params.onSelectionChange,
    selectionSwitchDisabled: params.selectionSwitchDisabled,
    selectionSwitchUnavailableReason: params.selectionSwitchUnavailableReason,
    activeConductor: params.activeConductor,
    driverModelLabel: params.driverModelLabel,
    onConductorChange: params.onConductorChange,
    onConductorModelSelect: params.onConductorModelSelect,
    supportsThinkingLevels: params.supportsThinkingLevels,
    gitBranch: params.gitBranch,
    gitBranches: params.gitBranches,
    branchSwitchPending: params.branchSwitchPending,
    costUsd: params.costUsd,
    contextPercent: params.contextPercent,
    usagePromptTokens: params.usagePromptTokens,
    usageCompletionTokens: params.usageCompletionTokens,
    usageContextTokens: params.usageContextTokens,
    cacheReadTokens: params.cacheReadTokens,
    usageCacheCreationTokens: params.usageCacheCreationTokens,
    permissionMode: params.permissionMode,
    showWorkspaceMenu: params.showWorkspaceMenu,
    showBranchMenu: params.showBranchMenu,
    historyActive: params.historyActive,
    onPasteImages: params.onPasteImages,
    onSubmit: () => params.onSubmit(),
    onInterrupt: () => params.onInterrupt(),
    permissionModes: params.permissionModes,
    onSelectPermissionMode: params.onSelectPermissionMode,
    onOpenWorkspace: params.onOpenWorkspace,
    onSetWorkspace: params.onSetWorkspace,
    onToggleWorkspaceMenu: params.onToggleWorkspaceMenu,
    onToggleBranchMenu: params.onToggleBranchMenu,
    onDismissMenus: params.onDismissMenus,
    onSwitchBranch: params.onSwitchBranch,
    onConductorModelChange: params.onConductorModelChange,
    onToggleGoalComposer: params.goal.toggleGoalComposer,
    onCreateGoal: (text: string) => params.goal.createGoal(text),
    onPauseGoal: () => params.goal.pauseGoal(),
    onResumeGoal: () => params.goal.resumeGoal(),
    onEditGoal: (text: string) => params.goal.editGoal(text),
    onClearGoal: () => params.goal.clearActiveGoal(),
    onOpenModelSettings: params.onOpenModelSettings,
    onOpenProviderSettings: params.onOpenProviderSettings,
  };
}
