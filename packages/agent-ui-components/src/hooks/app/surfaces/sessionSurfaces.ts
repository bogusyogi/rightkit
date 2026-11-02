// Session surfaces — workbench, onboarding, agent-progress, and composer assembly.
// Pure forwarding into the lib/appSurfaceProps builders; no effects.

import { QA_BUILD } from "@rightkit/agent-ui-core/qaBuild";
import { useStreamingStore } from "@rightkit/agent-ui-core/state/streamingStore";

import {
  buildComposerProps,
  buildOnboardingPanelProps,
  buildWorkbenchProps,
} from "../../../lib/appSurfaceProps/index.js";
import type { BackgroundTasksSurfaceProps as AgentRailProps } from "@rightkit/agent-ui-core/lib/appSurfaceProps/backgroundTasksTypes";
import { conductorSupportsThinkingLevels } from "@rightkit/agent-ui-core/lib/conductorModels";
import type { PermissionMode } from "@rightkit/agent-ui-core/lib/daemon";
import type { AppSurfaceInputs } from "./types.js";

// A normal engine boot with a known workspace is not an onboarding moment:
// flashing the start-point chooser (with a Retry button) during the first
// seconds of every launch reads as a failure and swaps layouts once the
// engine arrives. The chooser is only for "no workspace yet" or "engine failed".
//
// It must NOT key off a "starting/connecting" allowlist: the real boot status
// is often a session-level string like "Ready in <workspace>" that matches no
// such word, so an allowlist wrongly treats a healthy boot as a failure and
// flashes the chooser. Instead, with a known workspace, stay quiet through the
// entire boot and surface the chooser only on an explicit engine-failure status.
const ENGINE_FAILURE =
  /unavailable|unreachable|failed|refused|\berror\b|stopped|disconnect|offline/i;

export function shouldShowOnboardingPanel(args: {
  engineReady: boolean;
  status: string;
  workspaceRoot: string;
}): boolean {
  if (args.engineReady) return false;
  if (args.workspaceRoot.trim() === "") return true;
  return ENGINE_FAILURE.test(args.status);
}

export interface SessionSurfaces {
  workbenchProps: ReturnType<typeof buildWorkbenchProps>;
  showOnboardingPanel: boolean;
  onboardingPanelProps: ReturnType<typeof buildOnboardingPanelProps>;
  railProps: AgentRailProps | null;
  composerProps: ReturnType<typeof buildComposerProps>;
}

