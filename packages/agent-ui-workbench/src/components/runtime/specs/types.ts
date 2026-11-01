import type {
  ConstitutionDecision,
  ConstitutionDecisionVerdict,
  FeatureSpec,
  FeatureTask,
  SessionSpecCompletion,
  SpecAnalysisRun,
  SpecChecklistResponse,
  SpecClarifyResponse,
  SpecEnvelope,
  SpecImplementResponse,
  SpecPlanResponse,
  SpecTrace,
} from "@rightkit/agent-ui-core/lib/types";

export type SpecCoverageRowStatus = "covered" | "partial" | "missing";

export interface SpecCoverageRow {
  id: string;
  kind: string;
  label: string;
  mappedTasks: number;
  evidence: number;
  status: SpecCoverageRowStatus;
}

export interface SpecDraft {
  title: string;
  description: string;
}

export interface SpecTaskDraft {
  description: string;
  requirementIds: string;
  successCriterionIds: string;
  evidenceIds: string;
  foundationPhase: string;
}

export interface SpecConstitutionDraft {
  sourcePath: string;
  sourceSha256: string;
  rule: string;
  verdict: ConstitutionDecisionVerdict;
  rationale: string;
  evidenceIds: string;
}

export interface SpecSelectionCallbacks {
  onSelect: (id: string) => void;
}

export interface SpecCreateDraftProps {
  draft: SpecDraft;
  pending: string | null;
  onDraft: (patch: Partial<SpecDraft>) => void;
  onCreate: () => void;
}

export interface SpecListPaneProps extends SpecSelectionCallbacks, SpecCreateDraftProps {
  specs: FeatureSpec[];
  selectedSpecId: string | null;
}

export interface SpecExecutionActionsProps {
  specId: string;
  pending: string | null;
  selectedIsBound: boolean;
  hasSessionBinding: boolean;
  onClarify: () => void;
  onPlan: () => void;
  onChecklist: () => void;
  onAnalyze: () => void;
  onImplement: () => void;
  onBindSessionSpec: () => void;
  onClearSessionSpec: () => void;
}

export interface SpecMetricStripProps {
  spec: FeatureSpec;
}

export interface SpecWorkflowPanelProps {
  clarify: SpecClarifyResponse | null;
  plan: SpecPlanResponse | null;
  checklist: SpecChecklistResponse | null;
  implement: SpecImplementResponse | null;
}

export interface SpecCompletionGateProps {
  completion: SessionSpecCompletion;
}

export interface SpecCoverageMatrixProps {
  spec: FeatureSpec;
  tasks: FeatureTask[];
  trace: SpecTrace | null;
}

export interface SpecDetailPaneProps {
  spec: FeatureSpec;
  selectedIsBound: boolean;
  hasSessionBinding: boolean;
  pending: string | null;
  clarifyResult: SpecClarifyResponse | null;
  planResult: SpecPlanResponse | null;
  checklistResult: SpecChecklistResponse | null;
  implementResult: SpecImplementResponse | null;
  sessionSpecCompletion: SessionSpecCompletion | null;
  envelope: SpecEnvelope | null;
  trace: SpecTrace | null;
  onClarify: () => void;
  onPlan: () => void;
  onChecklist: () => void;
  onAnalyze: () => void;
  onImplement: () => void;
  onBindSessionSpec: () => void;
  onClearSessionSpec: () => void;
}

export interface SpecTaskEditorProps {
  specId: string;
  taskDraft: SpecTaskDraft;
  pending: string | null;
  tasks: FeatureTask[];
  onTaskDraft: (patch: Partial<SpecTaskDraft>) => void;
  onAddTask: () => void;
}

export interface SpecConstitutionPanelProps {
  spec: FeatureSpec;
  decisions: ConstitutionDecision[];
  draft: SpecConstitutionDraft;
  pending: boolean;
  onDraft: (patch: Partial<SpecConstitutionDraft>) => void;
  onRecord: () => void;
}

export interface SpecArtifactSummaryProps {
  specs: FeatureSpec[];
  envelope: SpecEnvelope | null;
  analysis: SpecAnalysisRun | null;
  trace: SpecTrace | null;
}

export interface SpecStatusBadgeProps {
  status: FeatureSpec["status"];
}

export interface SpecAnalysisBadgeProps {
  status: SpecAnalysisRun["status"];
}

export interface SpecCoverageStatusProps {
  status: SpecCoverageRowStatus;
}

export interface SpecConstitutionVerdictProps {
  verdict: ConstitutionDecisionVerdict;
}

export interface WorkflowSectionRow {
  id: string;
  label: string;
  detail: string;
}

export interface WorkflowSectionProps {
  title: string;
  meta: string;
  rows: WorkflowSectionRow[];
  empty: string;
}
