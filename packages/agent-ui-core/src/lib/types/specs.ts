export type SpecStatus = "draft" | "planned" | "implementing" | "done" | "blocked";
export type FeatureTaskStatus = "todo" | "inProgress" | "done" | "blocked";
export type TraceKind =
  | "story"
  | "requirement"
  | "successCriterion"
  | "task"
  | "evidence"
  | "constitution";
export type SpecAnalysisStatus = "pass" | "warn" | "block";
export type SpecFindingSeverity = "info" | "warning" | "critical";

export interface ConstitutionSource {
  scope: string;
  path: string;
  bodySha256: string;
}

export type ConstitutionDecisionVerdict = "compliant" | "violation" | "overridden";

export interface ConstitutionDecision {
  id: string;
  specId: string;
  sourcePath: string;
  sourceSha256: string;
  rule: string;
  verdict: ConstitutionDecisionVerdict;
  rationale: string;
  evidenceIds: string[];
  createdAt: string;
  updatedAt: string;
}

export interface AcceptanceScenario {
  id: string;
  given: string;
  when: string;
  then: string;
}

export interface UserStory {
  id: string;
  priority: number;
  title: string;
  narrative: string;
  independentTest: string;
  acceptanceScenarios: AcceptanceScenario[];
}

export interface Requirement {
  id: string;
  text: string;
  sourceStoryId?: string | null;
  needsClarification: boolean;
}

export interface SuccessCriterion {
  id: string;
  text: string;
  buildable: boolean;
}

export interface FeatureSpec {
  id: string;
  workspaceRoot: string;
  branch?: string | null;
  title: string;
  status: SpecStatus;
  sourcePromptHash: string;
  userStories: UserStory[];
  functionalRequirements: Requirement[];
  successCriteria: SuccessCriterion[];
  assumptions: string[];
  edgeCases: string[];
  constitutionSources: ConstitutionSource[];
  createdAt: string;
  updatedAt: string;
}

export interface FeatureTask {
  id: string;
  specId: string;
  storyId?: string | null;
  requirementIds: string[];
  successCriterionIds: string[];
  foundationPhase?: string | null;
  description: string;
  paths: string[];
  parallelizable: boolean;
  status: FeatureTaskStatus;
  evidenceIds: string[];
  createdAt: string;
  updatedAt: string;
}

export interface SpecFinding {
  id: string;
  severity: SpecFindingSeverity;
  kind: string;
  message: string;
  sourceKind: TraceKind;
  sourceId: string;
}

export interface SpecCoverage {
  requirementCount: number;
  requirementsWithTasks: number;
  successCriteriaCount: number;
  buildableCriteriaWithEvidence: number;
  taskCount: number;
  tasksWithMapping: number;
  independentlyTestableStories: number;
}

export interface SpecAnalysisRun {
  id: string;
  specId: string;
  status: SpecAnalysisStatus;
  findings: SpecFinding[];
  coverage: SpecCoverage;
  createdAt: string;
}

export interface TraceLink {
  id: string;
  specId: string;
  fromKind: TraceKind;
  fromId: string;
  toKind: TraceKind;
  toId: string;
  confidence: number;
  createdAt: string;
}

export interface SpecEvidence {
  id: string;
  sessionId: string;
  taskId?: string | null;
  evidenceId?: string | null;
  path: string;
  kind: string;
  contentHash: string;
  shippable: boolean;
  producer: string;
  provenance: string;
  verdictPassed?: boolean | null;
  verdictSummary?: string | null;
  createdAt: string;
}

export interface SpecTrace {
  specId: string;
  links: TraceLink[];
  evidence: SpecEvidence[];
  latestAnalysis?: SpecAnalysisRun | null;
}

export interface SessionSpecCompletion {
  sessionId: string;
  bound: boolean;
  specId?: string | null;
  specTitle?: string | null;
  status: SpecAnalysisStatus;
  canComplete: boolean;
  analysisId?: string | null;
  coverage?: SpecCoverage | null;
  blockerCount: number;
  warningCount: number;
  blockers: SpecFinding[];
  warnings: SpecFinding[];
}

export interface SpecClarification {
  id: string;
  sourceKind: TraceKind;
  sourceId: string;
  question: string;
  required: boolean;
}

export interface SpecClarifyResponse {
  specId: string;
  questions: SpecClarification[];
  blocking: boolean;
  nextAction: string;
}

export interface SpecPlanResponse {
  spec: FeatureSpec;
  tasks: FeatureTask[];
  createdTaskCount: number;
  skippedExistingCount: number;
  analysis: SpecAnalysisRun;
}

export interface SpecChecklistItem {
  id: string;
  label: string;
  status: "done" | "open" | "blocked" | "warn";
  sourceKind: TraceKind;
  sourceId: string;
  detail: string;
}

export interface SpecChecklistResponse {
  specId: string;
  focus?: string | null;
  ready: boolean;
  blockerCount: number;
  warningCount: number;
  items: SpecChecklistItem[];
  analysis: SpecAnalysisRun;
}

export interface SpecImplementResponse {
  specId: string;
  sessionId?: string | null;
  accepted: boolean;
  dryRun: boolean;
  prompt: string;
  analysis: SpecAnalysisRun;
}

export interface SpecEnvelope {
  spec: FeatureSpec;
  tasks: FeatureTask[];
}
