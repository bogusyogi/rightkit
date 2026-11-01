import SpecExecutionActions from "./SpecExecutionActions";
import { SpecStatusBadge } from "./SpecListPane";
import SpecConstitutionPanel from "./SpecConstitutionPanel";
import type {
  SpecCompletionGateProps,
  SpecConstitutionDraft,
  SpecDetailPaneProps,
  SpecMetricStripProps,
  SpecWorkflowPanelProps,
  WorkflowSectionProps,
} from "./types";
import type {
  FeatureTask,
  SpecChecklistItem,
  SpecClarification,
  SpecFinding,
} from "@rightkit/agent-ui-core/lib/types";

function SpecMetric({ label, value }: { label: string; value: number }) {
  return (
    <div className="rounded-md border border-line bg-panel px-3 py-2">
      <span className="block text-[10px] font-semibold uppercase text-text-3">{label}</span>
      <span className="mt-1 block text-[15px] font-semibold text-text">{value}</span>
    </div>
  );
}

function SpecMetricStrip({ spec }: SpecMetricStripProps) {
  return (
    <div className="mt-3 grid grid-cols-4 gap-2">
      <SpecMetric label="Stories" value={spec.userStories.length} />
      <SpecMetric label="Requirements" value={spec.functionalRequirements.length} />
      <SpecMetric label="Criteria" value={spec.successCriteria.length} />
      <SpecMetric label="Constitution" value={spec.constitutionSources.length} />
    </div>
  );
}

function WorkflowSection({ title, meta, rows, empty }: WorkflowSectionProps) {
  return (
    <div className="rounded border border-line bg-bg px-2.5 py-2">
      <div className="flex min-w-0 items-center gap-2">
        <strong className="shrink-0 text-[11px] text-text-2">{title}</strong>
        <span className="min-w-0 truncate text-[10px] text-text-3">{meta}</span>
      </div>
      {rows.length ? (
        <div className="mt-1.5 grid gap-1">
          {rows.map((row) => (
            <div className="grid grid-cols-[auto_minmax(0,1fr)] gap-2" key={row.id}>
              <code className="text-[10px] text-accent-2">{row.label}</code>
              <span className="min-w-0 truncate text-[11px] text-text-2">{row.detail}</span>
            </div>
          ))}
        </div>
      ) : (
        <p className="mt-1.5 text-[11px] text-text-3">{empty}</p>
      )}
    </div>
  );
}

function SpecWorkflowPanel({ clarify, plan, checklist, implement }: SpecWorkflowPanelProps) {
  if (!clarify && !plan && !checklist && !implement) return null;
  return (
    <div className="mt-3 grid gap-2 rounded-md border border-line bg-panel px-3 py-3">
      <div className="flex items-center gap-2">
        <strong className="text-[12px] text-text">Spec workflow</strong>
      </div>
      {clarify ? (
        <WorkflowSection
          title="Clarify"
          meta={`${clarify.questions.length} questions · next ${clarify.nextAction}`}
          rows={clarify.questions.slice(0, 4).map((question: SpecClarification) => ({
            id: question.id,
            label: question.sourceId,
            detail: question.question,
          }))}
          empty="No clarification questions."
        />
      ) : null}
      {plan ? (
        <WorkflowSection
          title="Plan"
          meta={`${plan.createdTaskCount} new tasks · ${plan.skippedExistingCount} already covered`}
          rows={plan.tasks.slice(0, 5).map((task: FeatureTask) => ({
            id: task.id,
            label: task.id,
            detail: task.description,
          }))}
          empty="No generated tasks."
        />
      ) : null}
      {checklist ? (
        <WorkflowSection
          title="Checklist"
          meta={`${checklist.blockerCount} blockers · ${checklist.warningCount} warnings`}
          rows={checklist.items
            .filter((item: SpecChecklistItem) => item.status !== "done")
            .slice(0, 5)
            .map((item: SpecChecklistItem) => ({
              id: item.id,
              label: item.status,
              detail: item.detail,
            }))}
          empty="Checklist has no open blockers."
        />
      ) : null}
      {implement ? (
        <WorkflowSection
          title="Implement"
          meta={implement.sessionId ? `session ${implement.sessionId}` : "prompt prepared"}
          rows={[
            {
              id: "implement",
              label: implement.accepted ? "started" : implement.dryRun ? "dry-run" : "prepared",
              detail: implement.prompt.split("\n").find(Boolean) ?? "Implementation prompt ready.",
            },
          ]}
          empty="No implementation result."
        />
      ) : null}
    </div>
  );
}

