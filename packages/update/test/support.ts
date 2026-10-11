import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import type { FetchLike } from "../src/http.js";

export const SHA = `sha256:${"a".repeat(64)}`;

export function mkRepo(files: Record<string, string | object>): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "right-update-"));
  for (const [rel, content] of Object.entries(files)) {
    const file = path.join(dir, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, typeof content === "string" ? content : JSON.stringify(content, null, 2));
  }
  return dir;
}

export function validToolSource(overrides: Record<string, unknown> = {}) {
  return {
    kind: "tool",
    name: "yt-dlp",
    pin: "2025.01.12",
    assets: {
      "macos-arm64": { asset: "yt-dlp_macos", checksum: SHA },
      "windows-x86_64": { asset: "yt-dlp.exe", checksum: SHA },
    },
    upstream: { type: "github-release", repo: "yt-dlp/yt-dlp" },
    journey: { command: ["node", "scripts/x.mjs", "--resource-dir", "{candidate}", "--out", "{out}"], report: "{out}/report.json" },
    ...overrides,
  };
}

export function manifest(sources: unknown[], policy: Record<string, unknown> = { default: "auto-land" }) {
  return { schemaVersion: 1, sources, policy };
}

type Route = string | object | Response | ((url: string) => string | object | Response);

/** Fetch double. Unknown URLs return 404. Every requested URL is recorded. */
export function mockFetch(routes: Record<string, Route>): { fetch: FetchLike; calls: string[] } {
  const calls: string[] = [];
  const fetch: FetchLike = async (url) => {
    calls.push(url);
    const route = routes[url];
    if (route === undefined) return new Response("not found", { status: 404 });
    const value = typeof route === "function" ? route(url) : route;
    if (value instanceof Response) return value;
    if (typeof value === "string") return new Response(value, { status: 200 });
    return new Response(JSON.stringify(value), { status: 200, headers: { "content-type": "application/json" } });
  };
  return { fetch, calls };
}

// ---------------------------------------------------------------- process and git helpers

import { execFileSync } from "node:child_process";
import { runCommand, type RunFn, type RunOptions, type RunResult } from "../src/exec.js";

// Keep git hermetic: no user or system config, fixed identity.
// An empty file, not the null device: Git for Windows cannot open `\\.\nul` as a config file.
const emptyGitConfig = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "rk-update-gitconfig-")), "gitconfig");
fs.writeFileSync(emptyGitConfig, "");
process.env.GIT_CONFIG_GLOBAL = emptyGitConfig;
process.env.GIT_CONFIG_NOSYSTEM = "1";
process.env.GIT_AUTHOR_NAME = "Test";
process.env.GIT_AUTHOR_EMAIL = "test@example.com";
process.env.GIT_COMMITTER_NAME = "Test";
process.env.GIT_COMMITTER_EMAIL = "test@example.com";

