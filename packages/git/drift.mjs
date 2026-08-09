// Compare installed workflows against what the manifest would render today
// (DESIGN.md §4, invariant I4: no silent forks). `--all` enumerates every
// repo carrying a `.rightgit.json` under a workspace root and re-checks
// visibility, flagging public->private flips (DESIGN.md §5 layer 3).

import fs from "node:fs";
import path from "node:path";
import { assertPublic } from "./policy/visibility-gate.mjs";
import { loadManifest } from "./manifest.mjs";
import { renderLanes, listManagedFiles } from "./sync.mjs";
import { MANAGED_MARKER } from "./render.mjs";

const IGNORED_DIRS = new Set(["node_modules", ".git", ".pnpm-store"]);

/**
 * Drift-check one repo. Never throws. Returns:
 * { ok, driftDetected, visibility, findings: [{filename, status}], errors }
 * status is one of: match | mismatch | missing | unmanaged-extra | orphan
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
      };
    }
    return { ok: false, driftDetected: null, visibility, findings: [], errors: loaded.errors };
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
  return { ok: true, driftDetected, visibility, findings, errors: [] };
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
