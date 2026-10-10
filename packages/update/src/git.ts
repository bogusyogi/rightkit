// Thin git helpers over the bounded runner. Argument arrays only, no shell.

import fs from "node:fs";
import type { RunFn } from "./exec.js";

const GIT_TIMEOUT_MS = 120_000;

export interface GitContext {
  run: RunFn;
  repo: string;
}

async function git(ctx: GitContext, args: string[], extra: { stdin?: string; timeoutMs?: number; captureBytes?: number } = {}) {
  return ctx.run(["git", ...args], { cwd: ctx.repo, timeoutMs: extra.timeoutMs ?? GIT_TIMEOUT_MS, stdin: extra.stdin, captureBytes: extra.captureBytes });
}

/** True when `repo` is exactly the top level of a git work tree. */
export async function isRepoRoot(ctx: GitContext): Promise<boolean> {
  const result = await git(ctx, ["rev-parse", "--show-toplevel"]);
  if (result.code !== 0) return false;
  try {
    return fs.realpathSync(result.stdout.trim()) === fs.realpathSync(ctx.repo);
  } catch {
    return false;
  }
}

export async function headCommit(ctx: GitContext): Promise<string | null> {
  const result = await git(ctx, ["rev-parse", "HEAD"]);
  return result.code === 0 ? result.stdout.trim() : null;
}

export async function currentBranch(ctx: GitContext): Promise<string | null> {
  const result = await git(ctx, ["symbolic-ref", "--short", "-q", "HEAD"]);
  return result.code === 0 ? result.stdout.trim() : null;
}

/** Default branch: origin/HEAD when set, else `main` or `master` if one exists locally. */
export async function defaultBranch(ctx: GitContext): Promise<string | null> {
  const remote = await git(ctx, ["symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD"]);
  if (remote.code === 0 && remote.stdout.trim().startsWith("origin/")) return remote.stdout.trim().slice("origin/".length);
  for (const name of ["main", "master"]) {
    const exists = await git(ctx, ["show-ref", "--verify", "--quiet", `refs/heads/${name}`]);
    if (exists.code === 0) return name;
  }
  return null;
}

export interface DirtyResult {
  ok: boolean;
  /** Repository-relative, `/`-separated paths with any change (staged, unstaged, untracked). Renames list both sides. */
  paths: string[];
  error?: string;
}

/** Every path with a change, from `git status --porcelain=v1 -z --untracked-files=all`. */
export async function dirtyPaths(ctx: GitContext): Promise<DirtyResult> {
  const result = await git(ctx, ["status", "--porcelain=v1", "-z", "--untracked-files=all"], { captureBytes: 64 * 1024 * 1024 });
  if (result.code !== 0) return { ok: false, paths: [], error: result.error ?? (result.stderr.trim() || `git status exited ${result.code}`) };
  const tokens = result.stdout.split("\0");
  const paths: string[] = [];
  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i];
    if (token.length < 4) continue;
    const xy = token.slice(0, 2);
    paths.push(token.slice(3));
    if (xy.includes("R") || xy.includes("C")) {
      const original = tokens[++i];
      if (original) paths.push(original);
    }
  }
  return { ok: true, paths };
}

export async function addPaths(ctx: GitContext, paths: string[]) {
  return git(ctx, ["add", "--", ...paths]);
}

export async function commitWithMessage(ctx: GitContext, message: string) {
  return git(ctx, ["commit", "-F", "-"], { stdin: message, timeoutMs: 600_000 });
}

export async function pushCurrent(ctx: GitContext) {
  return git(ctx, ["push"], { timeoutMs: 600_000 });
}
