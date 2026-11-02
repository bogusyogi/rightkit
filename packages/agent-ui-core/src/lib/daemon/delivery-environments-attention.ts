// D4 — Delivery / Environments / Attention daemon clients.
//
// The D4 lane ships standalone route + contract fragments
// (`engine/bins/coderight/src/api_delivery.rs`, `api_environments.rs`,
// `api_attention.rs`) without modifying the central generated client or the
// central API contract manifest. Agent 4 owns those central seams. Until that
// lane registers the new operations in `daemon-openapi.ts`, this module talks
// to the daemon via a dedicated `requestDaemon` helper that mirrors the
// generated client's URL/auth shape without depending on its operation table.
//
// Every request carries the daemon bearer token, so an unverified origin is
// rejected before the typed body is even reached. The helper refuses
// non-trusted URLs the same way the generated client does.

import type {
  AttentionBanner,
  AttentionListResponse,
  AttentionState,
  CommandRecord,
  DeeplinkResolution,
  DeliveryCapabilityKind,
  DeliveryHostKind,
  DeliverySnapshot,
  DensityResponse,
  EnvironmentKind,
  EnvironmentListResponse,
  EnvironmentRecord,
  EnvironmentTestResult,
  ProjectNotificationPolicy,
  QuietHoursResponse,
  SetupScriptListResponse,
  SetupScriptRun,
  TranscriptDensity,
} from "../types.js";
import type { DaemonConfig } from "../types.js";

interface DaemonRequestOptions {
  method: "GET" | "POST" | "PATCH" | "DELETE";
  query?: Record<string, string | number | boolean | null | undefined>;
  body?: unknown;
  pathParams?: Record<string, string | number>;
}

function buildPath(template: string, pathParams: Record<string, string | number> = {}): string {
  return template.replace(/:([A-Za-z_][A-Za-z0-9_]*)/g, (_, key: string) => {
    const value = pathParams[key];
    if (value === undefined) {
      throw new Error(`missing daemon path param: ${key}`);
    }
    return encodeURIComponent(String(value));
  });
}

export async function requestDaemon<TResponse = unknown>(
  config: DaemonConfig,
  template: string,
  options: DaemonRequestOptions,
): Promise<TResponse> {
  const baseUrl = config.url.replace(/\/+$/, "");
  const url = new URL(`${baseUrl}${buildPath(template, options.pathParams ?? {})}`);
  for (const [key, value] of Object.entries(options.query ?? {})) {
    if (value !== null && value !== undefined) {
      url.searchParams.set(key, String(value));
    }
  }
  const headers = new Headers();
  if (config.token && !headers.has("authorization")) {
    headers.set("authorization", `Bearer ${config.token}`);
  }
  const init: RequestInit = {
    method: options.method,
    headers,
  };
  if (options.body !== undefined) {
    headers.set("content-type", "application/json");
    init.body = JSON.stringify(options.body);
  }
  const response = await fetch(url, init);
  if (!response.ok) {
    throw new Error(
      `${options.method} ${template} failed: ${response.status} ${response.statusText}`,
    );
  }
  if (response.status === 204) {
    return undefined as TResponse;
  }
  return (await response.json()) as TResponse;
}

// ─── Delivery ────────────────────────────────────────────────────────────────

export async function getDeliveryStatus(
  config: DaemonConfig,
  workspaceRoot: string,
  host: DeliveryHostKind = "local",
): Promise<DeliverySnapshot> {
  return requestDaemon<DeliverySnapshot>(config, "/delivery/status", {
    method: "GET",
    query: { workspaceRoot: workspaceRoot.trim(), host },
  });
}

