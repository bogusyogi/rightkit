import type WorkbenchContent from "../../components/WorkbenchContent";
import type { Pane } from "../../components/WorkbenchContent";
import type { RuntimeTab } from "@rightkit/agent-ui-workbench/components/RuntimePane";
import type { SettingsTab } from "../../components/settings/types";
import type { ScheduleDraft } from "@rightkit/agent-ui-core/hooks/runtime/useSchedulesRuntime";
import type { WebhookTriggerDraft } from "@rightkit/agent-ui-core/hooks/runtime/useWebhookTriggersRuntime";
import type {
  BenchmarkRunRequest,
  ConstitutionDecisionVerdict,
  RecipeScheduleDraft,
} from "@rightkit/agent-ui-core/lib/types";
import type {
  DaemonConfig,
  DispatchDecision,
  ModelCatalogEntry,
  SessionRow,
  ScheduleRow,
  WebhookTrigger,
  TranscriptMessage,
  UserQuestion,
  ExecutionProcessLogs,
  ExecutionProcessRow,
} from "@rightkit/agent-ui-core/lib/types";

type WorkbenchProps = Parameters<typeof WorkbenchContent>[0];

interface PendingQuestion {
  questionId: string;
  questions: UserQuestion[];
}
interface PendingPlan {
  planId: string;
  plan: string;
}

