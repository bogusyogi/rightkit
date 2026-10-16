// The public-only enforcement (DESIGN.md §5, invariant I1). Install-time
// layer: resolve a repo's visibility and hard-refuse anything that is not
// confirmed public. No override flag exists in v1 — that is a deliberate
// policy change to this file, never a CLI flag.
//
// All child-process / network access is injected so tests can run fully
// offline (never hit gh or the network from a test).

import { execFile as execFileCb } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFileCb);

/** @typedef {"public"|"private"|"internal"|"unknown"} Visibility */

/**
 * Query `gh repo view --json visibility` in repoPath.
 * Returns { ok, visibility, raw, error }, never throws.
 */
async function ghVisibility(repoPath, execFile) {
  try {
    const { stdout } = await execFile("gh", ["repo", "view", "--json", "visibility"], { cwd: repoPath });
    const parsed = JSON.parse(stdout);
    const visibility = String(parsed.visibility || "").toLowerCase();
    if (visibility === "public" || visibility === "private" || visibility === "internal") {
      return { ok: true, visibility, raw: parsed, error: null };
    }
    return { ok: false, visibility: "unknown", raw: parsed, error: `unrecognized gh visibility value: ${parsed.visibility}` };
  } catch (error) {
    return { ok: false, visibility: "unknown", raw: null, error: error.message };
  }
}

const GITHUB_HOST = "github.com";

/**
 * Normalize a git remote into something the WHATWG `URL` parser accepts.
 * Full URLs (`https://…`, `ssh://…`, `git://…`) pass through unchanged.
 * Git's scp-like shorthand (`[user@]host:path`, no `://`) is rewritten to
 * `ssh://[user@]host/path` so the same parser handles both forms — that
 * parser is what does the real work below, not this rewrite.
 */
function toParseableUrl(remoteUrl) {
  const trimmed = remoteUrl.trim();
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//.test(trimmed)) return trimmed;
  const scpMatch = /^([a-zA-Z0-9_.-]+@)?([a-zA-Z0-9.-]+):(?!\/\/)(.+)$/.exec(trimmed);
  if (!scpMatch) return trimmed;
  const [, userAt = "", host, path] = scpMatch;
  return `ssh://${userAt}${host}/${path}`;
}

/**
 * Parse an owner/repo pair out of a git remote URL (https or ssh form).
 *
 * N1 fix: host matching must be exact, not substring. The prior regex
 * searched the raw string for the text `github.com` anywhere, so
 * `notgithub.com`, `github.com.evil.io`, and `https://github.com@evil.io/…`
 * (github.com as userinfo, real host evil.io) were all misread as GitHub.
 * This parses the remote as a real URL and requires `url.hostname` to equal
 * `github.com` exactly (case-insensitive; WHATWG URL already lowercases
 * hostnames) — the URL parser itself separates userinfo/port from host, so
 * those tricks fall out for free. Enterprise hosts (e.g. github.example.com)
 * and any other non-exact host still fail closed to null.
 *
 * F16 fix (kept): the repo-name group allows internal dots (e.g.
 * `repo.with.dot`) — it only excludes `/` (which ends the segment), not `.`.
 * A trailing `.git` suffix is stripped afterward in code.
 */
export function parseOwnerRepo(remoteUrl) {
  let url;
  try {
    url = new URL(toParseableUrl(remoteUrl));
  } catch {
    return null;
  }
  if (url.hostname.toLowerCase() !== GITHUB_HOST) return null;

  const match = /^\/?([^/]+)\/([^/]+?)\/?$/.exec(url.pathname);
  if (!match) return null;
  const owner = match[1];
  const repo = match[2].endsWith(".git") ? match[2].slice(0, -4) : match[2];
  if (!owner || !repo) return null;
  return { owner, repo };
}

/**
 * Fallback: read the origin remote, then ask the public GitHub API for
 * visibility. Unauthenticated API calls can only positively confirm
 * "public" (a 404 is ambiguous: could be private, could be nonexistent) —
 * so this fallback only ever returns "public" or "unknown", never a false
 * "private"/"internal" reading that a hard refusal downstream would relax.
 */
async function gitRemoteApiVisibility(repoPath, execFile, fetchFn) {
  let remoteUrl;
  try {
    const { stdout } = await execFile("git", ["remote", "get-url", "origin"], { cwd: repoPath });
    remoteUrl = stdout.trim();
  } catch (error) {
    return { ok: false, visibility: "unknown", raw: null, error: `git remote get-url origin failed: ${error.message}` };
  }

  const ownerRepo = parseOwnerRepo(remoteUrl);
  if (!ownerRepo) {
    return { ok: false, visibility: "unknown", raw: null, error: `could not parse owner/repo from remote: ${remoteUrl}` };
  }

  try {
    const response = await fetchFn(`https://api.github.com/repos/${ownerRepo.owner}/${ownerRepo.repo}`, {
      headers: { Accept: "application/vnd.github+json", "User-Agent": "right-git" },
    });
    if (!response.ok) {
      return { ok: false, visibility: "unknown", raw: { status: response.status }, error: `github api returned ${response.status}` };
    }
    const body = await response.json();
    if (body.private === false) {
      return { ok: true, visibility: "public", raw: body, error: null };
    }
    // private === true, or field absent: cannot distinguish private/internal
    // over the unauthenticated API — fail closed to "unknown".
    return { ok: false, visibility: "unknown", raw: body, error: "unauthenticated API cannot confirm public visibility" };
  } catch (error) {
    return { ok: false, visibility: "unknown", raw: null, error: error.message };
  }
}

/**
 * Resolve the visibility of the repo at repoPath.
 * deps allows tests to inject fakes for execFile/fetch (never hit the
 * network or spawn a real process from a test).
 * Returns { visibility, source, raw, error } — never throws.
 */
export async function resolveVisibility(repoPath, deps = {}) {
  const execFile = deps.execFile || execFileAsync;
  const fetchFn = deps.fetch || globalThis.fetch;

  const gh = await ghVisibility(repoPath, execFile);
  if (gh.ok) {
    return { visibility: gh.visibility, source: "gh", raw: gh.raw, error: null };
  }

  const fallback = await gitRemoteApiVisibility(repoPath, execFile, fetchFn);
  if (fallback.ok) {
    return { visibility: fallback.visibility, source: "git-remote-api", raw: fallback.raw, error: null };
  }

  return {
    visibility: "unknown",
    source: "error",
    raw: null,
    error: `gh: ${gh.error}; fallback: ${fallback.error}`,
  };
}

/**
 * I1: hard refusal of anything not confirmed public. No override.
 * Returns a typed result: { allowed, visibility, source, reason }.
 */
export async function assertPublic(repoPath, deps = {}) {
  const result = await resolveVisibility(repoPath, deps);
  if (result.visibility === "public") {
    return { allowed: true, visibility: result.visibility, source: result.source, reason: null };
  }
  const reason =
    result.visibility === "unknown"
      ? `could not confirm repository visibility (fail closed): ${result.error}`
      : `repository visibility is '${result.visibility}'; right-git installs into public repos only (invariant I1)`;
  return { allowed: false, visibility: result.visibility, source: result.source, reason };
}