export function buildSessionSurfaces(a: AppSurfaceInputs): SessionSurfaces {
  const {
    pane,
    openSettings,
    selected,
    messages,
    streamingToolName,
    dispatchDecision,
    pendingApproval,
    queuedMessages,
    goalState,
    runtimePanels,
    runtime,
    teamsRuntime,
    terminalProcesses,
    selectedTerminalProcessId,
    terminalLogs,
    terminalInput,
    terminalLoading,
    terminalError,
    terminalActionPending,
    runtimeTab,
    config,
    pendingQuestion,
    pendingPlan,
    sessionExportPending,
    awaySummary,
    awaySummaryPending,
    awaySummaryCursor,
    isWorking,
    workspaceRoot,
    engineReady,
    settings,
    skillsRuntime,
    conductorRuntimeModels,
    conductorOptions,
    catalogGroups,
    activeInferenceSelection,
    setConductorRuntimeSelection,
    activeConductor,
    driverModelLabel,
    onConductorChange,
    onConductorModelSelect,
    setConductorRuntimeModel,
    conductorModelPending,
    health,
    setReasoningExpanded,
    decideApproval,
    stageApprovalHunk,
    submitQuestionAnswers,
    submitPlanDecision,
    exportSessionJson,
    exportSessionMarkdown,
    loadAwaySummary,
    steerQueued,
    editQueued,
    deleteQueued,
    newSession,
    resumeDormant,
    editMessage,
    composerState,
    pickWorkspace,
    setSelectedTerminalProcessId,
    setTerminalInput,
    createTerminalSession,
    loadTerminalProcesses,
    sendTerminalInput,
    stopTerminalProcess,
    setRuntimeTab,
    bindSelectedSpecToSession,
    clearSelectedSessionSpec,
    pickRecipeFile,
    setSelectedId,
    setPane,
    setStatus,
    // onboarding
    status,
    runTarget,
    showAdvancedConnect,
    urlDraft,
    tokenDraft,
    setRunTarget,
    launchDaemon,
    setShowAdvancedConnect,
    setUrlDraft,
    setTokenDraft,
    connect,
    // progress
    subAgents,
    cancelSubAgent,
    cancelAllSubAgents,
    // composer
    commands,
    uniqueWorkspaces,
    primaryModelLabel,
    modelUnavailable,
    gitInfo,
    branchSwitchPending,
    sessionCostUsd,
    contextPercent,
    usagePromptTokens,
    usageCompletionTokens,
    usageContextTokens,
    cacheReadTokens,
    usageCacheCreationTokens,
    permissionMode,
    showWorkspaceMenu,
    showRunMenu,
    showBranchMenu,
    submit,
    interrupt,
    pasteComposerImages,
    cyclePermissionMode,
    selectPermissionMode,
    setShowWorkspaceMenu,
    setShowRunMenu,
    setShowBranchMenu,
    switchBranch,
    setSettingsTab,
  } = a;

  const awaySummaryAvailable = Boolean(selected && messages.length > awaySummaryCursor);
  const permissionCycle: PermissionMode[] = ["ask", "accept-edits", "auto-safe", "plan", "bypass"];

  const workbenchProps = buildWorkbenchProps({
    pane,
    selected,
    messages,
    streamingToolName,
    dispatchDecision,
    conductorModelOptions: conductorRuntimeModels,
    conductorModelPending,
    onConductorModelChange: setConductorRuntimeModel,
    pendingApproval,
    queuedMessages,
    activeGoal: goalState.activeGoal,
    tasks: runtimePanels.tasks,
    artifacts: runtimePanels.artifacts,
    learning: runtime.learning,
    benchmarks: runtime.benchmarks,
    taskGraph: runtimePanels.taskGraph,
    teams: teamsRuntime.teams,
    teamStorePath: teamsRuntime.teamStorePath,
    runtimeMessages: runtimePanels.runtimeMessages,
    messageStorePath: runtimePanels.messageStorePath,
    terminalProcesses,
    selectedTerminalProcessId,
    terminalLogs,
    terminalInput,
    terminalLoading,
    terminalError,
    memoryEntries: runtimePanels.memoryEntries,
    memoryQuery: runtimePanels.memoryQuery,
    memoryDraft: runtimePanels.memoryDraft,
    memoryKeywordDraft: runtimePanels.memoryKeywordDraft,
    memoryActionPending: runtimePanels.memoryActionPending,
    teamActionPending: teamsRuntime.teamActionPending,
    teamRunPendingName: teamsRuntime.teamRunPendingName,
    schedules: runtime.schedules,
    scheduleStorePath: runtime.scheduleStorePath,
    scheduleRunStorePath: runtime.scheduleRunStorePath,
    scheduleAdminAuditStorePath: runtime.scheduleAdminAuditStorePath,
    scheduleAdminAudit: runtime.scheduleAdminAudit,
    scheduleActionPending: runtime.scheduleActionPending,
    scheduleDraft: runtime.scheduleDraft,
    webhookTriggers: runtime.webhookTriggers,
    webhookTriggerAudit: runtime.webhookTriggerAudit,
    webhookTriggerPending: runtime.webhookTriggerPending,
    webhookTriggerLoading: runtime.webhookTriggerLoading,
    webhookTriggerError: runtime.webhookTriggerError,
    webhookTriggerDraft: runtime.webhookTriggerDraft,
    taste: runtime.taste,
    roleMetrics: runtime.roleMetrics,
    rollouts: runtime.rollouts,
    finetuning: runtime.finetuning,
    proposals: runtime.proposals,
    audit: runtime.audit,
    runtimeMiddlewareSnapshot: runtime.runtimeMiddlewareSnapshot,
    runtimeSpans: runtime.runtimeSpans,
    runtimeObservations: runtime.runtimeObservations,
    harnessRadar: runtime.harnessRadar,
    harnessRadarSchedulePending: runtime.harnessRadarSchedulePending,
    specs: runtime.specs,
    selectedSpecId: runtime.selectedSpecId,
    sessionSpecCompletion: runtime.sessionSpecCompletion,
    specClarifyResult: runtime.specClarifyResult,
    specPlanResult: runtime.specPlanResult,
    specChecklistResult: runtime.specChecklistResult,
    specImplementResult: runtime.specImplementResult,
    specEnvelope: runtime.specEnvelope,
    specAnalysis: runtime.specAnalysis,
    specTrace: runtime.specTrace,
    specConstitutionDecisions: runtime.specConstitutionDecisions,
    specActionPending: runtime.specActionPending,
    specDraft: runtime.specDraft,
    specTaskDraft: runtime.specTaskDraft,
    specConstitutionDraft: runtime.specConstitutionDraft,
    recipes: runtime.recipes,
    selectedRecipeId: runtime.selectedRecipeId,
    recipeJsonDraft: runtime.recipeJsonDraft,
    recipeUrlDraft: runtime.recipeUrlDraft,
    recipeParamsDraft: runtime.recipeParamsDraft,
    recipeValidation: runtime.recipeValidation,
    renderedRecipe: runtime.renderedRecipe,
    recipeRuns: runtime.recipeRuns,
    recipeActionPending: runtime.recipeActionPending,
    recipeScheduleOpen: runtime.recipeScheduleOpen,
    recipeScheduleDraft: runtime.recipeScheduleDraft,
    runtimeTab,
    config,
    pendingQuestion,
    pendingPlan,
    sessionExportPending,
    awaySummary,
    awaySummaryPending,
    awaySummaryAvailable,
    learningActionPending: runtime.learningActionPending,
    harnessRadarPromotionPending: runtime.harnessRadarPromotionPending,
    benchmarkActionPending: runtime.benchmarkActionPending,
    terminalActionPending,
    terminalBenchTasks: runtime.terminalBenchTasks,
    terminalBenchActionPending: runtime.terminalBenchActionPending,
    latestTerminalBenchResult: runtime.latestTerminalBenchResult,
    isWorking,
    workspaceRoot,
    compareModels: settings?.models ?? [],
    onToggleReasoning: () => {
      setReasoningExpanded((v) => !v);
      useStreamingStore.getState().setReasoningExpanded((v) => !v);
    },
    onApprove: () => void decideApproval(true),
    onDeny: () => void decideApproval(false),
    onStageHunk: (hunk, decision) => stageApprovalHunk(hunk, decision),
    submitQuestionAnswers: (answers: Record<string, string | string[]>) =>
      void submitQuestionAnswers(answers),
    submitPlanDecision: (approved: boolean) => void submitPlanDecision(approved),
    exportSessionJson: () => void exportSessionJson(),
    exportSessionMarkdown: () => void exportSessionMarkdown(),
    loadAwaySummary: () => void loadAwaySummary(),
    steerQueued: (id: string) => void steerQueued(id),
    editQueued,
    deleteQueued,
    resumeDormant: () => void resumeDormant(selected?.id ?? null),
    pauseGoal: goalState.pauseGoal,
    resumeGoal: goalState.resumeGoal,
    editGoal: (text: string) => void goalState.editGoal(text),
    clearActiveGoal: () => void goalState.clearActiveGoal(),
    editMessage: (id: string) => void editMessage(id),
    prefillComposer: (text: string) => {
      composerState.setComposer(text);
    },
    pickWorkspace: () => void pickWorkspace(),
    setSelectedTerminalProcessId,
    setTerminalInput,
    createTerminalSession: () => void createTerminalSession(),
    loadTerminalProcesses: () => void loadTerminalProcesses(),
    sendTerminalInput: () => void sendTerminalInput(),
    stopTerminalProcess: (id: string) => void stopTerminalProcess(id),
    setRuntimeTab,
    submitLearningDecision: (id, decision) => runtime.submitLearningDecision(id, decision),
    promoteRadarProposal: (id: string) => runtime.promoteRadarProposal(id),
    scheduleRadarRefresh: () => runtime.scheduleRadarRefresh(),
    startBenchmarkRun: (body) => runtime.startBenchmarkRun(body),
    startTerminalBenchRun: (taskId: string, agent: string) =>
      runtime.startTerminalBenchRun(taskId, agent),
    searchMemory: (query: string) => void runtimePanels.searchMemory(query),
    setMemoryDraft: runtimePanels.setMemoryDraft,
    setMemoryKeywordDraft: runtimePanels.setMemoryKeywordDraft,
    rememberMemory: () => runtimePanels.rememberMemory(),
    removeTeam: (name: string) => teamsRuntime.removeTeam(name),
    runTeamGoal: (name: string, goal: string) => teamsRuntime.runTeamGoal(name, goal),
    removeSchedule: (id: string) => runtime.removeSchedule(id),
    patchScheduleDraft: runtime.patchScheduleDraft,
    editScheduleDraft: runtime.editScheduleDraft,
    resetScheduleDraft: runtime.resetScheduleDraft,
    saveScheduleDraft: () => runtime.saveScheduleDraft(),
    updateScheduleTimeZone: (id: string, timeZone: string) =>
      runtime.updateScheduleTimeZone(id, timeZone),
    runSchedule: (id: string) => runtime.runSchedule(id),
    setSchedulePaused: (id: string, paused: boolean) => runtime.setSchedulePaused(id, paused),
    refreshWebhookTriggers: () => runtime.refreshWebhookTriggers(),
    patchWebhookTriggerDraft: runtime.patchWebhookTriggerDraft,
    editWebhookTriggerDraft: runtime.editWebhookTriggerDraft,
    resetWebhookTriggerDraft: runtime.resetWebhookTriggerDraft,
    saveWebhookTriggerDraft: () => runtime.saveWebhookTriggerDraft(),
    setWebhookTriggerEnabled: (id: string, enabled: boolean) =>
      runtime.setWebhookTriggerEnabled(id, enabled),
    removeWebhookTrigger: (id: string) => runtime.removeWebhookTrigger(id),
    selectSpec: (id: string) => runtime.selectSpec(id),
    patchSpecDraft: runtime.patchSpecDraft,
    patchSpecTaskDraft: runtime.patchSpecTaskDraft,
    patchSpecConstitutionDraft: runtime.patchSpecConstitutionDraft,
    createRuntimeSpec: () => runtime.createRuntimeSpec(),
    addTaskToSelectedSpec: () => runtime.addTaskToSelectedSpec(),
    analyzeSelectedSpec: () => runtime.analyzeSelectedSpec(),
    clarifySelectedSpec: () => runtime.clarifySelectedSpec(),
    planSelectedSpec: () => runtime.planSelectedSpec(),
    checklistSelectedSpec: () => runtime.checklistSelectedSpec(),
    implementSelectedSpec: () => runtime.implementSelectedSpec(),
    recordSelectedSpecConstitutionDecision: () => runtime.recordSelectedSpecConstitutionDecision(),
    bindSelectedSpecToSession: () => bindSelectedSpecToSession(),
    clearSelectedSessionSpec: () => clearSelectedSessionSpec(),
    selectRecipe: (id: string) => runtime.selectRecipe(id),
    setRecipeJsonDraft: runtime.setRecipeJsonDraft,
    setRecipeUrlDraft: runtime.setRecipeUrlDraft,
    setRecipeParamsDraft: runtime.setRecipeParamsDraft,
    validateRecipeDraft: () => runtime.validateRecipeDraft(),
    installRecipeDraft: () => runtime.installRecipeDraft(),
    pickRecipeFile: () => pickRecipeFile(runtime.installRecipeFromFile),
    installRecipeFromUrl: () => runtime.installRecipeFromUrl(),
    setSelectedId,
    setPane,
    setSettingsTab,
    openSettings,
    setStatus,
    renderSelectedRecipe: () => runtime.renderSelectedRecipe(),
    runSelectedRecipe: () => runtime.runSelectedRecipe(),
    openRecipeSchedule: () => runtime.openRecipeSchedule(),
    setRecipeScheduleOpen: runtime.setRecipeScheduleOpen,
    patchRecipeScheduleDraft: runtime.patchRecipeScheduleDraft,
    scheduleSelectedRecipe: () => runtime.scheduleSelectedRecipe(),
  });

  const qaAgentScenario =
    QA_BUILD &&
    typeof window !== "undefined" &&
    new URLSearchParams(window.location.search).get("qa") === "1" &&
    new URLSearchParams(window.location.search).get("scenario") === "agents";
  const showOnboardingPanel =
    shouldShowOnboardingPanel({ engineReady, status, workspaceRoot }) &&
    pane === "session" &&
    !qaAgentScenario;
  const onboardingPanelProps = buildOnboardingPanelProps({
    status,
    workspaceRoot,
    runTarget,
    showAdvancedConnect,
    urlDraft,
    tokenDraft,
    onOpenWorkspace: () => void pickWorkspace(),
    onRunTarget: setRunTarget,
    onRetryEngine: () => void launchDaemon(),
    onToggleAdvanced: () => setShowAdvancedConnect((value) => !value),
    onUrlDraft: setUrlDraft,
    onTokenDraft: setTokenDraft,
    onConnect: connect,
  });

  const railProps =
    pane === "session" && subAgents.length > 0
      ? {
          agents: subAgents,
          onCancel: cancelSubAgent,
          onCancelAll: cancelAllSubAgents,
          onOpenAgentSession: setSelectedId,
          sessionTitle: selected?.title || undefined,
          goalStatus: goalState.activeGoal?.status,
          costUsd: sessionCostUsd,
        }
      : null;

  const composerProps = buildComposerProps({
    commands,
    skills: skillsRuntime.skills,
    disabled: !config || !health || !selected,
    workspaceRoot,
    workspaces: uniqueWorkspaces,
    primaryModelLabel,
    modelUnavailable,
    modelOptions: conductorRuntimeModels,
    conductorOptions,
    catalogGroups,
    activeSelection: activeInferenceSelection,
    onSelectionChange: (selection) => void setConductorRuntimeSelection(selection),
    selectionSwitchDisabled:
      conductorModelPending || isWorking || subAgents.some((agent) => agent.status === "running"),
    selectionSwitchUnavailableReason: conductorModelPending
      ? "Inference selection is updating"
      : isWorking || subAgents.some((agent) => agent.status === "running")
        ? "Stop active inference and child agents before changing model"
        : null,
    activeConductor,
    driverModelLabel,
    onConductorChange,
    onConductorModelSelect,
    supportsThinkingLevels: conductorSupportsThinkingLevels(selected?.model),
    gitBranch: gitInfo?.current ?? null,
    gitBranches: gitInfo?.branches ?? [],
    branchSwitchPending,
    costUsd: sessionCostUsd,
    contextPercent,
    usagePromptTokens,
    usageCompletionTokens,
    usageContextTokens,
    cacheReadTokens,
    usageCacheCreationTokens,
    permissionMode,
    runTarget,
    isWorking,
    showWorkspaceMenu,
    showRunMenu,
    showBranchMenu,
    historyActive: composerState.promptHistoryIndex !== null,
    queueLength: queuedMessages.length,
    hasGoal: Boolean(goalState.activeGoal),
    goal: goalState,
    onSubmit: () => void submit(),
    onInterrupt: () => void interrupt(),
    onCyclePermissionMode: () => void cyclePermissionMode(permissionCycle),
    permissionModes: permissionCycle,
    onSelectPermissionMode: (mode: PermissionMode) => void selectPermissionMode(mode),
    onLoadSkills: () => void skillsRuntime.loadSkills(),
    onPasteImages: (event: React.ClipboardEvent<HTMLTextAreaElement>) =>
      void pasteComposerImages(event),
    onSetWorkspace: (path: string) => {
      setShowWorkspaceMenu(false);
      void newSession(path);
    },
    onRunTarget: (target) => {
      setRunTarget(target);
      setShowRunMenu(false);
    },
    onToggleWorkspaceMenu: () => setShowWorkspaceMenu((value) => !value),
    onToggleRunMenu: () => setShowRunMenu((value) => !value),
    onToggleBranchMenu: () => setShowBranchMenu((value) => !value),
    onDismissMenus: () => {
      setShowWorkspaceMenu(false);
      setShowRunMenu(false);
      setShowBranchMenu(false);
    },
    onSwitchBranch: (branch: string) => switchBranch(branch),
    onConductorModelChange: (modelId: string) => {
      void setConductorRuntimeModel(modelId);
    },
    onOpenSkillsSettings: () => {
      openSettings("skills");
      void skillsRuntime.loadSkills();
    },
    onOpenModelSettings: () => {
      openSettings("models");
    },
    onOpenProviderSettings: () => {
      openSettings("providers");
    },
    onOpenWorkspace: () => {
      setShowWorkspaceMenu(false);
      void pickWorkspace().then((path) => {
        if (path) void newSession(path);
      });
    },
  });

  return {
    workbenchProps,
    showOnboardingPanel,
    onboardingPanelProps,
    railProps,
    composerProps,
  };
}
