import {
  EmptyState,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeaderCell,
  TableRow,
} from "@rightkit/react-shell/react";
import RuntimeStat from "../RuntimeStat";
import type {
  SpecAnalysisBadgeProps,
  SpecArtifactSummaryProps,
  SpecCoverageMatrixProps,
  SpecCoverageRow,
  SpecCoverageStatusProps,
} from "./types";
import type {
  FeatureSpec,
  FeatureTask,
  SpecAnalysisRun,
  SpecEvidence,
  SpecFinding,
  SpecTrace,
} from "@rightkit/agent-ui-core/lib/types";

function SpecAnalysisBadge({ status }: SpecAnalysisBadgeProps) {
  const color =
    status === "pass"
      ? "border-accent-2 text-accent-2"
      : status === "block"
        ? "border-danger text-danger"
        : "border-warning text-warning";
  return (
    <span className={`rounded border bg-panel px-2 py-0.5 text-[10px] font-semibold ${color}`}>
      {status}
    </span>
  );
}

function SpecCoverageStatus({ status }: SpecCoverageStatusProps) {
  const label = status === "covered" ? "covered" : status === "partial" ? "partial" : "missing";
  const className =
    status === "covered"
      ? "border-accent-2 text-accent-2"
      : status === "partial"
        ? "border-warning text-warning"
        : "border-line text-text-3";
  return (
    <span
      className={`inline-flex rounded border bg-bg px-2 py-0.5 text-[10px] font-semibold uppercase ${className}`}
    >
      {label}
    </span>
  );
}

function countTaskEvidence(tasks: FeatureTask[], evidenceByTask: Map<string, number>) {
  return tasks.reduce(
    (count, task) => count + (evidenceByTask.get(task.id) ?? task.evidenceIds.length),
    0,
  );
}

function coverageStatus(mappedTasks: number, evidence: number): SpecCoverageRow["status"] {
  if (mappedTasks > 0 && evidence > 0) return "covered";
  if (mappedTasks > 0) return "partial";
  return "missing";
}

function buildSpecCoverageRows(
  spec: FeatureSpec,
  tasks: FeatureTask[],
  trace: SpecTrace | null,
): SpecCoverageRow[] {
  const evidenceByTask = new Map<string, number>();
  for (const evidence of trace?.evidence ?? []) {
    if (evidence.taskId) {
      evidenceByTask.set(evidence.taskId, (evidenceByTask.get(evidence.taskId) ?? 0) + 1);
    }
  }

  const rows: SpecCoverageRow[] = [];
  for (const story of spec.userStories) {
    const storyTasks = tasks.filter((task) => task.storyId === story.id);
    rows.push({
      id: story.id,
      kind: "story",
      label: story.title,
      mappedTasks: storyTasks.length,
      evidence: countTaskEvidence(storyTasks, evidenceByTask),
      status: coverageStatus(storyTasks.length, countTaskEvidence(storyTasks, evidenceByTask)),
    });
  }
  for (const requirement of spec.functionalRequirements) {
    const requirementTasks = tasks.filter((task) => task.requirementIds.includes(requirement.id));
    const evidenceCount = countTaskEvidence(requirementTasks, evidenceByTask);
    rows.push({
      id: requirement.id,
      kind: "requirement",
      label: requirement.text,
      mappedTasks: requirementTasks.length,
      evidence: evidenceCount,
      status: coverageStatus(requirementTasks.length, evidenceCount),
    });
  }
  for (const criterion of spec.successCriteria) {
    const criterionTasks = tasks.filter((task) => task.successCriterionIds.includes(criterion.id));
    const evidenceCount = countTaskEvidence(criterionTasks, evidenceByTask);
    rows.push({
      id: criterion.id,
      kind: criterion.buildable ? "criterion" : "criterion/ref",
      label: criterion.text,
      mappedTasks: criterionTasks.length,
      evidence: evidenceCount,
      status: criterion.buildable
        ? coverageStatus(criterionTasks.length, evidenceCount)
        : criterionTasks.length > 0
          ? "covered"
          : "partial",
    });
  }
  for (const task of tasks) {
    const mapped =
      Boolean(task.storyId) ||
      task.requirementIds.length > 0 ||
      task.successCriterionIds.length > 0 ||
      Boolean(task.foundationPhase);
    const evidenceCount = evidenceByTask.get(task.id) ?? task.evidenceIds.length;
    rows.push({
      id: task.id,
      kind: "task",
      label: task.description,
      mappedTasks: mapped ? 1 : 0,
      evidence: evidenceCount,
      status: mapped ? (evidenceCount > 0 ? "covered" : "partial") : "missing",
    });
  }
  return rows;
}

