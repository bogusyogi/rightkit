// I1 — Run-scoped changes daemon client.
//
// Talks to the `/run-changes/*` daemon surface that `api_run_changes.rs`
// exposes. The integration lead wires those routes into
// `api_router.rs`; the frontend client mirrors the schema fragment in
// `engine/bins/coderight/src/api_contract/run_changes_components.rs`
// and is validated by the protocol-mirror check on test-shake-out.

import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types/settings-plugins";

export type ChangeClassification =
  | "tracked"
  | "untracked"
  | "renamed"
  | "deleted"
  | "binary"
  | "generated"
  | "secret"
  | "too_large"
  | "conflict"
  | "complete"
  | "partial";

export type ChangeStage = "unstaged" | "staged" | "reverted";
export type AnchorStatus = "live" | "stale" | "vanished";
export type CommentKind = "inline" | "revision" | "suggestion" | "question" | "resolution";

export interface ChangeEntry {
  mutationId: string;
  workspaceRoot: string;
  path: string;
  originalPath: string | null;
  classification: ChangeClassification;
  stage: ChangeStage;
  additions: number;
  deletions: number;
  diff: string;
  truncated: boolean;
  anchorStatus: AnchorStatus;
  anchorOffset: number;
  capturedAt: string;
  fingerprint: string;
  sizeBytes: number;
  modifiedAt: string | null;
  undo: UndoBody | null;
  lastError: string | null;
}

export interface UndoBody {
  fingerprint: string;
  body: string;
  note: string;
}

export interface RunChangesInventory {
  runId: string;
  workspaceRoot: string;
  entries: ChangeEntry[];
  truncated: boolean;
  git: GitBaselineSummary;
  lastCaptureAt: string;
}

export interface RunChangesSummary {
  runId: string;
  workspaceRoot: string;
  entryCount: number;
  truncated: boolean;
  countsByClassification: Record<string, number>;
  firstPaths: string[];
  lastCaptureAt: string;
  git: GitBaselineSummary;
}

export interface GitBaselineSummary {
  present: boolean;
  headSha: string | null;
  branch: string | null;
  indexFingerprint: string | null;
  untrackedCount: number;
}

export interface CaptureSnapshotRequest {
  runId: string;
  workspaceRoot: string;
  entries: ChangeEntry[];
  expectedIndexFingerprint?: string | null;
}

export interface CaptureSnapshotResponse {
  runId: string;
  stored: number;
  truncated: boolean;
  git: GitBaselineSummary;
}

export interface InventoryRequest {
  runId: string;
  workspaceRoot: string;
  path?: string | null;
  maxDiffBytes?: number | null;
}

export interface UndoRequest {
  runId: string;
  workspaceRoot: string;
  mutationId: string;
  observedFingerprint: string;
}

export interface UndoResponse {
  mutationId: string;
  path: string;
  applied: boolean;
  newFingerprint: string;
  conflictReason: string | null;
}

export interface RevisionUpdateRequest {
  runId: string;
  workspaceRoot: string;
  mutationId: string;
  path: string;
  anchorOffset: number;
  kind: CommentKind;
  body: string;
  author: string;
}

export interface RevisionUpdateResponse {
  commentId: string;
  anchorStatus: AnchorStatus;
  stale: boolean;
}

export interface DiffBodyRequest {
  runId: string;
  workspaceRoot: string;
  path: string;
  split: boolean;
  cursor?: string | null;
  window?: number | null;
  search?: string | null;
}

export interface DiffRow {
  kind: "file" | "hunk" | "add" | "del" | "ctx";
  text: string;
  oldLine: number | null;
  newLine: number | null;
}

export interface HunkDecision {
  hunkId: string;
  filePath: string;
  hunkIndex: number;
  anchorOffset: number;
  decision: "approve" | "reject" | "pending";
}

export interface DiffBodyResponse {
  path: string;
  rows: DiffRow[];
  truncated: boolean;
  nextCursor: string | null;
  hunkDecisions: HunkDecision[];
  search: { matches: number; cursor: string | null; term: string } | null;
}

export interface RunChangesRequestError extends Error {
  status: number;
  reason: string;
}

