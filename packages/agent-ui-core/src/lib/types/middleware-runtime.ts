export type MiddlewarePhase =
  | "sessionStart"
  | "preLlmCall"
  | "postLlmCall"
  | "preToolCall"
  | "postToolCall"
  | "approvalRequested"
  | "subagentStart"
  | "subagentStop"
  | "scheduledJobStart"
  | "scheduledJobEnd"
  | "sessionEnd";

export type MiddlewareSource = "builtIn" | "localPlugin";
export type MiddlewareEffect = "observe" | "mutateArgs" | "requireApproval" | "deny";

export interface MiddlewareDefinition {
  id: string;
  title: string;
  source: MiddlewareSource;
  phase: MiddlewarePhase;
  effect: MiddlewareEffect;
  tool?: string;
  requiresToolset?: string;
  condition?: {
    argument?: string;
    contains?: string;
    equals?: unknown;
  };
  mutateArgs?: Record<string, unknown>;
  reason?: string;
  enabled: boolean;
  order: number;
  description: string;
}

export interface MiddlewareDecisionEvidence {
  middlewareId: string;
  title: string;
  phase: MiddlewarePhase | string;
  effect: MiddlewareEffect | string;
  decision: string;
  reason: string;
}

export interface RuntimeSpan {
  id: string;
  parentId?: string;
  sessionId: string;
  turnId?: string;
  phase: MiddlewarePhase;
  status: string;
  startedAt: string;
  endedAt?: string;
  attributes: Record<string, unknown>;
}

export interface RuntimeObservation {
  id: string;
  spanId: string;
  sessionId: string;
  severity: string;
  message: string;
  payload: Record<string, unknown>;
  createdAt: string;
}

export interface MiddlewareRegistrySnapshot {
  middleware: MiddlewareDefinition[];
  spans: RuntimeSpan[];
  observations: RuntimeObservation[];
}

export interface ToolInvocationRecord {
  id: string;
  sessionId: string;
  turnId: string;
  toolName: string;
  schemaVersion: string;
  argsHash: string;
  argsRedactedJson: unknown;
  lifecycleState: string;
  policyVerdictId?: string | null;
  approvalId?: string | null;
  processId?: string | null;
  startedAt?: string | null;
  completedAt?: string | null;
  outputHash?: string | null;
  artifactIds: string[];
  errorClass?: string | null;
}

export interface ToolInvocationEventRow {
  invocationId: string;
  sessionId: string;
  seq: number;
  kind: string;
  createdAt: string;
  payloadRedactedJson: unknown;
}

export interface ToolInvocationCancelResult {
  cancelled: boolean;
  toolInvocationId: string;
  sessionId: string;
  reason?: string | null;
}

export interface InstructionSourceRow {
  scope: "managed" | "user" | "project" | "local" | string;
  path: string;
  bodyBytes: number;
  bodyLines: number;
  globs: string[];
  paths: string[];
  includes: string[];
  hookRefs: string[];
}

export interface InstructionStatus {
  workspaceRoot: string;
  fileCount: number;
  composedBytes: number;
  files: InstructionSourceRow[];
}