function SpecCompletionGate({ completion }: SpecCompletionGateProps) {
  const coverage = completion.coverage;
  const gateClass = completion.canComplete
    ? completion.warningCount > 0
      ? "border-warning text-warning"
      : "border-accent-2 text-accent-2"
    : "border-danger text-danger";
  const gateLabel = completion.canComplete
    ? completion.warningCount > 0
      ? "Ready with warnings"
      : "Ready"
    : "Blocked";
  const findings = completion.canComplete ? completion.warnings.slice(0, 3) : completion.blockers;

  return (
    <div className="mt-3 rounded-md border border-line bg-panel px-3 py-3">
      <div className="flex min-w-0 items-center gap-2">
        <span className={`rounded border bg-bg px-2 py-0.5 text-[10px] font-semibold ${gateClass}`}>
          {gateLabel}
        </span>
        <strong className="min-w-0 flex-1 truncate text-[12px] text-text">Completion gate</strong>
        <span className="shrink-0 text-[11px] text-text-3">
          {completion.blockerCount} blockers · {completion.warningCount} warnings
        </span>
      </div>
      {coverage ? (
        <p className="mt-2 text-[11px] text-text-3">
          {coverage.requirementsWithTasks}/{coverage.requirementCount} requirements mapped ·{" "}
          {coverage.tasksWithMapping}/{coverage.taskCount} tasks mapped ·{" "}
          {coverage.buildableCriteriaWithEvidence}/{coverage.successCriteriaCount} criteria
          evidenced
        </p>
      ) : null}
      {findings.length ? (
        <div className="mt-2 grid gap-1.5">
          {findings.map((finding: SpecFinding) => (
            <div
              className="grid grid-cols-[auto_minmax(0,1fr)] gap-2 rounded border border-line bg-bg px-2 py-1.5"
              key={finding.id}
            >
              <code className="text-[10px] text-accent-2">{finding.kind}</code>
              <span className="min-w-0 truncate text-[11px] text-text-2">{finding.message}</span>
            </div>
          ))}
        </div>
      ) : (
        <p className="mt-2 text-[11px] text-text-3">
          No blocking spec findings are currently preventing completion.
        </p>
      )}
    </div>
  );
}

function SpecHeader({
  spec,
  selectedIsBound,
}: {
  spec: SpecDetailPaneProps["spec"];
  selectedIsBound: boolean;
}) {
  return (
    <div className="flex items-start gap-3">
      <div className="min-w-0 flex-1">
        <h3 className="truncate text-[16px] font-semibold text-text">{spec.title}</h3>
        <p className="mt-1 truncate text-[11px] text-text-3">
          {spec.workspaceRoot} · {spec.updatedAt}
        </p>
        <code className="mt-1 block truncate text-[11px] text-accent-2">
          prompt {spec.sourcePromptHash}
        </code>
      </div>
      <SpecStatusBadge status={spec.status} />
      {selectedIsBound ? (
        <span className="inline-flex h-8 items-center rounded-md border border-accent-2 bg-panel px-3 text-[12px] font-semibold text-accent-2">
          Bound
        </span>
      ) : null}
    </div>
  );
}

export default function SpecDetailPane({
  spec,
  selectedIsBound,
  hasSessionBinding,
  pending,
  clarifyResult,
  planResult,
  checklistResult,
  implementResult,
  sessionSpecCompletion,
  onClarify,
  onPlan,
  onChecklist,
  onAnalyze,
  onImplement,
  onBindSessionSpec,
  onClearSessionSpec,
}: SpecDetailPaneProps) {
  const constitutionDraft: SpecConstitutionDraft = {
    sourcePath: "",
    sourceSha256: "",
    rule: "",
    verdict: "compliant",
    rationale: "",
    evidenceIds: "",
  };

  return (
    <div className="px-5 py-4">
      <SpecHeader spec={spec} selectedIsBound={selectedIsBound} />
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <SpecExecutionActions
          specId={spec.id}
          pending={pending}
          selectedIsBound={selectedIsBound}
          hasSessionBinding={hasSessionBinding}
          onClarify={onClarify}
          onPlan={onPlan}
          onChecklist={onChecklist}
          onAnalyze={onAnalyze}
          onImplement={onImplement}
          onBindSessionSpec={onBindSessionSpec}
          onClearSessionSpec={onClearSessionSpec}
        />
      </div>
      <SpecMetricStrip spec={spec} />
      {selectedIsBound && sessionSpecCompletion ? (
        <SpecCompletionGate completion={sessionSpecCompletion} />
      ) : null}
      <SpecWorkflowPanel
        clarify={clarifyResult}
        plan={planResult}
        checklist={checklistResult}
        implement={implementResult}
      />
      <SpecConstitutionPanel
        spec={spec}
        decisions={[]}
        draft={constitutionDraft}
        pending={pending === `constitution:${spec.id}`}
        onDraft={() => {}}
        onRecord={() => {}}
      />
    </div>
  );
}

export { SpecHeader, SpecMetricStrip, SpecWorkflowPanel, SpecCompletionGate };
