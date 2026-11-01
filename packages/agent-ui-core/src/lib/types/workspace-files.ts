// D1 — Live workspace files/artifacts type mirrors.
//
// These types mirror the OpenAPI shapes published by the standalone
// `api_workspace` module on the daemon side. They live under
// `lib/types/workspace-files.ts` so the protocol-mirror checker (`check:protocol-mirror`)
// can validate that the frontend and backend agree without touching the
// generated client (the generated client only knows about routes that have
// been wired into the central OpenAPI document — Agent 4 owns that wiring).

export type WorkspaceEntryKind = "directory" | "file";

export interface WorkspaceEntry {
  name: string;
  /** Workspace-relative path with forward slashes. */
  path: string;
  kind: WorkspaceEntryKind;
  suffix?: string | null;
  sizeBytes?: number | null;
  /** RFC3339-like ISO timestamp, e.g. "2024-01-01T00:00:00Z". */
  modifiedAt?: string | null;
  gitStatus?: string | null;
  childCount?: number | null;
  truncated?: boolean | null;
  lastKnown?: boolean | null;
}

export interface WorkspaceTreeResponse {
  workspaceRoot: string;
  path: string;
  entries: WorkspaceEntry[];
  truncated: boolean;
  lastKnown: boolean;
}

export type WorkspaceFileKind = "file" | "missing" | "binary" | "directory" | "tooLarge";

export interface WorkspaceFileResponse {
  workspaceRoot: string;
  path: string;
  kind: WorkspaceFileKind;
  sizeBytes: number;
  modifiedAt: string | null;
  /** SHA-256 hex of the file content at read time; null when kind != file/binary. */
  fingerprint: string | null;
  content: string | null;
  /** POSIX mode bits in octal, e.g. "0644". */
  permission: string;
  truncated: boolean;
  reason: string | null;
}

export interface WorkspaceFileSaveRequest {
  workspaceRoot: string;
  path: string;
  content: string;
  /** SHA-256 the client observed; the server returns 409 on mismatch. */
  expectedFingerprint: string;
  baseRevision?: string | null;
}

export interface WorkspaceFileSaveResponse {
  ok: boolean;
  path: string;
  fingerprint: string;
  sizeBytes: number;
  modifiedAt: string;
}

export type WorkspaceSearchKind = "file" | "content" | "all";

export interface WorkspaceSearchRequest {
  workspaceRoot: string;
  query: string;
  /** Cancellation token — reuse to cancel an in-flight search. */
  requestId?: string | null;
  kind?: WorkspaceSearchKind | null;
  limit?: number | null;
  includeBinary?: boolean | null;
}

export interface WorkspaceSearchHit {
  path: string;
  name: string;
  line: number;
  column: number;
  snippet: string;
  kind: "file" | "content";
}

export interface WorkspaceSearchResponse {
  workspaceRoot: string;
  query: string;
  requestId?: string | null;
  hits: WorkspaceSearchHit[];
  cancelled: boolean;
  scannedBytes: number;
  truncated: boolean;
}

export interface WorkspaceArtifact {
  id: string;
  kind: string;
  path: string;
  title?: string | null;
  provenance?: string | null;
  verdict?: string | null;
  fingerprint: string;
  sizeBytes?: number | null;
  createdAt?: string | null;
}

export interface WorkspaceArtifactsResponse {
  workspaceRoot: string;
  artifacts: WorkspaceArtifact[];
}

export interface WorkspaceRecentEntry {
  path: string;
  name: string;
  openedAt: string;
}

export interface WorkspaceRecentResponse {
  workspaceRoot: string;
  recents: WorkspaceRecentEntry[];
}

export type WorkspaceWatchKind = "created" | "modified" | "removed" | "rescan";

export interface WorkspaceWatchEvent {
  kind: WorkspaceWatchKind;
  path: string;
  at: string;
  fingerprint: string | null;
}

export type WorkspaceFileSaveOutcome =
  | { kind: "saved"; response: WorkspaceFileSaveResponse }
  | { kind: "stale" }
  | { kind: "forbidden"; reason: string }
  | { kind: "missing" }
  | { kind: "unsupported"; reason: string };

export interface WorkspaceFileReadOutcome {
  kind: WorkspaceFileKind;
  response: WorkspaceFileResponse;
}

export type WorkspaceLoadStatus =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "ready" }
  | { kind: "unavailable"; reason: string }
  | { kind: "error"; reason: string };
