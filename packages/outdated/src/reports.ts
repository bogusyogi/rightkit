// Shapes of the files apply, gate and land exchange under `.rightkit/update/`.
// Loaders validate structure and path safety because land commits whatever these name.

import path from "node:path";
import type { Platform, Policy } from "./types.js";
import { readJson, sha256Bytes } from "./state.js";

export interface FileChange {
  /** Repository-relative, `/`-separated. */
  path: string;
  sha256Before: string | null;
  sha256After: string | null;
}

export interface ToolPlatformDigest {
  platform: Platform;
  asset: string;
  size: number;
  sha256: string;
  /** Always `observed`: computed from the download, never taken from a trusted upstream source. */
  trust: "observed";
  /** The manifest checksum this replaces. */
  previousChecksum: string;
}

export interface ToolStage {
  platform: Platform;
  /** Upstream tag or version text used in the download URL. */
  tag: string;
  /** New `pin` value. */
  version: string;
  asset: string;
  sourceUrl: string;
  /** Staged candidate directory, repository-relative. */
  candidateDir: string;
  /** Path of the runnable binary relative to `candidateDir`. */
  binary: string;
  size: number;
  sha256: string;
  trust: "observed";
  previousChecksum: string;
  /** Digest of the extracted member when the asset is an archive. */
  binarySha256?: string;
  /** Digests of the other platforms' assets, computed without storing them. */
  otherPlatforms: ToolPlatformDigest[];
}

export type ChangeKind = "cargo" | "pnpm" | "swiftpm" | "tool";

export interface ApplyChange {
  kind: ChangeKind;
  name: string;
  /** Manifest the dependency was declared in, or `tool:<name>`. */
  source: string;
  before: string;
  after: string;
  /** Upstream latest as reported by check. */
  latest: string;
  status: "applied" | "staged" | "planned";
  files: FileChange[];
  commands: string[][];
  notes: string[];
  tool?: ToolStage;
}

export interface Unapplied {
  kind: string;
  name: string;
  source: string;
  before: string | null;
  latest: string | null;
  reason: string;
  policy?: Policy;
}

export interface ApplyReport {
  schemaVersion: 1;
  tool: "right-update";
  version: string;
  repo: string;
  createdAt: string;
  dryRun: boolean;
  baseCommit: string | null;
  changes: ApplyChange[];
  needsMigration: Unapplied[];
  held: Unapplied[];
  skipped: Unapplied[];
  /** Rows whose edit or lock refresh failed; their files were restored. */
  failed: Unapplied[];
  /** Set by land once the changes are committed. */
  landed: { commit: string; at: string } | null;
}

export interface JourneyResult {
  name: string;
  ok: boolean;
  /** True when the journey could not decide (exit 3, or a missing or malformed report). */
  inconclusive: boolean;
  exitCode: number | null;
  timedOut: boolean;
  durationMs: number;
  outDir: string;
  reportPath: string;
  stdoutPath: string;
  stderrPath: string;
  detail?: string;
}

export interface GateResult {
  name: string;
  ok: boolean;
  exitCode: number | null;
  signal: string | null;
  timedOut: boolean;
  durationMs: number;
  stdoutPath: string;
  stderrPath: string;
  detail?: string;
}

export interface GateReport {
  schemaVersion: 1;
  repo: string;
  startedAt: string;
  finishedAt: string;
  /** sha256 of the apply-report file this gate ran against, or null when none existed. */
  applyReportSha256: string | null;
  passed: boolean;
  exitCode: 0 | 1 | 3;
  gates: GateResult[];
  journeys: JourneyResult[];
  warnings: string[];
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/** Safe repository-relative path: no absolute path, no parent traversal, no empty segments, nothing inside `.git`. */
export function isSafeRel(value: unknown): value is string {
  if (typeof value !== "string" || value.length === 0 || value.length > 1024) return false;
  if (path.isAbsolute(value) || /^[A-Za-z]:/.test(value) || value.startsWith("\\")) return false;
  const segments = value.split(/[\\/]/);
  if (segments[0].toLowerCase() === ".git") return false;
  return segments.every((seg) => seg.length > 0 && seg !== "." && seg !== "..");
}

const TOOL_NAME_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const TOOL_VERSION_RE = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$/;
const ASSET_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const DIGEST_RE = /^[0-9a-f]{64}$/;
const CANDIDATES_PREFIX = ".rightkit/update/candidates/";

export type Loaded<T> = { ok: true; value: T; sha256: string } | { ok: false; error: string };

export function loadApplyReport(file: string): Loaded<ApplyReport> {
  const read = readJson<unknown>(file);
  if (!read.ok) return read;
  const v = read.value;
  if (!isObject(v) || v.schemaVersion !== 1 || v.tool !== "right-update") return { ok: false, error: `${file} is not a right-update apply report` };
  if (!Array.isArray(v.changes) || !Array.isArray(v.needsMigration) || !Array.isArray(v.held) || !Array.isArray(v.skipped) || !Array.isArray(v.failed)) {
    return { ok: false, error: `${file} is missing report sections` };
  }
  for (const change of v.changes) {
    if (!isObject(change) || typeof change.kind !== "string" || typeof change.name !== "string" || !Array.isArray(change.files)) {
      return { ok: false, error: `${file} has a malformed change entry` };
    }
    for (const f of change.files) {
      if (!isObject(f) || !isSafeRel(f.path)) return { ok: false, error: `${file} names an unsafe path: ${String(isObject(f) ? f.path : f)}` };
    }
    if (change.kind === "tool") {
      const tool = change.tool;
      const wellFormed =
        isObject(tool) &&
        TOOL_NAME_RE.test(change.name as string) &&
        typeof tool.version === "string" && TOOL_VERSION_RE.test(tool.version) &&
        typeof tool.asset === "string" && ASSET_RE.test(tool.asset) &&
        typeof tool.sha256 === "string" && DIGEST_RE.test(tool.sha256) &&
        isSafeRel(tool.candidateDir) && (tool.candidateDir as string).startsWith(CANDIDATES_PREFIX) &&
        isSafeRel(tool.binary) &&
        Array.isArray(tool.otherPlatforms) &&
        tool.otherPlatforms.every((o) => isObject(o) && typeof o.sha256 === "string" && DIGEST_RE.test(o.sha256));
      if (!wellFormed) return { ok: false, error: `${file} has a malformed tool stage for ${String(change.name)}` };
    }
  }
  return { ok: true, value: v as unknown as ApplyReport, sha256: sha256Bytes(read.raw) };
}

export function loadGateReport(file: string): Loaded<GateReport> {
  const read = readJson<unknown>(file);
  if (!read.ok) return read;
  const v = read.value;
  if (!isObject(v) || v.schemaVersion !== 1 || typeof v.passed !== "boolean" || !Array.isArray(v.gates) || !Array.isArray(v.journeys)) {
    return { ok: false, error: `${file} is not a right-update gate report` };
  }
  return { ok: true, value: v as unknown as GateReport, sha256: sha256Bytes(read.raw) };
}