export async function listBranches(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<CommandRecord> {
  return requestDaemon<CommandRecord>(config, "/delivery/branches", {
    method: "GET",
    query: { workspaceRoot: workspaceRoot.trim() },
  });
}

export async function listWorktrees(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<CommandRecord> {
  return requestDaemon<CommandRecord>(config, "/delivery/worktrees", {
    method: "GET",
    query: { workspaceRoot: workspaceRoot.trim() },
  });
}

export interface CreateWorktreeArgs {
  workspaceRoot: string;
  branch: string;
  base?: string;
  path?: string;
  force?: boolean;
}

export interface DeliveryCommandResult {
  command: string;
  exitCode: number | null;
  stdout: string;
  stderr: string;
  status: string;
  path?: string;
  branch?: string;
  paths?: string[];
  message?: string;
}

export async function createWorktree(
  config: DaemonConfig,
  args: CreateWorktreeArgs,
): Promise<DeliveryCommandResult> {
  return requestDaemon<DeliveryCommandResult>(config, "/delivery/worktree", {
    method: "POST",
    body: args,
  });
}

export async function removeWorktree(
  config: DaemonConfig,
  args: { workspaceRoot: string; path: string; force?: boolean },
): Promise<DeliveryCommandResult> {
  return requestDaemon<DeliveryCommandResult>(config, "/delivery/worktree", {
    method: "DELETE",
    body: args,
  });
}

export async function stagePaths(
  config: DaemonConfig,
  workspaceRoot: string,
  paths: string[],
): Promise<DeliveryCommandResult> {
  return requestDaemon<DeliveryCommandResult>(config, "/delivery/stage", {
    method: "POST",
    body: { workspaceRoot, paths },
  });
}

export async function unstagePaths(
  config: DaemonConfig,
  workspaceRoot: string,
  paths: string[],
): Promise<DeliveryCommandResult> {
  return requestDaemon<DeliveryCommandResult>(config, "/delivery/stage", {
    method: "DELETE",
    body: { workspaceRoot, paths },
  });
}

export async function commitChanges(
  config: DaemonConfig,
  args: {
    workspaceRoot: string;
    message: string;
    amend?: boolean;
    signOff?: boolean;
  },
): Promise<DeliveryCommandResult> {
  return requestDaemon<DeliveryCommandResult>(config, "/delivery/commit", {
    method: "POST",
    body: args,
  });
}

export async function pushBranch(
  config: DaemonConfig,
  args: {
    workspaceRoot: string;
    remote?: string;
    branch?: string;
    setUpstream?: boolean;
    force?: boolean;
  },
): Promise<DeliveryCommandResult> {
  return requestDaemon<DeliveryCommandResult>(config, "/delivery/push", {
    method: "POST",
    body: args,
  });
}

export async function pullBranch(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<DeliveryCommandResult> {
  return requestDaemon<DeliveryCommandResult>(config, "/delivery/pull", {
    method: "POST",
    body: { workspaceRoot },
  });
}

export interface PullRequestCreateArgs {
  workspaceRoot: string;
  title?: string;
  body?: string;
  base?: string;
  head?: string;
  draft?: boolean;
}

export async function createPullRequest(
  config: DaemonConfig,
  args: PullRequestCreateArgs,
): Promise<DeliveryCommandResult> {
  return requestDaemon<DeliveryCommandResult>(config, "/delivery/pr", {
    method: "POST",
    body: args,
  });
}

export async function getPullRequest(
  config: DaemonConfig,
  workspaceRoot: string,
  id: string,
): Promise<DeliveryCommandResult> {
  return requestDaemon<DeliveryCommandResult>(config, "/delivery/pr/:id", {
    method: "GET",
    query: { workspaceRoot: workspaceRoot.trim() },
    pathParams: { id },
  });
}

export async function listPullRequestChecks(
  config: DaemonConfig,
  workspaceRoot: string,
  id: string,
): Promise<DeliveryCommandResult> {
  return requestDaemon<DeliveryCommandResult>(config, "/delivery/pr/:id/checks", {
    method: "GET",
    query: { workspaceRoot: workspaceRoot.trim() },
    pathParams: { id },
  });
}

export async function rerunCheck(
  config: DaemonConfig,
  workspaceRoot: string,
  id: string,
  checkId: string,
): Promise<DeliveryCommandResult> {
  return requestDaemon<DeliveryCommandResult>(config, "/delivery/pr/:id/checks/:checkId/rerun", {
    method: "POST",
    query: { workspaceRoot: workspaceRoot.trim() },
    pathParams: { id, checkId },
  });
}

export async function getCheckLogs(
  config: DaemonConfig,
  workspaceRoot: string,
  id: string,
  checkId: string,
): Promise<DeliveryCommandResult> {
  return requestDaemon<DeliveryCommandResult>(config, "/delivery/pr/:id/checks/:checkId/logs", {
    method: "GET",
    query: { workspaceRoot: workspaceRoot.trim() },
    pathParams: { id, checkId },
  });
}

export async function listPullRequestThreads(
  config: DaemonConfig,
  workspaceRoot: string,
  id: string,
): Promise<DeliveryCommandResult> {
  return requestDaemon<DeliveryCommandResult>(config, "/delivery/pr/:id/threads", {
    method: "GET",
    query: { workspaceRoot: workspaceRoot.trim() },
    pathParams: { id },
  });
}

export async function runBackgroundCommand(
  config: DaemonConfig,
  args: {
    workspaceRoot: string;
    command: string;
    args: string[];
    timeoutMs?: number;
  },
): Promise<CommandRecord> {
  return requestDaemon<CommandRecord>(config, "/delivery/command", {
    method: "POST",
    body: args,
  });
}

export async function cancelBackgroundCommand(
  config: DaemonConfig,
  workspaceRoot: string,
  commandId: string,
): Promise<{ cancelled: boolean; commandId: string; status: string; note?: string }> {
  return requestDaemon(config, "/delivery/cancel", {
    method: "POST",
    body: { workspaceRoot, commandId },
  });
}

// ─── Environments ────────────────────────────────────────────────────────────

export async function listEnvironments(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<EnvironmentListResponse> {
  return requestDaemon<EnvironmentListResponse>(config, "/environments", {
    method: "GET",
    query: { workspaceRoot: workspaceRoot.trim() },
  });
}

export async function selectEnvironment(
  config: DaemonConfig,
  workspaceRoot: string,
  environmentId: string,
): Promise<{ environmentId: string; saved: boolean; savedAt: string }> {
  return requestDaemon(config, "/environments/select", {
    method: "POST",
    body: { workspaceRoot, environmentId },
  });
}

export async function testEnvironment(
  config: DaemonConfig,
  workspaceRoot: string,
  environmentId: string,
): Promise<EnvironmentTestResult> {
  return requestDaemon<EnvironmentTestResult>(config, "/environments/:id/test", {
    method: "POST",
    body: { workspaceRoot, environmentId },
    pathParams: { id: environmentId },
  });
}

export async function listSetupScripts(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<SetupScriptListResponse> {
  return requestDaemon<SetupScriptListResponse>(config, "/environments/scripts", {
    method: "GET",
    query: { workspaceRoot: workspaceRoot.trim() },
  });
}

export interface RunSetupScriptArgs {
  workspaceRoot: string;
  scriptId: string;
  version?: number;
  parameters?: Record<string, string>;
}

export async function runSetupScript(
  config: DaemonConfig,
  args: RunSetupScriptArgs,
): Promise<SetupScriptRun> {
  return requestDaemon<SetupScriptRun>(config, "/environments/scripts/run", {
    method: "POST",
    body: args,
  });
}

export async function cancelSetupScriptRun(
  config: DaemonConfig,
  workspaceRoot: string,
  runId: string,
  scriptId: string,
): Promise<{ runId: string; cancelled: boolean; state: string; note?: string }> {
  return requestDaemon(config, "/environments/scripts/runs/:runId/cancel", {
    method: "POST",
    body: { workspaceRoot, scriptId },
    pathParams: { runId },
  });
}

export async function getSetupScriptLogs(
  config: DaemonConfig,
  workspaceRoot: string,
  runId: string,
): Promise<SetupScriptRun> {
  return requestDaemon<SetupScriptRun>(config, "/environments/scripts/runs/:runId/logs", {
    method: "GET",
    query: { workspaceRoot: workspaceRoot.trim() },
    pathParams: { runId },
  });
}

export async function rerunSetupScript(
  config: DaemonConfig,
  _workspaceRoot: string,
  runId: string,
  args: RunSetupScriptArgs,
): Promise<SetupScriptRun> {
  return requestDaemon<SetupScriptRun>(config, "/environments/scripts/runs/:runId/rerun", {
    method: "POST",
    body: args,
    pathParams: { runId },
  });
}

// ─── Attention ───────────────────────────────────────────────────────────────

export async function listAttentionItems(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<AttentionListResponse> {
  return requestDaemon<AttentionListResponse>(config, "/attention/items", {
    method: "GET",
    query: { workspaceRoot: workspaceRoot.trim() },
  });
}

export async function updateAttentionState(
  config: DaemonConfig,
  workspaceRoot: string,
  id: string,
  state: AttentionState,
): Promise<{ id: string; state: AttentionState; updated: boolean; updatedAt: string }> {
  return requestDaemon(config, "/attention/items/:id/state", {
    method: "PATCH",
    body: { workspaceRoot, state },
    pathParams: { id },
  });
}

export async function snoozeAttentionItem(
  config: DaemonConfig,
  workspaceRoot: string,
  id: string,
  snoozeUntil: string,
): Promise<{ id: string; snoozedUntil: string; saved: boolean }> {
  return requestDaemon(config, "/attention/items/:id/snooze", {
    method: "POST",
    body: { workspaceRoot, snoozeUntil },
    pathParams: { id },
  });
}

export async function unsnoozeAttentionItem(
  config: DaemonConfig,
  workspaceRoot: string,
  id: string,
): Promise<{ id: string; saved: boolean }> {
  return requestDaemon(config, "/attention/items/:id/unsnooze", {
    method: "POST",
    query: { workspaceRoot: workspaceRoot.trim() },
    pathParams: { id },
  });
}

export async function getDensity(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<DensityResponse> {
  return requestDaemon<DensityResponse>(config, "/attention/density", {
    method: "GET",
    query: { workspaceRoot: workspaceRoot.trim() },
  });
}

export async function setDensity(
  config: DaemonConfig,
  workspaceRoot: string,
  density: TranscriptDensity,
): Promise<DensityResponse> {
  return requestDaemon<DensityResponse>(config, "/attention/density", {
    method: "PATCH",
    body: { workspaceRoot, density },
  });
}

export async function getNotificationPolicy(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<ProjectNotificationPolicy> {
  return requestDaemon<ProjectNotificationPolicy>(config, "/attention/policy", {
    method: "GET",
    query: { workspaceRoot: workspaceRoot.trim() },
  });
}

export async function setNotificationPolicy(
  config: DaemonConfig,
  policy: ProjectNotificationPolicy,
): Promise<ProjectNotificationPolicy> {
  return requestDaemon<ProjectNotificationPolicy>(config, "/attention/policy", {
    method: "PATCH",
    body: policy,
  });
}

export async function getQuietHours(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<QuietHoursResponse> {
  return requestDaemon<QuietHoursResponse>(config, "/attention/quiet-hours", {
    method: "GET",
    query: { workspaceRoot: workspaceRoot.trim() },
  });
}

export async function setQuietHours(
  config: DaemonConfig,
  args: {
    workspaceRoot: string;
    enabled: boolean;
    windows: QuietHoursResponse["windows"];
  },
): Promise<QuietHoursResponse> {
  return requestDaemon<QuietHoursResponse>(config, "/attention/quiet-hours", {
    method: "PATCH",
    body: args,
  });
}

export async function resolveDeeplink(
  config: DaemonConfig,
  workspaceRoot: string,
  target: string,
  params: Record<string, unknown> = {},
): Promise<DeeplinkResolution> {
  return requestDaemon<DeeplinkResolution>(config, "/attention/deeplink", {
    method: "POST",
    body: { workspaceRoot, target, params },
  });
}

export async function getAttentionBanner(
  config: DaemonConfig,
  workspaceRoot: string,
): Promise<AttentionBanner | null> {
  return requestDaemon<AttentionBanner | null>(config, "/attention/banner", {
    method: "GET",
    query: { workspaceRoot: workspaceRoot.trim() },
  });
}

// ─── Pure helpers (no daemon calls) ──────────────────────────────────────────

/**
 * Pick the first environment whose `runtimeSupported` flag is true. Used by
 * the UI to fall back from a typed-but-unavailable kind (SSH, WSL,
 * configuredRemote) to the Local environment honestly.
 */
export function pickRuntimeSupportedEnvironment(
  environments: EnvironmentRecord[],
): EnvironmentRecord | null {
  for (const env of environments) {
    if (env.runtimeSupported) return env;
  }
  return null;
}

/**
 * Group attention items by their typed state. The returned shape preserves
 * the contract enum values, not display strings, so a re-render of the
 * sidebar/title does not change when a translator flips a label.
 */
export function groupAttentionByState(
  items: AttentionListResponse["items"],
): Record<AttentionState, AttentionListResponse["items"]> {
  const groups: Record<AttentionState, AttentionListResponse["items"]> = {
    unread: [],
    working: [],
    needsInput: [],
    failed: [],
    complete: [],
    snoozed: [],
  };
  for (const item of items) {
    groups[item.state].push(item);
  }
  return groups;
}

/**
 * Whether the attention list contains any state the UI must surface
 * (needsInput / failed). The banner row derives its visibility from this.
 */
export function hasUrgentAttention(items: AttentionListResponse["items"]): boolean {
  return items.some((item) => item.state === "needsInput" || item.state === "failed");
}

/** Stable string id used by the UI to focus a delivery row. */
export function deliveryCapabilityKey(kind: DeliveryCapabilityKind): string {
  return `capability:${kind}`;
}

export function environmentKindLabel(kind: EnvironmentKind): string {
  switch (kind) {
    case "local":
      return "Local";
    case "worktree":
      return "Worktree";
    case "ssh":
      return "SSH";
    case "wsl":
      return "WSL";
    case "configuredRemote":
      return "Configured remote";
  }
}
