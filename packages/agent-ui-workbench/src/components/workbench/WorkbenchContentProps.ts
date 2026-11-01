import type { RuntimeTab } from "../RuntimePane.js";
import type { PendingApproval } from "@rightkit/agent-ui-hooks/hooks/useApprovals";
import type { ScheduleDraft, WebhookTriggerDraft } from "@rightkit/agent-ui-hooks/hooks/useRuntimeAggregates";
import type { QueuedMessage } from "@rightkit/agent-ui-hooks/hooks/useQueuedMessages";
import type { DiffHunk, DiffHunkDecision } from "@rightkit/agent-ui-hooks/components/workbench/DiffView";
import type {
  ArtifactRow,
  AuditLogView,
  AwaySummary,
  BenchmarkRunRequest,
  BenchmarkReport,
  ConstitutionDecision,
  ConstitutionDecisionVerdict,
  DaemonConfig,
  GoalSnapshotV1,
  DispatchDecision,
  FeatureSpec,
  FinetuningRow,
  HarnessRadarSnapshot,
  LearningProposal,
  ExecutionProcessLogs,
  ExecutionProcessRow,
  MemoryEntryRow,
  MessageHistoryRow,
  ModelCatalogEntry,
  MiddlewareRegistrySnapshot,
  RoleMetric,
  RolloutView,
  RuntimeObservation,
  RuntimeSpan,
  InstalledRecipe,
  ProtocolEvent,
  RecipeRun,
  RecipeScheduleDraft,
  RecipeValidationRun,
  RenderedRecipe,
  ScheduleAdminAuditEvent,
  ScheduleRow,
  SessionRow,
  SessionSpecCompletion,
  SpecChecklistResponse,
  SpecClarifyResponse,
  SpecImplementResponse,
  SpecAnalysisRun,
  SpecEnvelope,
  SpecPlanResponse,
  SpecTrace,
  TaskNode,
  TaskRow,
  TasteProposal,
  TasteScore,
  TerminalBenchRunResult,
  TerminalBenchTask,
  TeamDefinition,
  TranscriptMessage,
  UserQuestion,
  WebhookTrigger,
  WebhookTriggerDeliveryAuditRow,
} from "@rightkit/agent-ui-core/lib/types";

export type Pane = "session" | "tasks" | "runtime" | "terminal" | "memory";

