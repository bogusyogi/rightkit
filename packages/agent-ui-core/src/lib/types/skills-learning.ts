export interface SkillItem {
  slug: string;
  name: string;
  /** Optional compatibility metadata supplied by older skill inventory clients. */
  source?: string | null;
  description: string;
  trigger: string;
  enabled: boolean;
  lifecycle?: "proposed" | "quarantined" | "benchmarked" | "approved" | "active" | "retired";
  active?: boolean;
  version?: string | null;
  author?: string | null;
  compatibility?: string[];
  platforms?: string[];
  requiredEnvironmentVariables?: string[];
  metadata?: {
    coderight?: {
      category?: "tool" | "behavioral" | null;
      requiresToolsets?: string[];
      fallbackForToolsets?: string[];
      config?: unknown;
      gateMode?: "hard" | "soft" | "mixed" | null;
      benchmarkIds?: string[];
    };
  };
  category: "tool" | "behavioral";
  priority: number;
  disableModelInvocation?: boolean;
  tags: string[];
  pathAffinity: string[];
  effects?: {
    executionMode?: string;
    allowedTools?: string[];
    disallowedTools?: string[];
    prompt?: string;
  };
  path: string;
  directory: string;
  kind: string;
  valid: boolean;
  error?: string | null;
}

export interface SkillListResponse {
  skillDirs: string[];
  skills: SkillItem[];
}

export interface SkillReadResponse {
  skill: SkillItem;
  content: string;
}

export interface SkillSaveResponse {
  saved: boolean;
  skill: SkillItem;
  content: string;
}

export interface SkillProvenanceReviewRequest {
  sourceUrl: string;
  content: string;
}

export interface SkillProvenanceFinding {
  severity: "info" | "warning" | "error";
  code: string;
  message: string;
}

export interface SkillProvenanceReviewResponse {
  ok: boolean;
  sourceUrl: string;
  normalizedSourceUrl?: string | null;
  contentHash: string;
  manifestName?: string | null;
  manifestDescription?: string | null;
  recommendedLifecycle: string;
  findings: SkillProvenanceFinding[];
}

export type SkillProposalStatus = "proposed" | "quarantined" | "benchmarked" | "active" | "retired";

export interface SkillProposalRecord {
  id: string;
  slug: string;
  title: string;
  summary: string;
  proposedContent: string;
  contentHash: string;
  status: SkillProposalStatus;
  sourceSessionId?: string | null;
  sourceTaskId?: string | null;
  createdAtUnixSecs: number;
  updatedAtUnixSecs: number;
}

export interface SkillVersionRecord {
  id: string;
  slug: string;
  version: number;
  contentHash: string;
  parentHash?: string | null;
  proposalId?: string | null;
  changeReason: string;
  createdAtUnixSecs: number;
}

export interface SkillBenchmarkVerdict {
  id: string;
  slug: string;
  proposalId?: string | null;
  benchmarkId: string;
  gateMode: "hard" | "soft" | "mixed";
  scoreContinuous: number;
  passed: boolean;
  verdictHash: string;
  createdAtUnixSecs: number;
}

export interface SkillProposalBenchmarkRunRequest {
  kind?: "role" | "terminalBench" | "terminal-bench" | "tbench" | string;
  sessionId?: string;
  role?: string;
  model?: string;
  goal?: string;
  repeats?: number;
  taskId?: string;
  agent?: string;
  benchmarkId?: string;
  gateMode?: "hard" | "soft" | "mixed";
  threshold?: number;
  reason?: string;
}

export interface SkillProposalBenchmarkRunResponse {
  ran: boolean;
  gateMode: "hard" | "soft" | "mixed";
  threshold: number;
  scoreContinuous: number;
  runnerPassed: boolean;
  record: {
    recorded: boolean;
    proposal: SkillProposalRecord;
    benchmarkVerdict: SkillBenchmarkVerdict;
  };
}

export interface SkillGovernanceAuditEntry {
  id: string;
  targetType: string;
  targetId: string;
  slug?: string | null;
  action: string;
  reason?: string | null;
  actor: string;
  operatorOverride: boolean;
  timestampUnixSecs: number;
}

export interface SkillGovernanceResponse {
  storePath: string;
  proposals: SkillProposalRecord[];
  versions: SkillVersionRecord[];
  benchmarkVerdicts: SkillBenchmarkVerdict[];
  auditLog: SkillGovernanceAuditEntry[];
}

export interface LearningProposal {
  id: string;
  kind: string;
  title: string;
  status: string;
  confidence: number;
  body: string;
  path?: string;
}