function SpecCoverageMatrix({ spec, tasks, trace }: SpecCoverageMatrixProps) {
  const rows = buildSpecCoverageRows(spec, tasks, trace);
  const missing = rows.filter((row) => row.status === "missing").length;
  const partial = rows.filter((row) => row.status === "partial").length;
  return (
    <div className="mt-3 rounded-md border border-line bg-panel">
      <div className="flex items-center gap-3 border-b border-line px-3 py-2">
        <strong className="text-[12px] text-text">Coverage matrix</strong>
        <span className="text-[11px] text-text-3">
          {rows.length - missing}/{rows.length} covered · {partial} partial
        </span>
      </div>
      {rows.length === 0 ? (
        <div className="px-3 py-3 text-[12px] text-text-3">
          Coverage rows appear after the spec contains stories, requirements, criteria, or tasks.
        </div>
      ) : (
        <div className="max-h-72 overflow-y-auto">
          <Table dense label="Coverage matrix" className="table-fixed">
            <TableHead className="sticky top-0 bg-panel-2">
              <TableRow>
                <TableHeaderCell className="w-24">Kind</TableHeaderCell>
                <TableHeaderCell>Item</TableHeaderCell>
                <TableHeaderCell className="w-20">Tasks</TableHeaderCell>
                <TableHeaderCell className="w-24">Evidence</TableHeaderCell>
                <TableHeaderCell className="w-24">Status</TableHeaderCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {rows.map((row) => (
                <TableRow key={`${row.kind}:${row.id}`}>
                  <TableCell className="align-top text-text-3">{row.kind}</TableCell>
                  <TableCell className="min-w-0 align-top">
                    <code className="mr-2 text-accent-2">{row.id}</code>
                    <span className="text-text-2">{row.label}</span>
                  </TableCell>
                  <TableCell className="align-top">{row.mappedTasks}</TableCell>
                  <TableCell className="align-top">{row.evidence}</TableCell>
                  <TableCell className="align-top">
                    <SpecCoverageStatus status={row.status} />
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
    </div>
  );
}

function SpecAnalysisPanel({ analysis }: { analysis: SpecAnalysisRun }) {
  return (
    <div className="flex flex-col divide-y divide-line">
      <div className="px-5 py-3">
        <div className="flex items-center gap-2">
          <SpecAnalysisBadge status={analysis.status} />
          <code className="truncate text-[11px] text-text-3">{analysis.id}</code>
        </div>
        <p className="mt-2 text-[11px] text-text-3">
          {analysis.coverage.requirementsWithTasks}/{analysis.coverage.requirementCount}{" "}
          requirements mapped · {analysis.coverage.buildableCriteriaWithEvidence}/
          {analysis.coverage.successCriteriaCount} criteria proven
        </p>
      </div>
      {analysis.findings.length === 0 ? (
        <div className="px-5 py-3 text-[12px] text-text-2">No findings.</div>
      ) : (
        analysis.findings.map((finding: SpecFinding) => (
          <div className="px-5 py-3" key={finding.id}>
            <div className="flex items-center gap-2">
              <span className="rounded border border-line bg-panel px-2 py-0.5 text-[10px] text-text-3">
                {finding.severity}
              </span>
              <code className="truncate text-[11px] text-accent-2">{finding.kind}</code>
            </div>
            <p className="mt-1 text-[12px] leading-relaxed text-text-2">{finding.message}</p>
          </div>
        ))
      )}
    </div>
  );
}

function SpecEvidencePanel({ trace }: { trace: SpecTrace | null }) {
  if (!trace?.evidence.length) {
    return (
      <EmptyState title="No trace evidence">{"Hash-bound artifacts with spec metadata appear here after implementation work produces evidence."}</EmptyState>
    );
  }
  return (
    <div className="flex flex-col divide-y divide-line">
      {trace.evidence.map((item: SpecEvidence) => (
        <div className="px-5 py-3" key={item.id}>
          <div className="flex items-center gap-2">
            <strong className="min-w-0 flex-1 truncate text-[12px] text-text">{item.path}</strong>
            <span className="text-[11px] text-text-3">
              {item.verdictPassed === false ? "failed" : item.shippable ? "shippable" : "open"}
            </span>
          </div>
          <code className="mt-1 block truncate text-[11px] text-accent-2">{item.contentHash}</code>
        </div>
      ))}
    </div>
  );
}

export default function SpecArtifactSummary({
  specs,
  envelope,
  analysis,
  trace,
}: SpecArtifactSummaryProps) {
  const coverage = analysis?.coverage;
  const spec = envelope?.spec;
  return (
    <div className="min-w-0">
      <div className="grid grid-cols-5 gap-3 border-b border-line px-5 py-3">
        <RuntimeStat label="Specs" value={specs.length} />
        <RuntimeStat label="Tasks" value={envelope?.tasks.length ?? 0} />
        <RuntimeStat label="Req mapped" value={coverage?.requirementsWithTasks ?? 0} />
        <RuntimeStat label="Criteria proven" value={coverage?.buildableCriteriaWithEvidence ?? 0} />
        <RuntimeStat label="Trace links" value={trace?.links.length ?? 0} />
      </div>
      <div className="border-b border-line px-5 py-2 text-[11px] font-semibold uppercase text-text-3">
        Analysis
      </div>
      {analysis ? (
        <SpecAnalysisPanel analysis={analysis} />
      ) : (
        <EmptyState title="No analysis">{"Run analysis to see requirement, task, and evidence coverage."}</EmptyState>
      )}
      <div className="border-y border-line px-5 py-2 text-[11px] font-semibold uppercase text-text-3">
        Evidence
      </div>
      <SpecEvidencePanel trace={trace} />
      {spec ? <SpecCoverageMatrix spec={spec} tasks={envelope?.tasks ?? []} trace={trace} /> : null}
    </div>
  );
}

export { SpecCoverageMatrix, SpecCoverageStatus, SpecAnalysisBadge, buildSpecCoverageRows };