export function buildWorkbenchProps(params: {
  pane: Pane;
  selected: SessionRow | null;
  messages: TranscriptMessage[];
  streamingToolName: string | null;
  dispatchDecision: DispatchDecision | null;
  conductorModelOptions: Array<{ id: string; label: string }>;
  conductorModelPending: boolean;
  onConductorModelChange: (modelId: string) => void;
  pendingApproval: WorkbenchProps["pendingApproval"];
  queuedMessages: WorkbenchProps["queuedMessages"];
  activeGoal: WorkbenchProps["activeGoal"];
  tasks: WorkbenchProps["tasks"];
  artifacts: WorkbenchProps["artifacts"];
  learning: WorkbenchProps["learning"];
  benchmarks: WorkbenchProps["benchmarks"];
  taskGraph: WorkbenchProps["taskGraph"];
  teams: WorkbenchProps["teams"];
  teamStorePath: WorkbenchProps["teamStorePath"];
  runtimeMessages: WorkbenchProps["runtimeMessages"];
  messageStorePath: WorkbenchProps["messageStorePath"];
  terminalProcesses: ExecutionProcessRow[];
  selectedTerminalProcessId: string | null;
  terminalLogs: ExecutionProcessLogs | null;
  terminalInput: string;
  terminalLoading: boolean;
  terminalError: string | null;
  memoryEntries: WorkbenchProps["memoryEntries"];
  memoryQuery: WorkbenchProps["memoryQuery"];
  memoryDraft: WorkbenchProps["memoryDraft"];
  memoryKeywordDraft: WorkbenchProps["memoryKeywordDraft"];
  memoryActionPending: WorkbenchProps["memoryActionPending"];
  teamActionPending: WorkbenchProps["teamActionPending"];
  teamRunPendingName: WorkbenchProps["teamRunPendingName"];
  schedules: WorkbenchProps["schedules"];
  scheduleStorePath: WorkbenchProps["scheduleStorePath"];
  scheduleRunStorePath: WorkbenchProps["scheduleRunStorePath"];
  scheduleAdminAuditStorePath: WorkbenchProps["scheduleAdminAuditStorePath"];
  scheduleAdminAudit: WorkbenchProps["scheduleAdminAudit"];
  scheduleActionPending: WorkbenchProps["scheduleActionPending"];
  scheduleDraft: WorkbenchProps["scheduleDraft"];
  webhookTriggers: WorkbenchProps["webhookTriggers"];
  webhookTriggerAudit: WorkbenchProps["webhookTriggerAudit"];
  webhookTriggerPending: WorkbenchProps["webhookTriggerPending"];
  webhookTriggerLoading: WorkbenchProps["webhookTriggerLoading"];
  webhookTriggerError: WorkbenchProps["webhookTriggerError"];
  webhookTriggerDraft: WorkbenchProps["webhookTriggerDraft"];
  taste: WorkbenchProps["taste"];
  roleMetrics: WorkbenchProps["roleMetrics"];
  rollouts: WorkbenchProps["rollouts"];
  finetuning: WorkbenchProps["finetuning"];
  proposals: WorkbenchProps["proposals"];
  audit: WorkbenchProps["audit"];
  runtimeMiddlewareSnapshot: WorkbenchProps["runtimeMiddlewareSnapshot"];
  runtimeSpans: WorkbenchProps["runtimeSpans"];
  runtimeObservations: WorkbenchProps["runtimeObservations"];
  harnessRadar: WorkbenchProps["harnessRadar"];
  harnessRadarSchedulePending: WorkbenchProps["harnessRadarSchedulePending"];
  specs: WorkbenchProps["specs"];
  selectedSpecId: WorkbenchProps["selectedSpecId"];
  sessionSpecCompletion: WorkbenchProps["sessionSpecCompletion"];
  specClarifyResult: WorkbenchProps["specClarifyResult"];
  specPlanResult: WorkbenchProps["specPlanResult"];
  specChecklistResult: WorkbenchProps["specChecklistResult"];
  specImplementResult: WorkbenchProps["specImplementResult"];
  specEnvelope: WorkbenchProps["specEnvelope"];
  specAnalysis: WorkbenchProps["specAnalysis"];
  specTrace: WorkbenchProps["specTrace"];
  specConstitutionDecisions: WorkbenchProps["specConstitutionDecisions"];
  specActionPending: WorkbenchProps["specActionPending"];
  specDraft: WorkbenchProps["specDraft"];
  specTaskDraft: WorkbenchProps["specTaskDraft"];
  specConstitutionDraft: WorkbenchProps["specConstitutionDraft"];
  recipes: WorkbenchProps["recipes"];
  selectedRecipeId: WorkbenchProps["selectedRecipeId"];
  recipeJsonDraft: WorkbenchProps["recipeJsonDraft"];
  recipeUrlDraft: WorkbenchProps["recipeUrlDraft"];
  recipeParamsDraft: WorkbenchProps["recipeParamsDraft"];
  recipeValidation: WorkbenchProps["recipeValidation"];
  renderedRecipe: WorkbenchProps["renderedRecipe"];
  recipeRuns: WorkbenchProps["recipeRuns"];
  recipeActionPending: WorkbenchProps["recipeActionPending"];
  recipeScheduleOpen: WorkbenchProps["recipeScheduleOpen"];
  recipeScheduleDraft: WorkbenchProps["recipeScheduleDraft"];
  runtimeTab: RuntimeTab;
  config: DaemonConfig | null;
  pendingQuestion: PendingQuestion | null;
  pendingPlan: PendingPlan | null;
  sessionExportPending: boolean;
  awaySummary: WorkbenchProps["awaySummary"];
  awaySummaryPending: WorkbenchProps["awaySummaryPending"];
  awaySummaryAvailable: boolean;
  learningActionPending: WorkbenchProps["learningActionPending"];
  harnessRadarPromotionPending: WorkbenchProps["harnessRadarPromotionPending"];
  benchmarkActionPending: WorkbenchProps["benchmarkActionPending"];
  terminalActionPending: string | null;
  terminalBenchTasks: WorkbenchProps["terminalBenchTasks"];
  terminalBenchActionPending: WorkbenchProps["terminalBenchActionPending"];
  latestTerminalBenchResult: WorkbenchProps["latestTerminalBenchResult"];
  isWorking: boolean;
  workspaceRoot: string;
  compareModels: ModelCatalogEntry[];
  onToggleReasoning: () => void;
  onApprove: () => void;
  onDeny: () => void;
  onStageHunk: WorkbenchProps["onStageHunk"];
  submitQuestionAnswers: (answers: Record<string, string | string[]>) => void;
  submitPlanDecision: (approved: boolean) => void;
  exportSessionJson: () => void;
  exportSessionMarkdown: () => void;
  loadAwaySummary: () => void;
  steerQueued: (id: string) => void;
  editQueued: (id: string) => void;
  deleteQueued: (id: string) => void;
  resumeDormant: () => void;
  pauseGoal: () => void;
  resumeGoal: () => void;
  editGoal: (text: string) => void;
  clearActiveGoal: () => void;
  editMessage: (id: string) => void;
  prefillComposer: (text: string) => void;
  pickWorkspace: () => void;
  setSelectedTerminalProcessId: (id: string | null) => void;
  setTerminalInput: (input: string) => void;
  createTerminalSession: () => void;
  loadTerminalProcesses: () => void;
  sendTerminalInput: () => void;
  stopTerminalProcess: (id: string) => void;
  setRuntimeTab: (tab: RuntimeTab) => void;
  submitLearningDecision: (id: string, decision: "approve" | "reject") => void;
  promoteRadarProposal: (id: string) => Promise<void>;
  scheduleRadarRefresh: () => Promise<void>;
  startBenchmarkRun: (body: BenchmarkRunRequest) => void;
  startTerminalBenchRun: (taskId: string, agent: string) => Promise<void>;
  searchMemory: (query: string) => void;
  setMemoryDraft: (value: string) => void;
  setMemoryKeywordDraft: (value: string) => void;
  rememberMemory: () => Promise<void>;
  removeTeam: (name: string) => Promise<void>;
  runTeamGoal: (name: string, goal: string) => Promise<void>;
  removeSchedule: (id: string) => Promise<void>;
  patchScheduleDraft: (patch: Partial<ScheduleDraft>) => void;
  editScheduleDraft: (schedule: ScheduleRow) => void;
  resetScheduleDraft: () => void;
  saveScheduleDraft: () => Promise<void>;
  updateScheduleTimeZone: (id: string, timeZone: string) => Promise<void>;
  runSchedule: (id: string) => Promise<void>;
  setSchedulePaused: (id: string, paused: boolean) => Promise<void>;
  refreshWebhookTriggers: () => Promise<unknown>;
  patchWebhookTriggerDraft: (patch: Partial<WebhookTriggerDraft>) => void;
  editWebhookTriggerDraft: (trigger: WebhookTrigger) => void;
  resetWebhookTriggerDraft: () => void;
  saveWebhookTriggerDraft: () => Promise<void>;
  setWebhookTriggerEnabled: (id: string, enabled: boolean) => Promise<void>;
  removeWebhookTrigger: (id: string) => Promise<void>;
  selectSpec: (id: string) => void;
  patchSpecDraft: (
    patch: Partial<{
      title: string;
      description: string;
    }>,
  ) => void;
  patchSpecTaskDraft: (
    patch: Partial<{
      description: string;
      requirementIds: string;
      successCriterionIds: string;
      evidenceIds: string;
      foundationPhase: string;
    }>,
  ) => void;
  patchSpecConstitutionDraft: (
    patch: Partial<{
      sourcePath: string;
      sourceSha256: string;
      rule: string;
      verdict: ConstitutionDecisionVerdict;
    }>,
  ) => void;
  createRuntimeSpec: () => Promise<void>;
  addTaskToSelectedSpec: () => Promise<void>;
  analyzeSelectedSpec: () => Promise<void>;
  clarifySelectedSpec: () => Promise<void>;
  planSelectedSpec: () => Promise<void>;
  checklistSelectedSpec: () => Promise<void>;
  implementSelectedSpec: () => Promise<void>;
  recordSelectedSpecConstitutionDecision: () => Promise<void>;
  bindSelectedSpecToSession: () => Promise<void>;
  clearSelectedSessionSpec: () => Promise<void>;
  selectRecipe: (id: string) => void;
  setRecipeJsonDraft: (value: string) => void;
  setRecipeUrlDraft: (value: string) => void;
  setRecipeParamsDraft: (value: string) => void;
  validateRecipeDraft: () => Promise<void>;
  installRecipeDraft: () => Promise<void>;
  pickRecipeFile: () => void;
  installRecipeFromUrl: () => Promise<void>;
  setSelectedId: (id: string) => void;
  setPane: (pane: Pane) => void;
  setSettingsTab: (tab: SettingsTab) => void;
  openSettings: (tab?: SettingsTab) => void;
  setStatus: (value: string) => void;
  renderSelectedRecipe: () => Promise<void>;
  runSelectedRecipe: () => Promise<void>;
  openRecipeSchedule: () => void;
  setRecipeScheduleOpen: (open: boolean) => void;
  patchRecipeScheduleDraft: (patch: Partial<RecipeScheduleDraft>) => void;
  scheduleSelectedRecipe: () => Promise<void>;
}): WorkbenchProps {
  const messages = params.messages;
  const selected = params.selected;
  return {
    pane: params.pane,
    selected: params.selected,
    messages: params.messages,
    events: [],
    streamingToolName: params.streamingToolName,
    dispatchDecision: params.dispatchDecision,
    onToggleReasoning: params.onToggleReasoning,
    conductorModelOptions: params.conductorModelOptions,
    conductorModelPending: params.conductorModelPending,
    onConductorModelChange: params.onConductorModelChange,
    pendingApproval: params.pendingApproval,
    queuedMessages: params.queuedMessages,
    activeGoal: params.activeGoal,
    tasks: params.tasks,
    artifacts: params.artifacts,
    learning: params.learning,
    benchmarks: params.benchmarks,
    taskGraph: params.taskGraph,
    teams: params.teams,
    teamStorePath: params.teamStorePath,
    runtimeMessages: params.runtimeMessages,
    messageStorePath: params.messageStorePath,
    terminalProcesses: params.terminalProcesses,
    selectedTerminalProcessId: params.selectedTerminalProcessId,
    terminalLogs: params.terminalLogs,
    terminalInput: params.terminalInput,
    terminalLoading: params.terminalLoading,
    terminalError: params.terminalError,
    memoryEntries: params.memoryEntries,
    memoryQuery: params.memoryQuery,
    memoryDraft: params.memoryDraft,
    memoryKeywordDraft: params.memoryKeywordDraft,
    memoryActionPending: params.memoryActionPending,
    teamActionPending: params.teamActionPending,
    teamRunPendingName: params.teamRunPendingName,
    schedules: params.schedules,
    scheduleStorePath: params.scheduleStorePath,
    scheduleRunStorePath: params.scheduleRunStorePath,
    scheduleAdminAuditStorePath: params.scheduleAdminAuditStorePath,
    scheduleAdminAudit: params.scheduleAdminAudit,
    scheduleActionPending: params.scheduleActionPending,
    scheduleDraft: params.scheduleDraft,
    webhookTriggers: params.webhookTriggers,
    webhookTriggerAudit: params.webhookTriggerAudit,
    webhookTriggerPending: params.webhookTriggerPending,
    webhookTriggerLoading: params.webhookTriggerLoading,
    webhookTriggerError: params.webhookTriggerError,
    webhookTriggerDraft: params.webhookTriggerDraft,
    taste: params.taste,
    roleMetrics: params.roleMetrics,
    rollouts: params.rollouts,
    finetuning: params.finetuning,
    proposals: params.proposals,
    audit: params.audit,
    runtimeMiddlewareSnapshot: params.runtimeMiddlewareSnapshot,
    runtimeSpans: params.runtimeSpans,
    runtimeObservations: params.runtimeObservations,
    harnessRadar: params.harnessRadar,
    harnessRadarSchedulePending: params.harnessRadarSchedulePending,
    specs: params.specs,
    selectedSpecId: params.selectedSpecId,
    activeSessionSpecId: params.selected?.specId ?? null,
    sessionSpecCompletion: params.sessionSpecCompletion,
    specClarifyResult: params.specClarifyResult,
    specPlanResult: params.specPlanResult,
    specChecklistResult: params.specChecklistResult,
    specImplementResult: params.specImplementResult,
    specEnvelope: params.specEnvelope,
    specAnalysis: params.specAnalysis,
    specTrace: params.specTrace,
    specConstitutionDecisions: params.specConstitutionDecisions,
    specActionPending: params.specActionPending,
    specDraft: params.specDraft,
    specTaskDraft: params.specTaskDraft,
    specConstitutionDraft: params.specConstitutionDraft,
    recipes: params.recipes,
    selectedRecipeId: params.selectedRecipeId,
    recipeJsonDraft: params.recipeJsonDraft,
    recipeUrlDraft: params.recipeUrlDraft,
    recipeParamsDraft: params.recipeParamsDraft,
    recipeValidation: params.recipeValidation,
    renderedRecipe: params.renderedRecipe,
    recipeRuns: params.recipeRuns,
    recipeActionPending: params.recipeActionPending,
    recipeScheduleOpen: params.recipeScheduleOpen,
    recipeScheduleDraft: params.recipeScheduleDraft,
    runtimeTab: params.runtimeTab,
    config: params.config,
    compareModels: params.compareModels,
    onApprove: params.onApprove,
    onDeny: params.onDeny,
    onStageHunk: params.onStageHunk,
    pendingQuestion: params.pendingQuestion,
    onAnswerQuestion: params.submitQuestionAnswers,
    pendingPlan: params.pendingPlan,
    onPlanDecision: params.submitPlanDecision,
    sessionExportPending: params.sessionExportPending,
    onExportSessionJson: params.exportSessionJson,
    onExportSessionMarkdown: params.exportSessionMarkdown,
    awaySummary: params.awaySummary,
    awaySummaryPending: params.awaySummaryPending,
    awaySummaryAvailable: params.awaySummaryAvailable,
    onRefreshAwaySummary: params.loadAwaySummary,
    onSteerQueued: params.steerQueued,
    onEditQueued: params.editQueued,
    onDeleteQueued: params.deleteQueued,
    onResume: params.resumeDormant,
    onOpenDriverSettings: () => params.openSettings("conductors"),
    onPauseGoal: params.pauseGoal,
    onResumeGoal: params.resumeGoal,
    onEditGoal: params.editGoal,
    onClearGoal: params.clearActiveGoal,
    onEditMessage: params.editMessage,
    canRollbackMessages: !params.isWorking && messages.some((m) => m.$kind === "user"),
    onPrefillComposer: params.prefillComposer,
    workspaceRoot: selected?.workspaceRoot || params.workspaceRoot,
    onOpenWorkspace: params.pickWorkspace,
    onSelectTerminalProcess: params.setSelectedTerminalProcessId,
    onTerminalInput: params.setTerminalInput,
    onCreateTerminalProcess: params.createTerminalSession,
    onRefreshTerminalProcesses: params.loadTerminalProcesses,
    onSendTerminalInput: params.sendTerminalInput,
    onKillTerminalProcess: params.stopTerminalProcess,
    onRuntimeTab: params.setRuntimeTab,
    learningActionPending: params.learningActionPending,
    onLearningDecision: params.submitLearningDecision,
    harnessRadarPromotionPending: params.harnessRadarPromotionPending,
    onPromoteHarnessRadarProposal: params.promoteRadarProposal,
    onScheduleHarnessRadarRefresh: params.scheduleRadarRefresh,
    benchmarkActionPending: params.benchmarkActionPending,
    terminalActionPending: params.terminalActionPending,
    onRunBenchmark: params.startBenchmarkRun,
    terminalBenchTasks: params.terminalBenchTasks,
    terminalBenchActionPending: params.terminalBenchActionPending,
    latestTerminalBenchResult: params.latestTerminalBenchResult,
    onRunTerminalBench: params.startTerminalBenchRun,
    onMemoryQuery: params.searchMemory,
    onMemoryDraft: params.setMemoryDraft,
    onMemoryKeywordDraft: params.setMemoryKeywordDraft,
    onAddMemory: params.rememberMemory,
    onDeleteTeam: params.removeTeam,
    onRunTeam: params.runTeamGoal,
    onDeleteSchedule: params.removeSchedule,
    onScheduleDraft: params.patchScheduleDraft,
    onEditSchedule: params.editScheduleDraft,
    onResetScheduleDraft: params.resetScheduleDraft,
    onSaveSchedule: params.saveScheduleDraft,
    onScheduleTimeZone: params.updateScheduleTimeZone,
    onRunSchedule: params.runSchedule,
    onSchedulePaused: params.setSchedulePaused,
    onRefreshWebhookTriggers: params.refreshWebhookTriggers,
    onWebhookTriggerDraft: params.patchWebhookTriggerDraft,
    onEditWebhookTrigger: params.editWebhookTriggerDraft,
    onResetWebhookTriggerDraft: params.resetWebhookTriggerDraft,
    onSaveWebhookTrigger: params.saveWebhookTriggerDraft,
    onWebhookTriggerEnabled: params.setWebhookTriggerEnabled,
    onDeleteWebhookTrigger: params.removeWebhookTrigger,
    onSelectSpec: params.selectSpec,
    onSpecDraft: params.patchSpecDraft,
    onSpecTaskDraft: params.patchSpecTaskDraft,
    onSpecConstitutionDraft: params.patchSpecConstitutionDraft,
    onCreateSpec: params.createRuntimeSpec,
    onAddSpecTask: params.addTaskToSelectedSpec,
    onAnalyzeSpec: params.analyzeSelectedSpec,
    onClarifySpec: params.clarifySelectedSpec,
    onPlanSpec: params.planSelectedSpec,
    onChecklistSpec: params.checklistSelectedSpec,
    onImplementSpec: params.implementSelectedSpec,
    onRecordSpecConstitutionDecision: params.recordSelectedSpecConstitutionDecision,
    onBindSessionSpec: params.bindSelectedSpecToSession,
    onClearSessionSpec: params.clearSelectedSessionSpec,
    onSelectRecipe: params.selectRecipe,
    onRecipeJsonDraft: params.setRecipeJsonDraft,
    onRecipeUrlDraft: params.setRecipeUrlDraft,
    onRecipeParamsDraft: params.setRecipeParamsDraft,
    onValidateRecipeDraft: params.validateRecipeDraft,
    onInstallRecipeDraft: params.installRecipeDraft,
    onInstallRecipeFile: () => params.pickRecipeFile(),
    onInstallRecipeUrl: params.installRecipeFromUrl,
    onOpenRecipeRunTarget: (run, target) => {
      params.setSelectedId(run.sessionId);
      if (target === "session") {
        params.setPane("session");
        params.setStatus(`Opened recipe run session: ${run.sessionId}`);
        return;
      }
      params.openSettings("runtime");
      params.setRuntimeTab(target === "evidence" ? "observability" : "artifacts");
      params.setStatus(
        target === "evidence"
          ? `Opened recipe run evidence: ${run.renderedPromptHash.slice(0, 18)}`
          : `Opened recipe run artifact: ${run.resultArtifactId || "none"}`,
      );
    },
    onRenderSelectedRecipe: params.renderSelectedRecipe,
    onRunSelectedRecipe: params.runSelectedRecipe,
    onOpenRecipeSchedule: params.openRecipeSchedule,
    onRecipeScheduleOpen: params.setRecipeScheduleOpen,
    onRecipeScheduleDraft: params.patchRecipeScheduleDraft,
    onScheduleSelectedRecipe: params.scheduleSelectedRecipe,
  };
}
