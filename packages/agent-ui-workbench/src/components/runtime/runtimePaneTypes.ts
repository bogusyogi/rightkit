import type {
  ArtifactRow,
  AuditLogView,
  BenchmarkRunRequest,
  BenchmarkReport,
  DaemonConfig,
  ConstitutionDecision,
  ConstitutionDecisionVerdict,
  FeatureSpec,
  FinetuningRow,
  HarnessRadarSnapshot,
  InstalledRecipe,
  LearningProposal,
  MemoryEntryRow,
  MessageHistoryRow,
  ModelCatalogEntry,
  MiddlewareRegistrySnapshot,
  RoleMetric,
  RolloutView,
  RuntimeObservation,
  RuntimeSpan,
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
  TeamDefinition,
  TaskNode,
  TasteProposal,
  TasteScore,
  TerminalBenchRunResult,
  TerminalBenchTask,
  WebhookTrigger,
  WebhookTriggerDeliveryAuditRow,
} from "@rightkit/agent-ui-core/lib/types";
import type { ScheduleDraft, WebhookTriggerDraft } from "@rightkit/agent-ui-hooks/hooks/useRuntimeAggregates";

export type RuntimeTab =
  | "context"
  | "taste"
  | "metrics"
  | "rollouts"
  | "finetuning"
  | "proposals"
  | "compare"
  | "audit"
  | "room"
  | "memory"
  | "messages"
  | "observability"
  | "harness"
  | "specs"
  | "recipes"
  | "teams"
  | "schedules"
  | "triggers"
  | "artifacts"
  | "learning"
  | "benchmarks"
  | "taskgraph"
  | "automations"
  | "inbox";

export type RuntimePaneProps = {
  artifacts: ArtifactRow[];
  learning: LearningProposal[];
  benchmarks: BenchmarkReport[];
  taskGraph: TaskNode[];
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
  runtimeMessages: MessageHistoryRow[];
  messageStorePath: string;
  memoryEntries: MemoryEntryRow[];
  memoryQuery: string;
  memoryDraft: string;
  memoryKeywordDraft: string;
  memoryActionPending: boolean;
  teams: TeamDefinition[];
  teamStorePath: string;
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
  runtimeTab: RuntimeTab;
  onRuntimeTab: (tab: RuntimeTab) => void;
  learningActionPending: string | null;
  onLearningDecision: (id: string, decision: "approve" | "reject") => void;
  harnessRadarPromotionPending: string | null;
  onPromoteHarnessRadarProposal: (id: string) => void;
  onScheduleHarnessRadarRefresh: () => void;
  onDeleteTeam: (name: string) => void;
  onRunTeam: (name: string, goal: string) => void;
  onMemoryQuery: (query: string) => void;
  onMemoryDraft: (draft: string) => void;
  onMemoryKeywordDraft: (draft: string) => void;
  onAddMemory: () => void;
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
  config: DaemonConfig | null;
  compareModels: ModelCatalogEntry[];
  selected: SessionRow | null;
  benchmarkActionPending: boolean;
  terminalBenchTasks: TerminalBenchTask[];
  terminalBenchActionPending: boolean;
  latestTerminalBenchResult: TerminalBenchRunResult | null;
  onRunBenchmark: (body: BenchmarkRunRequest) => void;
  onRunTerminalBench: (taskId: string, agent: string) => void;
};