export function git(dir: string, ...args: string[]): string {
  return execFileSync("git", args, { cwd: dir, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
}

/** A temp repo with the given files, `git init -b main`, and one commit. */
export function gitRepo(files: Record<string, string | object>): string {
  const dir = mkRepo(files);
  git(dir, "init", "-q", "-b", "main");
  git(dir, "add", "-A");
  git(dir, "commit", "-q", "-m", "init");
  return dir;
}

export const ok = (stdout = "", stderr = ""): RunResult => ({ code: 0, signal: null, timedOut: false, durationMs: 1, stdout, stderr });
export const fail = (stderr = "boom", code = 1): RunResult => ({ code, signal: null, timedOut: false, durationMs: 1, stdout: "", stderr });

/**
 * A RunFn that lets a test answer chosen commands and falls through to the real runner for the rest
 * (git, node). Every call is recorded.
 */
export function fakeRun(intercept: (argv: string[], options: RunOptions) => RunResult | Promise<RunResult> | undefined): { run: RunFn; calls: string[][] } {
  const calls: string[][] = [];
  const run: RunFn = async (argv, options) => {
    calls.push(argv);
    const answered = await intercept(argv, options);
    return answered ?? runCommand(argv, options);
  };
  return { run, calls };
}

/** Body of a download response, as a fetch Response. */
export function binary(bytes: Buffer | string, headers: Record<string, string> = {}): Response {
  return new Response(typeof bytes === "string" ? Buffer.from(bytes) : bytes, { status: 200, headers });
}

// ---------------------------------------------------------------- report fixtures

import crypto from "node:crypto";
import type { ApplyChange, ApplyReport } from "../src/reports.js";

export const sha256 = (data: string | Buffer): string => crypto.createHash("sha256").update(data).digest("hex");

export const TOOL_BYTES = "candidate-binary-bytes";
export const TOOL_WIN_BYTES = "windows-binary-bytes";

/** Stage a yt-dlp candidate on disk and return the matching apply-report change. */
export function stageTool(repo: string, version = "2025.10.01"): ApplyChange {
  const candidateDir = `.rightkit/update/candidates/yt-dlp/${version}`;
  const dir = path.join(repo, ...candidateDir.split("/"));
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, "yt-dlp_macos"), TOOL_BYTES, { mode: 0o755 });
  return {
    kind: "tool",
    name: "yt-dlp",
    source: "tool:yt-dlp",
    before: "2025.01.12",
    after: version,
    latest: version,
    status: "staged",
    files: [],
    commands: [],
    notes: [],
    tool: {
      platform: "macos-arm64",
      tag: version,
      version,
      asset: "yt-dlp_macos",
      sourceUrl: `https://github.com/yt-dlp/yt-dlp/releases/download/${version}/yt-dlp_macos`,
      candidateDir,
      binary: "yt-dlp_macos",
      size: TOOL_BYTES.length,
      sha256: sha256(TOOL_BYTES),
      trust: "observed",
      previousChecksum: SHA,
      otherPlatforms: [
        { platform: "windows-x86_64", asset: "yt-dlp.exe", size: TOOL_WIN_BYTES.length, sha256: sha256(TOOL_WIN_BYTES), trust: "observed", previousChecksum: SHA },
      ],
    },
  };
}

export function writeApplyReport(repo: string, changes: ApplyChange[], extra: Partial<ApplyReport> = {}): ApplyReport {
  let baseCommit: string | null = null;
  try {
    baseCommit = git(repo, "rev-parse", "HEAD");
  } catch {
    baseCommit = null;
  }
  const report: ApplyReport = {
    schemaVersion: 1,
    tool: "right-update",
    version: "0.2.0",
    repo,
    createdAt: new Date().toISOString(),
    dryRun: false,
    baseCommit,
    changes,
    needsMigration: [],
    held: [],
    skipped: [],
    failed: [],
    landed: null,
    ...extra,
  };
  const file = path.join(repo, ".rightkit/update/apply-report.json");
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(path.join(path.dirname(file), ".gitignore"), "*\n");
  fs.writeFileSync(file, `${JSON.stringify(report, null, 2)}\n`);
  return report;
}

/** Write a gate-report bound to the current apply-report (or to `boundTo`). */
export function writeGateReport(repo: string, options: { passed?: boolean; boundTo?: string | null } = {}): void {
  const passed = options.passed ?? true;
  const applyFile = path.join(repo, ".rightkit/update/apply-report.json");
  const bound = options.boundTo !== undefined ? options.boundTo : fs.existsSync(applyFile) ? sha256(fs.readFileSync(applyFile)) : null;
  const report = {
    schemaVersion: 1,
    repo,
    startedAt: new Date().toISOString(),
    finishedAt: new Date().toISOString(),
    applyReportSha256: bound,
    passed,
    exitCode: passed ? 0 : 1,
    gates: [],
    journeys: [],
    warnings: [],
  };
  fs.writeFileSync(path.join(repo, ".rightkit/update/gate-report.json"), `${JSON.stringify(report, null, 2)}\n`);
}

/** FileChange for a file edited since `before` text. */
export function fileChange(repo: string, rel: string, before: string | null) {
  const abs = path.join(repo, rel);
  return { path: rel, sha256Before: before === null ? null : sha256(before), sha256After: fs.existsSync(abs) ? sha256(fs.readFileSync(abs)) : null };
}
