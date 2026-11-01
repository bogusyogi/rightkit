// D4 — Delivery, Environments, Attention: typed FE mirrors of the daemon
// contract fragments in `engine/bins/coderight/src/api_delivery.rs`,
// `api_environments.rs`, and `api_attention.rs`. This file is the single
// source of truth for these shapes on the frontend; consumers import from
// `lib/types` (re-exported by `lib/types.ts`).

// ─── Delivery ────────────────────────────────────────────────────────────────

export type DeliveryCapabilityKind =
  | "git"
  | "pullRequest"
  | "checks"
  | "failingLogs"
  | "backgroundCommand";

export type DeliveryHostKind = "local" | "worktree" | "ssh" | "wsl" | "configuredRemote";

export interface DeliveryCapability {
  kind: DeliveryCapabilityKind;
  available: boolean;
  reason?: string | null;
}

export interface DeliveryAuthorityStatus {
  trusted: boolean;
  source: string;
  workspaceRoot: string;
  trustLabel: string;
}

export interface DeliverySnapshot {
  workspaceRoot: string;
  hostKind: DeliveryHostKind;
  hostAvailable: boolean;
  hostUnavailableReason?: string | null;
  capabilities: DeliveryCapability[];
  authority: DeliveryAuthorityStatus;
  gitBinary?: string | null;
  ghBinary?: string | null;
  gitVersion?: string | null;
  ghVersion?: string | null;
  checkedAt: string;
}

export type CommandStatus =
  | "pending"
  | "running"
  | "exited"
  | "killed"
  | "spawnFailed"
  | "cancelled"
  | "authorityDenied";

export interface CommandRecord {
  commandId: string;
  command: string;
  args: string[];
  cwd: string;
  startedAt: string;
  finishedAt?: string | null;
  exitCode?: number | null;
  status: CommandStatus;
  stdoutTail: string;
  stderrTail: string;
  hostKind: DeliveryHostKind;
}

export interface CommandFailure {
  code: string;
  message: string;
}

// ─── Environments ────────────────────────────────────────────────────────────

export type EnvironmentKind = "local" | "worktree" | "ssh" | "wsl" | "configuredRemote";

export type EnvironmentSetupState =
  | "ready"
  | "pending"
  | "running"
  | "failed"
  | "cancelled"
  | "unsupported";

export type EnvironmentTrustLevel = "trusted" | "sandboxed" | "locked" | "untrusted";

export interface EnvironmentProviderStatus {
  provider: string;
  available: boolean;
  defaultModel?: string | null;
  reason?: string | null;
}

export interface EnvironmentRecord {
  id: string;
  label: string;
  kind: EnvironmentKind;
  workspaceRoot: string;
  branch?: string | null;
  trust: EnvironmentTrustLevel;
  setupState: EnvironmentSetupState;
  setupError?: string | null;
  lastLatencyMs?: number | null;
  lastTestedAt?: string | null;
  runtimeSupported: boolean;
  runtimeUnavailableReason?: string | null;
  providers: EnvironmentProviderStatus[];
}

export interface EnvironmentListResponse {
  selectedId?: string | null;
  environments: EnvironmentRecord[];
  generatedAt: string;
}

export interface EnvironmentTestResult {
  environmentId: string;
  reachable: boolean;
  latencyMs?: number | null;
  testedAt: string;
  error?: string | null;
  command: string;
  exitCode?: number | null;
  stdoutTail: string;
  stderrTail: string;
}

export interface SetupScriptStep {
  id: string;
  label: string;
  command: string;
  args: string[];
  workingDir?: string | null;
  timeoutMs?: number | null;
}

export interface SetupScript {
  id: string;
  version: number;
  label: string;
  environmentKind: EnvironmentKind;
  projectPolicy: string;
  steps: SetupScriptStep[];
}

export interface SetupScriptListResponse {
  workspaceRoot: string;
  scripts: SetupScript[];
}

export type SetupScriptRunState =
  | "pending"
  | "running"
  | "cancelled"
  | "completed"
  | "failed"
  | "timedOut"
  | "unsupported";

export interface SetupScriptRunLogLine {
  ts: string;
  stream: string;
  line: string;
}

export interface SetupScriptRun {
  runId: string;
  scriptId: string;
  scriptVersion: number;
  workspaceRoot: string;
  environmentKind: EnvironmentKind;
  state: SetupScriptRunState;
  startedAt: string;
  finishedAt?: string | null;
  exitCode?: number | null;
  logPath: string;
  logTail: SetupScriptRunLogLine[];
  cancellable: boolean;
}

export interface SetupScriptLogsResponse {
  run: SetupScriptRun;
}

// ─── Attention ───────────────────────────────────────────────────────────────

export type AttentionState =
  | "unread"
  | "working"
  | "needsInput"
  | "failed"
  | "complete"
  | "snoozed";

export type TranscriptDensity = "summary" | "normal" | "verbose";

export interface AttentionDeeplink {
  target: string;
  params: Record<string, unknown>;
}

export interface AttentionItem {
  id: string;
  kind: string;
  title: string;
  body: string;
  source: string;
  sessionId?: string | null;
  scheduleId?: string | null;
  createdAt: string;
  updatedAt: string;
  state: AttentionState;
  deeplink?: AttentionDeeplink | null;
  snoozedUntil?: string | null;
  read: boolean;
}

export interface AttentionCounts {
  unread: number;
  working: number;
  needsInput: number;
  failed: number;
  complete: number;
  snoozed: number;
}

export interface AttentionListResponse {
  workspaceRoot: string;
  density: TranscriptDensity;
  items: AttentionItem[];
  counts: AttentionCounts;
  quietHoursActive: boolean;
}

export interface AttentionBanner {
  bannerId: string;
  state: AttentionState;
  title: string;
  body: string;
  deeplink?: AttentionDeeplink | null;
  workspaceRoot: string;
  generatedAt: string;
}

export interface DensityResponse {
  workspaceRoot: string;
  density: TranscriptDensity;
  durableHistory: boolean;
}

export interface ProjectNotificationPolicy {
  workspaceRoot: string;
  enabled: boolean;
  quietProgressSpam: boolean;
  maxItemsPerSession: number;
  persistCompleteForHours: number;
}

export interface QuietHoursWindow {
  start: string;
  end: string;
  timezone?: string | null;
}

export interface QuietHoursResponse {
  workspaceRoot: string;
  enabled: boolean;
  windows: QuietHoursWindow[];
}

export interface DeeplinkResolution {
  resolved: boolean;
  target: string;
  path: string;
  params: Record<string, unknown>;
  note?: string | null;
}