function buildError(status: number, reason: string): RunChangesRequestError {
  const err = new Error(
    `run_changes request failed (${status}): ${reason}`,
  ) as RunChangesRequestError;
  err.status = status;
  err.reason = reason;
  return err;
}

function camelize(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(camelize);
  if (!value || typeof value !== "object") return value;
  return Object.fromEntries(
    Object.entries(value).map(([key, nested]) => [
      key.replace(/_([a-z])/g, (_, letter: string) => letter.toUpperCase()),
      camelize(nested),
    ]),
  );
}

async function runRequest<T>(
  config: DaemonConfig,
  method: "GET" | "POST",
  path: string,
  options: {
    body?: unknown;
    query?: Record<string, string | number | boolean | null | undefined>;
    signal?: AbortSignal;
  } = {},
): Promise<T> {
  const url = new URL(`${config.url.replace(/\/+$/, "")}${path}`);
  if (options.query) {
    for (const [key, value] of Object.entries(options.query)) {
      if (value === undefined || value === null) continue;
      url.searchParams.set(key, String(value));
    }
  }
  const headers = new Headers();
  if (config.token) headers.set("authorization", `Bearer ${config.token}`);
  let body: BodyInit | undefined;
  if (options.body !== undefined) {
    headers.set("content-type", "application/json");
    body = JSON.stringify(options.body);
  }
  const response = await fetch(url, {
    method,
    headers,
    body,
    signal: options.signal,
  });
  if (!response.ok) {
    const text = await response.text().catch(() => "");
    throw buildError(response.status, text || response.statusText);
  }
  if (response.status === 204) return undefined as unknown as T;
  return (await response.json()) as T;
}

export async function captureRunChanges(
  config: DaemonConfig,
  request: CaptureSnapshotRequest,
  options: { signal?: AbortSignal } = {},
): Promise<CaptureSnapshotResponse> {
  return runRequest<CaptureSnapshotResponse>(config, "POST", "/run-changes/capture", {
    body: request,
    signal: options.signal,
  });
}

export async function getRunChangesInventory(
  config: DaemonConfig,
  request: InventoryRequest,
  options: { signal?: AbortSignal } = {},
): Promise<RunChangesInventory> {
  const url = new URL(`${config.url.replace(/\/+$/, "")}/run-changes/inventory`);
  url.searchParams.set("run_id", request.runId);
  url.searchParams.set("workspace_root", request.workspaceRoot);
  if (request.path !== undefined && request.path !== null) {
    url.searchParams.set("path", request.path);
  }
  if (request.maxDiffBytes !== undefined && request.maxDiffBytes !== null) {
    url.searchParams.set("max_diff_bytes", String(request.maxDiffBytes));
  }
  return camelize(
    await runRequest<RunChangesInventory>(config, "GET", `${url.pathname}${url.search}`, {
      signal: options.signal,
    }),
  ) as RunChangesInventory;
}

export async function getRunChangesSummary(
  config: DaemonConfig,
  request: InventoryRequest,
  options: { signal?: AbortSignal } = {},
): Promise<RunChangesSummary> {
  return camelize(
    await runRequest<RunChangesSummary>(config, "GET", "/run-changes/summary", {
      query: { run_id: request.runId, workspace_root: request.workspaceRoot },
      signal: options.signal,
    }),
  ) as RunChangesSummary;
}

export async function undoRunChange(
  config: DaemonConfig,
  request: UndoRequest,
  options: { signal?: AbortSignal } = {},
): Promise<UndoResponse> {
  return runRequest<UndoResponse>(config, "POST", "/run-changes/undo", {
    body: request,
    signal: options.signal,
  });
}

export async function recordRevisionComment(
  config: DaemonConfig,
  request: RevisionUpdateRequest,
  options: { signal?: AbortSignal } = {},
): Promise<RevisionUpdateResponse> {
  return runRequest<RevisionUpdateResponse>(config, "POST", "/run-changes/revise", {
    body: request,
    signal: options.signal,
  });
}

export async function getDiffBody(
  config: DaemonConfig,
  request: DiffBodyRequest,
  options: { signal?: AbortSignal } = {},
): Promise<DiffBodyResponse> {
  return runRequest<DiffBodyResponse>(config, "POST", "/run-changes/diff-body", {
    body: request,
    signal: options.signal,
  });
}
