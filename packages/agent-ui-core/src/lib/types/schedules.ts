export interface ScheduleRunEvent {
  schemaVersion?: number;
  scheduleId: string;
  epochMinute: number;
  status: string;
  sessionId: string;
  message: string;
  startedAt?: string;
  finishedAt?: string | null;
  mode?: "agent" | "scriptOnly" | string;
  toolset?: string | null;
  executionBackend?: string | null;
  resolvedExecutionBackend?: string | null;
  executionWorkspaceId?: string | null;
  executionWorkspacePath?: string | null;
  resourceLimits?: {
    cpu?: number | null;
    memoryMb?: number | null;
    diskMb?: number | null;
  } | null;
  workdir?: string | null;
  skills?: string[];
  deliveryKinds?: string[];
  sourceRecipeId?: string | null;
  exitCode?: number | null;
  artifactPath?: string | null;
  deliveryStatus?: string | null;
  deliveryErrors?: string[];
}

export interface ScheduleDeliveryTarget {
  kind: string;
  target?: string | null;
  allowPrivateNetwork?: boolean;
}

export interface ScheduleRow {
  id: string;
  cronExpr: string;
  prompt: string;
  timeZone?: string;
  skills?: string[];
  workdir?: string | null;
  toolset?: string | null;
  executionBackend?: string | null;
  delivery?: ScheduleDeliveryTarget[];
  scriptAllowlist?: string[];
  mode?: "agent" | "scriptOnly" | string;
  paused?: boolean;
  sourceRecipeId?: string | null;
  recipeParams?: Record<string, string>;
  maxRuntimeSeconds?: number | null;
  nextDueEpochMinute?: number | null;
  nextDueError?: string | null;
  lastClaimedMinute?: number | null;
  latestRun?: ScheduleRunEvent | null;
  runs?: ScheduleRunEvent[];
}

export interface ScheduleListResponse {
  workspaceRoot: string;
  scheduleStorePath: string;
  runStorePath: string;
  adminAuditStorePath?: string;
  schedules: ScheduleRow[];
}

export interface ScheduleAdminAuditEvent {
  id: string;
  scheduleId: string;
  action: string;
  actor: string;
  source: string;
  createdAt: string;
  status: string;
  details?: {
    cronExpr?: string;
    timeZone?: string;
    mode?: string;
    paused?: boolean;
    toolset?: string | null;
    executionBackend?: string | null;
    hasWorkdir?: boolean;
    skillCount?: number;
    deliveryKinds?: string[];
    scriptAllowlistCount?: number;
    sourceRecipeId?: string | null;
    maxRuntimeSeconds?: number | null;
    changedFields?: string[];
    manual?: boolean;
  } | null;
}

export interface ScheduleAdminAuditResponse {
  workspaceRoot: string;
  auditStorePath: string;
  history: ScheduleAdminAuditEvent[];
}

export interface ScheduleRunsResponse {
  id: string;
  workspaceRoot: string;
  runStorePath: string;
  latestRun?: ScheduleRunEvent | null;
  runs: ScheduleRunEvent[];
}

export interface ScheduleWriteRequest {
  id?: string;
  cronExpr?: string;
  prompt?: string;
  timeZone?: string;
  skills?: string[];
  workdir?: string | null;
  toolset?: string | null;
  executionBackend?: string | null;
  delivery?: ScheduleDeliveryTarget[];
  scriptAllowlist?: string[];
  mode?: "agent" | "scriptOnly" | string;
  paused?: boolean;
  maxRuntimeSeconds?: number | null;
}

export interface ScheduleWriteResponse {
  created?: boolean;
  updated?: boolean;
  id: string;
  scheduleStorePath: string;
  schedule: ScheduleRow;
}

export interface ScheduleRecipeRequest {
  id?: string;
  cronExpr: string;
  params?: Record<string, string>;
  workspaceRoot?: string;
  timeZone?: string;
  skills?: string[];
  workdir?: string | null;
  toolset?: string | null;
  executionBackend?: string | null;
  delivery?: ScheduleDeliveryTarget[];
  paused?: boolean;
}

export interface ScheduleRecipeResponse {
  created: boolean;
  id: string;
  recipeId: string;
  workspaceRoot: string;
  scheduleStorePath: string;
  schedule: ScheduleRow;
}
