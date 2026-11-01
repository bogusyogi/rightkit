import { Select } from "@rightkit/react-shell/react";
import ComparePanel from "./ComparePanel.js";
import {
  AuditPanel,
  BenchmarksPanel,
  FinetuningPanel,
  LearningPanel,
  MetricsPanel,
  ProposalsPanel,
  RolloutsPanel,
  TastePanel,
} from "./RuntimePanels.js";
import TeamsRuntimePanel from "./runtime/TeamsRuntimePanel.js";
import SchedulesRuntimePanel from "./runtime/SchedulesRuntimePanel.js";
import WebhookTriggersRuntimePanel from "./runtime/WebhookTriggersRuntimePanel.js";
import ObservabilityRuntimePanel from "./runtime/ObservabilityRuntimePanel.js";
import HarnessRadarPanel from "./runtime/HarnessRadarPanel.js";
import SpecsRuntimePanel from "./runtime/SpecsRuntimePanel.js";
import RecipesRuntimePanel from "./runtime/RecipesRuntimePanel.js";
import MessagesRuntimePanel from "./runtime/MessagesRuntimePanel.js";
import MemoryRuntimePanel from "./runtime/MemoryRuntimePanel.js";
import ContextRuntimePanel from "./runtime/ContextRuntimePanel.js";
import ReviewRoomPanel from "./runtime/ReviewRoomPanel.js";
import ArtifactRowsPanel from "./runtime/ArtifactRowsPanel.js";
import { AutomationsPanel, WorkflowInboxPanel } from "./runtime/WorkflowControlPanels.js";
import { runtimeLabel } from "./runtime/runtimeTabs.js";
import { RUNTIME_TABS } from "./runtime/runtimePaneTabs.js";
import { renderRows } from "./runtime/runtimeTabsRenderRows.js";
import type { RuntimePaneProps, RuntimeTab } from "./runtime/runtimePaneTypes.js";

