// Compare installed workflows against what the manifest would render today
// (DESIGN.md §4, invariant I4: no silent forks). `--all` enumerates every
// repo carrying a `.rightgit.json` under a workspace root and re-checks
// visibility, flagging public->private flips (DESIGN.md §5 layer 3).

import fs from "node:fs";
import path from "node:path";
import { execFile as execFileCb } from "node:child_process";
import { promisify } from "node:util";
import { assertPublic, parseOwnerRepo } from "./policy/visibility-gate.mjs";
import { loadManifest } from "./manifest.mjs";
import { renderLanes, listManagedFiles } from "./sync.mjs";
import { MANAGED_MARKER } from "./render.mjs";

const IGNORED_DIRS = new Set(["node_modules", ".git", ".pnpm-store"]);
const execFileAsync = promisify(execFileCb);

/**
 * Drift-check one repo. Never throws. Returns:
 * { ok, driftDetected, visibility, findings: [{filename, status}], errors, warnings, notes }
 * status is one of: match | mismatch | missing | unmanaged-extra | orphan
 * warnings are advisory (never drift): see checkRegenerateApproval.
 * notes record checks that were skipped and why.
 */
export async function driftCommand(repoPath, deps = {}) {
  const visibility = await assertPublic(repoPath, deps);

  const loaded = loadManifest(repoPath);
  if (!loaded.ok) {
    // F7: a repo can carry right-git-managed workflow files (identified by
    // MANAGED_MARKER) with no readable .rightgit.json — e.g. the manifest
    // was deleted, never committed, or lost in an interrupted install. That
    // is drift to report, not a tool failure to hide behind: the old
    // behavior returned a hard `ok:false` here with no findings, which made
    // `findManagedRepos` (below) — and therefore `drift --all` — blind to
    // exactly this repo.
    const orphanManaged = listManagedFiles(repoPath);
    if (orphanManaged.length > 0) {
      return {
        ok: true,
        driftDetected: true,
        visibility,
        findings: orphanManaged.map((filename) => ({ filename, status: "orphan" })),
        errors: [],
        warnings: [],
        notes: [],
      };
    }
    return { ok: false, driftDetected: null, visibility, findings: [], errors: loaded.errors, warnings: [], notes: [] };
  }

  const rendered = renderLanes(loaded.manifest, repoPath);
  const wantedFilenames = new Set(rendered.map((r) => r.filename));
  const findings = [];

  for (const { filename, content } of rendered) {
    const target = path.join(repoPath, ".github", "workflows", filename);
    let existing = null;
    try {
      existing = fs.readFileSync(target, "utf8");
    } catch {
      // not installed
    }
    if (existing === null) {
      findings.push({ filename, status: "missing" });
    } else if (existing !== content) {
      findings.push({ filename, status: "mismatch" });
    } else {
      findings.push({ filename, status: "match" });
    }
  }

  for (const managedFilename of listManagedFiles(repoPath)) {
    if (!wantedFilenames.has(managedFilename)) {
      findings.push({ filename: managedFilename, status: "unmanaged-extra" });
    }
  }

  const driftDetected = findings.some((f) => f.status !== "match") || !visibility.allowed;

  const warnings = [];
  const notes = [];
  if (loaded.manifest.lanes.includes("regenerate")) {
    const approval = await checkRegenerateApproval(repoPath, deps);
    if (approval.status === "warn") warnings.push(approval.message);
    else if (approval.status === "skipped") notes.push(approval.message);
  }
  return { ok: true, driftDetected, visibility, findings, errors: [], warnings, notes };
}

/**
 * The regenerate lane opens its pull request with the workflow token, which
 * GitHub refuses unless the repository allows Actions to create and approve
 * pull requests. Reads `gh api repos/{owner}/{repo}/actions/permissions/workflow`
 * (field can_approve_pull_request_reviews). Read-only: an unavailable or
 * unreadable lookup is a skipped check with a note, never a drift finding.
 * Returns { status: "ok" | "warn" | "skipped", message }. Never throws.
 */
export async function checkRegenerateApproval(repoPath, deps = {}) {
  const execFile = deps.execFile || execFileAsync;
  const skip = (reason) => ({
    status: "skipped",
    message: `regenerate lane: skipped the Actions pull-request approval check (${reason})`,
  });

  let remoteUrl;
  try {
    const { stdout } = await execFile("git", ["remote", "get-url", "origin"], { cwd: repoPath });
    remoteUrl = stdout.trim();
  } catch (error) {
    return skip(`no origin remote: ${firstLine(error)}`);
  }
  const ownerRepo = parseOwnerRepo(remoteUrl);
  if (!ownerRepo) return skip("origin is not a github.com repository");

  let stdout;
  try {
    ({ stdout } = await execFile("gh", ["api", `repos/${ownerRepo.owner}/${ownerRepo.repo}/actions/permissions/workflow`], { cwd: repoPath }));
  } catch (error) {
    return skip(error.code === "ENOENT" ? "gh is not installed" : `gh api failed: ${firstLine(error)}`);
  }

  let settings;
  try {
    settings = JSON.parse(stdout);
  } catch {
    return skip("gh api returned no JSON");
  }
  if (typeof settings?.can_approve_pull_request_reviews !== "boolean") {
    return skip("response has no can_approve_pull_request_reviews");
  }
  if (settings.can_approve_pull_request_reviews) return { status: "ok", message: null };
  return {
    status: "warn",
    message:
      "regenerate lane: the repository setting \"Allow GitHub Actions to create and approve pull requests\" is off, so regenerate.yml cannot open its pull request. Enable it under Settings > Actions > General.",
  };
}

function firstLine(error) {
  return String(error?.message ?? error).split("\n")[0];
}

/**
 * Recursively find every repo under root carrying right-git-managed state:
 * either a `.rightgit.json` manifest, or (F7) a `.github/workflows/*.yml`
 * file that starts with MANAGED_MARKER with no manifest present — an
 * "orphan" repo the old implementation never discovered because it looked
 * only for the manifest filename.
 */
export function findManagedRepos(root) {
  const found = new Set();
  const stack = [root];
  while (stack.length > 0) {
    const dir = stack.pop();
    let entries;
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      continue;
    }

    if (entries.some((e) => e.isFile() && e.name === ".rightgit.json")) {
      found.add(dir);
    }

    if (path.basename(dir) === "workflows" && path.basename(path.dirname(dir)) === ".github") {
      const hasManagedWorkflow = entries.some(
        (e) => e.isFile() && (e.name.endsWith(".yml") || e.name.endsWith(".yaml")) && startsWithManagedMarker(path.join(dir, e.name)),
      );
      if (hasManagedWorkflow) {
        found.add(path.dirname(path.dirname(dir)));
      }
    }

    for (const entry of entries) {
      if (entry.isDirectory() && !IGNORED_DIRS.has(entry.name)) {
        stack.push(path.join(dir, entry.name));
      }
    }
  }
  return [...found];
}

function startsWithManagedMarker(filePath) {
  try {
    return fs.readFileSync(filePath, "utf8").startsWith(MANAGED_MARKER);
  } catch {
    return false;
  }
}

/** `right-git drift --all <workspace-root>`: drift-check every managed repo found under root. */
export async function driftAll(workspaceRoot, deps = {}) {
  const repos = findManagedRepos(workspaceRoot);
  const results = [];
  for (const repoPath of repos) {
    results.push({ repoPath, result: await driftCommand(repoPath, deps) });
  }
  return results;
}