export interface SkillBundle {
  id: string;
  name: string;
  description: string;
  skillBody: string;
  cycles: number;
  verificationScore: number;
  sourceProposalIds: string[];
  approved: boolean;
}

export interface SkillOptimizationTarget {
  skillId: string;
  objective: string;
  optimizer: string;
  targetModel?: string | null;
}

export interface RejectedEdit {
  artifactId: string;
  reason: string;
  patchPreview: string;
}

export type BenchmarkGateKind = "hard" | "soft" | "mixed";

export interface BenchmarkGate {
  name: string;
  kind: BenchmarkGateKind;
  threshold: number;
}

export interface SkillOptimizationCandidate {
  id: string;
  epoch: number;
  operation: string;
  summary: string;
  proposedBody: string;
  proposedHash: string;
  baselineScore: number;
  validationScore: number;
  accepted: boolean;
  rejectionReason?: string | null;
  createdAt: string;
}

export interface SkillOptimizationCandidateProposal {
  id: string;
  runId: string;
  epoch: number;
  operation: string;
  summary: string;
  proposedBody: string;
  proposedHash: string;
  source: string;
  metaSkillHash: string;
  createdAt: string;
}

export interface SkillOptimizationEpochInput {
  proposedBody: string;
  validationScore: number;
  operation?: string | null;
  summary?: string | null;
  gateMode?: BenchmarkGateKind | null;
  threshold?: number | null;
}

export interface SkillOptimizationProposalInput {
  operation?: string | null;
  summary?: string | null;
  instructions?: string | null;
}

export interface SkillOptimizationProposalResult {
  proposed: boolean;
  proposal: SkillOptimizationCandidateProposal;
  run: SkillOptimizationRun;
}

export interface SkillOptimizationEpochResult {
  executed: boolean;
  accepted: boolean;
  candidate: SkillOptimizationCandidate;
  run: SkillOptimizationRun;
}

export interface SkillOptMetaSkill {
  version: number;
  body: string;
  contentHash: string;
  refinedFromEpoch: number;
  acceptedPatternCount: number;
  rejectedPatternCount: number;
  guidance: string[];
  updatedAt: string;
}

export type SkillOptimizationStatus =
  | "pending"
  | "running"
  | "paused"
  | "accepted"
  | "rejected"
  | "cancelled";

export interface SkillOptimizationHistoryEntry {
  event: string;
  epoch: number;
  message: string;
  createdAt: string;
  checkpoint?: unknown;
}

export interface SkillOptimizationRun {
  id: string;
  name: string;
  target: SkillOptimizationTarget;
  rejectedEdits: RejectedEdit[];
  gates: BenchmarkGate[];
  status: SkillOptimizationStatus;
  currentEpoch: number;
  history: SkillOptimizationHistoryEntry[];
  candidates: SkillOptimizationCandidate[];
  pendingCandidateProposals: SkillOptimizationCandidateProposal[];
  resumeCheckpoint?: unknown;
  bestVersionHash?: string | null;
  createdAt: string;
  updatedAt: string;
  completedAt?: string | null;
  skillBundle: SkillBundle;
  metaSkill: SkillOptMetaSkill;
}

export interface HarnessRadarSource {
  id: string;
  kind: string;
  url: string;
  cadence: string;
  snapshotUpdatedAt?: string | null;
  minEntries?: number | null;
  requiredCapabilities: string[];
  requiredCategories: string[];
}

export interface HarnessRadarProposal {
  id: string;
  title: string;
  capability: string;
  sourceId: string;
  projectNames: string[];
  rationale: string;
}

export interface HarnessRadarEntry {
  sourceId: string;
  projectName: string;
  repoUrl: string;
  category: string;
  tags: string[];
  summary: string;
  updatedAt?: string | null;
  starsSnapshot?: number | null;
}

export interface HarnessRadarValidationIssue {
  severity: string;
  code: string;
  message: string;
}

export interface HarnessRadarValidation {
  sourceId: string;
  status: string;
  snapshotUpdatedAt?: string | null;
  entryCount: number;
  capabilityCount: number;
  missingCapabilities: string[];
  missingCategories: string[];
  issues: HarnessRadarValidationIssue[];
}

export interface HarnessRadarChange {
  sourceId: string;
  projectName: string;
  repoUrl: string;
  status: string;
  changedFields: string[];
  previousUpdatedAt?: string | null;
  currentUpdatedAt?: string | null;
}

export interface HarnessRadarSnapshot {
  sources: HarnessRadarSource[];
  entries: HarnessRadarEntry[];
  capabilities: string[];
  capabilityMap: Record<string, number>;
  proposals: HarnessRadarProposal[];
  changes: HarnessRadarChange[];
  validations: HarnessRadarValidation[];
  notes: string[];
}