export interface WorkbenchContentProps {
  pane: Pane;
  selected: SessionRow | null;
  messages: TranscriptMessage[];
  events: ProtocolEvent[];
  // streamingText/streamingReasoning/reasoningExpanded are read directly from
  // useStreamingStore inside WorkbenchContent — see Lane C-2 perf isolation.
  // streamingToolName stays on props because it's only used by the action bar
  // (not the per-token stream).
  streamingToolName: string | null;
  dispatchDecision: DispatchDecision | null;
  onToggleReasoning: () => void;
  conductorModelOptions: Array<{ id: string; label: string }>;
  conductorModelPending: boolean;
  onConductorModelChange: (modelId: string) => void;
  pendingApproval: PendingApproval | null;
  pendingQuestion: { questionId: string; questions: UserQuestion[] } | null;
  onAnswerQuestion: (answers: Record<string, string | string[]>) => void;
  pendingPlan: { planId: string; plan: string } | null;
  onPlanDecision: (approved: boolean) => void;
  onStageHunk: (hunk: DiffHunk, decision: DiffHunkDecision) => Promise<void> | void;
  sessionExportPending: boolean;
  onExportSessionMarkdown: () => void;
  onExportSessionJson: () => void;
  awaySummary: AwaySummary | null;
  awaySummaryPending: boolean;
  awaySummaryAvailable: boolean;
  onRefreshAwaySummary: () => void;
  queuedMessages: QueuedMessage[];
  activeGoal: GoalSnapshotV1 | null;
  tasks: TaskRow[];
  artifacts: ArtifactRow[];
  learning: LearningProposal[];
  benchmarks: BenchmarkReport[];
  taskGraph: TaskNode[];
  teams: TeamDefinition[];
  teamStorePath: string;
  runtimeMessages: MessageHistoryRow[];
  messageStorePath: string;
  terminalProcesses: ExecutionProcessRow[];
  selectedTerminalProcessId: string | null;
  terminalLogs: ExecutionProcessLogs | null;
  terminalInput: string;
  terminalLoading: boolean;
  terminalError: string | null;
  memoryEntries: MemoryEntryRow[];
  memoryQuery: string;
  memoryDraft: string;
  memoryKeywordDraft: string;
  memoryActionPending: boolean;
  teamActionPending: string | null;
  teamRunPendingName: string | null;
  schedules: ScheduleRow[];
  scheduleStorePath: string;
  scheduleRunStorePath: string;
  scheduleAdminAuditStorePath: string;
  scheduleAdminAudit: ScheduleAdminAuditEvent[];
  scheduleActionPending: string | null;
  scheduleDraft: ScheduleDraft;
  webhookTriggers: WebhookTrigger[];
  webhookTriggerAudit: WebhookTriggerDeliveryAuditRow[];
  webhookTriggerPending: string | null;
  webhookTriggerLoading: boolean;
  webhookTriggerError: string | null;
  webhookTriggerDraft: WebhookTriggerDraft;
  taste: TasteScore[];
  roleMetrics: RoleMetric[];
  rollouts: RolloutView[];
  finetuning: FinetuningRow[];
  proposals: TasteProposal[];
  audit: AuditLogView;
  runtimeMiddlewareSnapshot: MiddlewareRegistrySnapshot;
  runtimeSpans: RuntimeSpan[];
  runtimeObservations: RuntimeObservation[];
  harnessRadar: HarnessRadarSnapshot;
  harnessRadarSchedulePending: boolean;
  specs: FeatureSpec[];
  selectedSpecId: string | null;
  activeSessionSpecId: string | null;
  sessionSpecCompletion: SessionSpecCompletion | null;
  specClarifyResult: SpecClarifyResponse | null;
  specPlanResult: SpecPlanResponse | null;
  specChecklistResult: SpecChecklistResponse | null;
  specImplementResult: SpecImplementResponse | null;
  specEnvelope: SpecEnvelope | null;
  specAnalysis: SpecAnalysisRun | null;
  specTrace: SpecTrace | null;
  specConstitutionDecisions: ConstitutionDecision[];
  specActionPending: string | null;
  specDraft: { title: string; description: string };
  specTaskDraft: {
    description: string;
    requirementIds: string;
    successCriterionIds: string;
    evidenceIds: string;
    foundationPhase: string;
  };
  specConstitutionDraft: {
    sourcePath: string;
    sourceSha256: string;
    rule: string;
    verdict: ConstitutionDecisionVerdict;
    rationale: string;
    evidenceIds: string;
  };
  recipes: InstalledRecipe[];
  selectedRecipeId: string | null;
  recipeJsonDraft: string;
  recipeUrlDraft: string;
  recipeParamsDraft: string;
  recipeValidation: RecipeValidationRun | null;
  renderedRecipe: RenderedRecipe | null;
  recipeRuns: RecipeRun[];
  recipeActionPending: string | null;
  recipeScheduleOpen: boolean;
  recipeScheduleDraft: RecipeScheduleDraft;
  runtimeTab: RuntimeTab;
  config: DaemonConfig | null;
  compareModels: ModelCatalogEntry[];
  onApprove: () => void;
  onDeny: () => void;
  onPrefillComposer: (text: string) => void;
  workspaceRoot: string;
  onOpenWorkspace: () => void;
  onSelectTerminalProcess: (id: string) => void;
  onTerminalInput: (value: string) => void;
  onCreateTerminalProcess: () => void;
  onRefreshTerminalProcesses: () => void;
  onSendTerminalInput: () => void;
  onKillTerminalProcess: (id: string) => void;
  onSteerQueued: (id: string) => void;
  onEditQueued: (id: string) => void;
  onDeleteQueued: (id: string) => void;
  onResume: () => void;
  onOpenDriverSettings: () => void;
  onPauseGoal: () => void;
  onResumeGoal: () => void;
  onEditGoal: (text: string) => void;
  onClearGoal: () => void;
  onEditMessage: (id: string) => void;
  canRollbackMessages: boolean;
  onRuntimeTab: (tab: RuntimeTab) => void;
  learningActionPending: string | null;
  onLearningDecision: (id: string, decision: "approve" | "reject") => void;
  harnessRadarPromotionPending: string | null;
  onPromoteHarnessRadarProposal: (id: string) => void;
  onScheduleHarnessRadarRefresh: () => void;
  onDeleteTeam: (name: string) => void;
  onRunTeam: (name: string, goal: string) => void;
  onDeleteSchedule: (id: string) => void;
  onScheduleDraft: (patch: Partial<ScheduleDraft>) => void;
  onEditSchedule: (schedule: ScheduleRow) => void;
  onResetScheduleDraft: () => void;
  onSaveSchedule: () => void;
  onScheduleTimeZone: (id: string, timeZone: string) => void;
  onRunSchedule: (id: string) => void;
  onSchedulePaused: (id: string, paused: boolean) => void;
  onRefreshWebhookTriggers: () => void;
  onWebhookTriggerDraft: (patch: Partial<WebhookTriggerDraft>) => void;
  onEditWebhookTrigger: (trigger: WebhookTrigger) => void;
  onResetWebhookTriggerDraft: () => void;
  onSaveWebhookTrigger: () => void;
  onWebhookTriggerEnabled: (id: string, enabled: boolean) => void;
  onDeleteWebhookTrigger: (id: string) => void;
  onSelectSpec: (id: string) => void;
  onSpecDraft: (patch: Partial<{ title: string; description: string }>) => void;
  onSpecTaskDraft: (
    patch: Partial<{
      description: string;
      requirementIds: string;
      successCriterionIds: string;
      evidenceIds: string;
      foundationPhase: string;
    }>,
  ) => void;
  onSpecConstitutionDraft: (
    patch: Partial<{
      sourcePath: string;
      sourceSha256: string;
      rule: string;
      verdict: ConstitutionDecisionVerdict;
      rationale: string;
      evidenceIds: string;
    }>,
  ) => void;
  onCreateSpec: () => void;
  onAddSpecTask: () => void;
  onAnalyzeSpec: () => void;
  onClarifySpec: () => void;
  onPlanSpec: () => void;
  onChecklistSpec: () => void;
  onImplementSpec: () => void;
  onRecordSpecConstitutionDecision: () => void;
  onBindSessionSpec: () => void;
  onClearSessionSpec: () => void;
  onSelectRecipe: (id: string) => void;
  onRecipeJsonDraft: (draft: string) => void;
  onRecipeUrlDraft: (draft: string) => void;
  onRecipeParamsDraft: (draft: string) => void;
  onValidateRecipeDraft: () => void;
  onInstallRecipeDraft: () => void;
  onInstallRecipeFile: () => void;
  onInstallRecipeUrl: () => void;
  onOpenRecipeRunTarget: (run: RecipeRun, target: "session" | "artifact" | "evidence") => void;
  onRenderSelectedRecipe: () => void;
  onRunSelectedRecipe: () => void;
  onOpenRecipeSchedule: () => void;
  onRecipeScheduleOpen: (open: boolean) => void;
  onRecipeScheduleDraft: (patch: Partial<RecipeScheduleDraft>) => void;
  onScheduleSelectedRecipe: () => void;
  benchmarkActionPending: boolean;
  terminalActionPending: string | null;
  onRunBenchmark: (body: BenchmarkRunRequest) => void;
  terminalBenchTasks: TerminalBenchTask[];
  terminalBenchActionPending: boolean;
  latestTerminalBenchResult: TerminalBenchRunResult | null;
  onRunTerminalBench: (taskId: string, agent: string) => void;
  onMemoryQuery: (query: string) => void;
  onMemoryDraft: (draft: string) => void;
  onMemoryKeywordDraft: (draft: string) => void;
  onAddMemory: () => void;
}
