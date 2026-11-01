export interface BenchmarkReport {
  id: string;
  role: string;
  model: string;
  suite: string;
  repeats: number;
  successes: number;
  successRate: number;
  taskId?: string | null;
  passed?: boolean | null;
  agentOutput?: string | null;
  verifyOutput?: string | null;
  verifyError?: string | null;
  createdAt: string;
}

export interface BenchmarkRunRequest {
  role: string;
  model?: string;
  goal: string;
  repeats: number;
}

export interface BenchmarkRunResult {
  role: string;
  model: string;
  repeats: number;
  successes: number;
  successRate: number;
}

export interface TerminalBenchTask {
  id: string;
  category: string;
  difficulty: string;
  prompt: string;
  setupFiles: { name: string; content: string }[];
  verifyCommand: string;
  expectedPassRate: number;
}

export interface TerminalBenchRunResult extends BenchmarkRunResult {
  benchmarkSuite: string;
  taskId: string;
  passed: boolean;
  agentOutput: string;
  verifyOutput: string;
  verifyError: string;
}

export interface RoleMetric {
  role: string;
  runs: number;
  successes: number;
  successRate: number;
}

// Per-role taste score from the evaluation engine (GET /runtime/taste).
export interface TasteScore {
  role: string;
  samples: number;
  overall: number;
  gds: number;
  longHorizonReliability: number;
  reliabilityDecaySlope: number;
  quality: number;
  reliability: number;
  consistency: number;
  robustness: number;
  predictability: number;
  safety: number;
  contextEfficiency: number;
  vaf: number;
  mopRate: number;
}

// Taste-engine improvement proposal (GET /runtime/proposals).
export interface TasteProposal {
  role: string;
  kind: string;
  rationale: string;
  confidence: number;
}

// Active staged model rollout (GET /runtime/rollouts).
export interface RolloutView {
  role: string;
  fromModel: string;
  toModel: string;
  stage: string;
  baselineRate: number;
}

// Fine-tuning readiness per (role, model) (GET /runtime/finetuning).
export interface FinetuningRow {
  role: string;
  model: string;
  qualifyingRecords: number;
  threshold: number;
  ready: boolean;
}

export interface AuditEntry {
  seq: number;
  createdAt: string;
  kind: string;
  subject: string;
  summary: string;
  prevHash: string;
  hash: string;
}

export interface AuditLogView {
  verified: boolean;
  entries: AuditEntry[];
}

// ── Protocol types (engine/crates/protocol) ────────────────────────────────

export interface ToolCall {
  id: string;
  name: string;
  argumentsJson: string;
}

export interface OutputRef {
  id: string;
  kind: string;
  sizeBytes: number;
  contentHash?: string;
  preview?: string;
  mediaType: string;
  truncated: boolean;
  retrievable: boolean;
}

export interface ArtifactRef {
  id: string;
  kind: string;
  title?: string;
  contentHash?: string;
}

export interface ArtifactVerdict {
  passed: boolean;
  juror: string;
  boundHash: string;
  summary: string;
}

export interface ArtifactApproval {
  approver: string;
  boundHash: string;
  createdAt: string;
}

export interface ArtifactRow {
  id: string;
  path: string;
  kind: string;
  contentHash: string;
  shippable: boolean;
  producer: string;
  provenance: string;
  verdict?: ArtifactVerdict;
  approvals: ArtifactApproval[];
  meta: string;
  createdAt: string;
}