export default function RuntimePane({
  artifacts,
  learning,
  benchmarks,
  taskGraph,
  taste,
  roleMetrics,
  rollouts,
  finetuning,
  proposals,
  audit,
  runtimeMiddlewareSnapshot,
  runtimeSpans,
  runtimeObservations,
  harnessRadar,
  harnessRadarSchedulePending,
  specs,
  selectedSpecId,
  activeSessionSpecId,
  sessionSpecCompletion,
  specClarifyResult,
  specPlanResult,
  specChecklistResult,
  specImplementResult,
  specEnvelope,
  specAnalysis,
  specTrace,
  specConstitutionDecisions,
  specActionPending,
  specDraft,
  specTaskDraft,
  specConstitutionDraft,
  recipes,
  selectedRecipeId,
  recipeJsonDraft,
  recipeUrlDraft,
  recipeParamsDraft,
  recipeValidation,
  renderedRecipe,
  recipeRuns,
  recipeActionPending,
  recipeScheduleOpen,
  recipeScheduleDraft,
  runtimeMessages,
  messageStorePath,
  memoryEntries,
  memoryQuery,
  memoryDraft,
  memoryKeywordDraft,
  memoryActionPending,
  teams,
  teamStorePath,
  teamActionPending,
  teamRunPendingName,
  schedules,
  scheduleStorePath,
  scheduleRunStorePath,
  scheduleAdminAuditStorePath,
  scheduleAdminAudit,
  scheduleActionPending,
  scheduleDraft,
  webhookTriggers,
  webhookTriggerAudit,
  webhookTriggerPending,
  webhookTriggerLoading,
  webhookTriggerError,
  webhookTriggerDraft,
  runtimeTab,
  onRuntimeTab,
  learningActionPending,
  onLearningDecision,
  harnessRadarPromotionPending,
  onPromoteHarnessRadarProposal,
  onScheduleHarnessRadarRefresh,
  onDeleteTeam,
  onRunTeam,
  onMemoryQuery,
  onMemoryDraft,
  onMemoryKeywordDraft,
  onAddMemory,
  onDeleteSchedule,
  onScheduleDraft,
  onEditSchedule,
  onResetScheduleDraft,
  onSaveSchedule,
  onScheduleTimeZone,
  onRunSchedule,
  onSchedulePaused,
  onRefreshWebhookTriggers,
  onWebhookTriggerDraft,
  onEditWebhookTrigger,
  onResetWebhookTriggerDraft,
  onSaveWebhookTrigger,
  onWebhookTriggerEnabled,
  onDeleteWebhookTrigger,
  onSelectSpec,
  onSpecDraft,
  onSpecTaskDraft,
  onSpecConstitutionDraft,
  onCreateSpec,
  onAddSpecTask,
  onAnalyzeSpec,
  onClarifySpec,
  onPlanSpec,
  onChecklistSpec,
  onImplementSpec,
  onRecordSpecConstitutionDecision,
  onBindSessionSpec,
  onClearSessionSpec,
  onSelectRecipe,
  onRecipeJsonDraft,
  onRecipeUrlDraft,
  onRecipeParamsDraft,
  onValidateRecipeDraft,
  onInstallRecipeDraft,
  onInstallRecipeFile,
  onInstallRecipeUrl,
  onOpenRecipeRunTarget,
  onRenderSelectedRecipe,
  onRunSelectedRecipe,
  onOpenRecipeSchedule,
  onRecipeScheduleOpen,
  onRecipeScheduleDraft,
  onScheduleSelectedRecipe,
  config,
  compareModels,
  selected,
  benchmarkActionPending,
  terminalBenchTasks,
  terminalBenchActionPending,
  latestTerminalBenchResult,
  onRunBenchmark,
  onRunTerminalBench,
}: RuntimePaneProps) {
  return (
    <section className="flex flex-col flex-1 min-h-0 overflow-hidden">
      <div className="flex items-center gap-4 px-6 py-4 border-b border-line flex-shrink-0">
        <h2 className="text-[16px] font-bold text-text">Runtime</h2>
        <label className="ml-auto inline-flex items-center gap-2 text-[12px] text-text-3">
          <span className="sr-only">Runtime view</span>
          <Select value={runtimeTab} options={[...RUNTIME_TABS.map((tab) => ({ value: tab, label: runtimeLabel(tab) }))]} onChange={(value) => onRuntimeTab(value as RuntimeTab)} label="Runtime view" className="h-8 rounded-lg border border-line bg-panel px-2.5 text-[12px] text-text-2 outline-none cursor-pointer hover:border-line-2 hover:text-text" />
        </label>
      </div>
      {runtimeTab === "context" && <ContextRuntimePanel config={config} />}
      {runtimeTab === "taste" && <TastePanel rows={taste} />}
      {runtimeTab === "metrics" && <MetricsPanel rows={roleMetrics} />}
      {runtimeTab === "rollouts" && <RolloutsPanel rows={rollouts} />}
      {runtimeTab === "finetuning" && <FinetuningPanel rows={finetuning} />}
      {runtimeTab === "proposals" && <ProposalsPanel rows={proposals} />}
      {runtimeTab === "learning" && (
        <LearningPanel
          rows={learning}
          pendingId={learningActionPending}
          onDecision={onLearningDecision}
        />
      )}
      {runtimeTab === "compare" && <ComparePanel config={config} models={compareModels} />}
      {runtimeTab === "audit" && <AuditPanel view={audit} />}
      {runtimeTab === "room" && <ReviewRoomPanel />}
      {runtimeTab === "memory" && (
        <MemoryRuntimePanel
          entries={memoryEntries}
          query={memoryQuery}
          draft={memoryDraft}
          keywordDraft={memoryKeywordDraft}
          pending={memoryActionPending}
          onQuery={onMemoryQuery}
          onDraft={onMemoryDraft}
          onKeywordDraft={onMemoryKeywordDraft}
          onAdd={onAddMemory}
        />
      )}
      {runtimeTab === "messages" && (
        <MessagesRuntimePanel messages={runtimeMessages} messageStorePath={messageStorePath} />
      )}
      {runtimeTab === "observability" && (
        <ObservabilityRuntimePanel
          middleware={runtimeMiddlewareSnapshot}
          spans={runtimeSpans}
          observations={runtimeObservations}
        />
      )}
      {runtimeTab === "harness" && (
        <HarnessRadarPanel
          snapshot={harnessRadar}
          pendingPromotionId={harnessRadarPromotionPending}
          schedulePending={harnessRadarSchedulePending}
          onPromoteProposal={onPromoteHarnessRadarProposal}
          onScheduleRefresh={onScheduleHarnessRadarRefresh}
        />
      )}
      {runtimeTab === "specs" && (
        <SpecsRuntimePanel
          specs={specs}
          selectedSpecId={selectedSpecId}
          activeSessionSpecId={activeSessionSpecId}
          sessionSpecCompletion={sessionSpecCompletion}
          clarifyResult={specClarifyResult}
          planResult={specPlanResult}
          checklistResult={specChecklistResult}
          implementResult={specImplementResult}
          envelope={specEnvelope}
          analysis={specAnalysis}
          trace={specTrace}
          constitutionDecisions={specConstitutionDecisions}
          pending={specActionPending}
          draft={specDraft}
          taskDraft={specTaskDraft}
          constitutionDraft={specConstitutionDraft}
          onSelect={onSelectSpec}
          onDraft={onSpecDraft}
          onTaskDraft={onSpecTaskDraft}
          onConstitutionDraft={onSpecConstitutionDraft}
          onCreate={onCreateSpec}
          onAddTask={onAddSpecTask}
          onAnalyze={onAnalyzeSpec}
          onClarify={onClarifySpec}
          onPlan={onPlanSpec}
          onChecklist={onChecklistSpec}
          onImplement={onImplementSpec}
          onRecordConstitutionDecision={onRecordSpecConstitutionDecision}
          onBindSessionSpec={onBindSessionSpec}
          onClearSessionSpec={onClearSessionSpec}
        />
      )}
      {runtimeTab === "recipes" && (
        <RecipesRuntimePanel
          recipes={recipes}
          selectedRecipeId={selectedRecipeId}
          recipeJsonDraft={recipeJsonDraft}
          recipeUrlDraft={recipeUrlDraft}
          recipeParamsDraft={recipeParamsDraft}
          recipeValidation={recipeValidation}
          renderedRecipe={renderedRecipe}
          recipeRuns={recipeRuns}
          pendingId={recipeActionPending}
          scheduleOpen={recipeScheduleOpen}
          scheduleDraft={recipeScheduleDraft}
          onSelectRecipe={onSelectRecipe}
          onRecipeJsonDraft={onRecipeJsonDraft}
          onRecipeUrlDraft={onRecipeUrlDraft}
          onRecipeParamsDraft={onRecipeParamsDraft}
          onValidateDraft={onValidateRecipeDraft}
          onInstallDraft={onInstallRecipeDraft}
          onInstallFile={onInstallRecipeFile}
          onInstallUrl={onInstallRecipeUrl}
          onOpenRunTarget={onOpenRecipeRunTarget}
          onRenderSelected={onRenderSelectedRecipe}
          onRunSelected={onRunSelectedRecipe}
          onOpenSchedule={onOpenRecipeSchedule}
          onScheduleOpen={onRecipeScheduleOpen}
          onScheduleDraft={onRecipeScheduleDraft}
          onScheduleSelected={onScheduleSelectedRecipe}
        />
      )}
      {runtimeTab === "teams" && (
        <TeamsRuntimePanel
          teams={teams}
          teamStorePath={teamStorePath}
          pendingName={teamActionPending}
          runPendingName={teamRunPendingName}
          selectedSessionId={selected?.id}
          onDelete={onDeleteTeam}
          onRun={onRunTeam}
        />
      )}
      {runtimeTab === "schedules" && (
        <SchedulesRuntimePanel
          schedules={schedules}
          scheduleStorePath={scheduleStorePath}
          scheduleRunStorePath={scheduleRunStorePath}
          scheduleAdminAuditStorePath={scheduleAdminAuditStorePath}
          scheduleAdminAudit={scheduleAdminAudit}
          pendingId={scheduleActionPending}
          draft={scheduleDraft}
          onDelete={onDeleteSchedule}
          onDraft={onScheduleDraft}
          onEdit={onEditSchedule}
          onResetDraft={onResetScheduleDraft}
          onSave={onSaveSchedule}
          onTimeZone={onScheduleTimeZone}
          onRun={onRunSchedule}
          onPaused={onSchedulePaused}
        />
      )}
      {runtimeTab === "triggers" && (
        <WebhookTriggersRuntimePanel
          connected={Boolean(config)}
          daemonUrl={config?.url || ""}
          triggers={webhookTriggers}
          audit={webhookTriggerAudit}
          loading={webhookTriggerLoading}
          error={webhookTriggerError}
          pendingId={webhookTriggerPending}
          draft={webhookTriggerDraft}
          onRefresh={onRefreshWebhookTriggers}
          onDraft={onWebhookTriggerDraft}
          onEdit={onEditWebhookTrigger}
          onResetDraft={onResetWebhookTriggerDraft}
          onSave={onSaveWebhookTrigger}
          onEnabled={onWebhookTriggerEnabled}
          onDelete={onDeleteWebhookTrigger}
        />
      )}
      {runtimeTab === "automations" && <AutomationsPanel config={config} />}
      {runtimeTab === "inbox" && <WorkflowInboxPanel config={config} onRuntimeTab={onRuntimeTab} />}
      {(runtimeTab === "artifacts" || runtimeTab === "taskgraph") && (
        <div className="flex-1 min-h-0 overflow-y-auto flex flex-col divide-y divide-line">
          {runtimeTab === "artifacts" && <ArtifactRowsPanel artifacts={artifacts} />}
          {runtimeTab === "taskgraph" &&
            renderRows(taskGraph, (t) => [t.title, t.doneWhen, String(t.state)])}
        </div>
      )}
      {runtimeTab === "benchmarks" && (
        <BenchmarksPanel
          rows={benchmarks}
          selectedSessionId={selected?.id}
          pending={benchmarkActionPending}
          terminalBenchTasks={terminalBenchTasks}
          terminalBenchPending={terminalBenchActionPending}
          latestTerminalBenchResult={latestTerminalBenchResult}
          onRunBenchmark={onRunBenchmark}
          onRunTerminalBench={onRunTerminalBench}
        />
      )}
    </section>
  );
}

export type { RuntimeTab } from "./runtime/runtimePaneTypes.js";
