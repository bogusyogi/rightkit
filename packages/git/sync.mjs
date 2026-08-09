// Render templates for a target repo's manifest and install them into
// <repo>/.github/workflows/, showing a diff before applying (DESIGN.md §4).
// Refuses private/internal/unknown-visibility repos — no override (I1).

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { assertPublic } from "./policy/visibility-gate.mjs";
import { loadManifest, LANE_FILENAMES, buildRenderContext } from "./manifest.mjs";
import { renderTemplate, MANAGED_MARKER } from "./render.mjs";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)));
const TEMPLATES_DIR = path.join(packageRoot, "templates");
const WORKFLOWS_SUBDIR = path.join(".github", "workflows");

/** Render every lane declared in the manifest. Returns { lane, filename, content }[]. */
export function renderLanes(manifest, repoPath) {
  const context = buildRenderContext(manifest, repoPath);
  return manifest.lanes.map((lane) => {
    const filename = LANE_FILENAMES[lane];
    const templatePath = manifest.profile === "rust-hybrid"
      ? path.join(TEMPLATES_DIR, "profiles", "rust-hybrid", filename)
      : path.join(TEMPLATES_DIR, filename);
    const templateText = fs.readFileSync(templatePath, "utf8");
    return { lane, filename, content: renderTemplate(templateText, context) };
  });
}

/** List files in <repo>/.github/workflows that carry the right-git managed marker. */
export function listManagedFiles(repoPath) {
  const dir = path.join(repoPath, WORKFLOWS_SUBDIR);
  let entries;
  try {
    entries = fs.readdirSync(dir);
  } catch {
    return [];
  }
  return entries.filter((name) => {
    if (!name.endsWith(".yml") && !name.endsWith(".yaml")) return false;
    try {
      const content = fs.readFileSync(path.join(dir, name), "utf8");
      return content.startsWith(MANAGED_MARKER);
    } catch {
      return false;
    }
  });
}

/** Minimal line-based diff: common prefix/suffix trimmed, middle shown as -/+ . */
export function lineDiff(oldText, newText) {
  const oldLines = oldText == null ? [] : oldText.split("\n");
  const newLines = newText.split("\n");
  let start = 0;
  while (start < oldLines.length && start < newLines.length && oldLines[start] === newLines[start]) start++;
  let oldEnd = oldLines.length;
  let newEnd = newLines.length;
  while (oldEnd > start && newEnd > start && oldLines[oldEnd - 1] === newLines[newEnd - 1]) {
    oldEnd--;
    newEnd--;
  }
  const out = [];
  for (let i = start; i < oldEnd; i++) out.push(`-${oldLines[i]}`);
  for (let i = start; i < newEnd; i++) out.push(`+${newLines[i]}`);
  return out;
}

/**
 * Compute the sync plan without writing anything: visibility check, manifest
 * load, per-lane render, diff against what's on disk, plus removal of any
 * managed file whose lane fell out of the manifest.
 * Returns a typed result — never throws.
 */
export async function planSync(repoPath, deps = {}, manifestOverride = null) {
  const visibility = await assertPublic(repoPath, deps);
  if (!visibility.allowed) {
    return { ok: false, stage: "visibility", errors: [visibility.reason], changes: [] };
  }

  const loaded = manifestOverride === null
    ? loadManifest(repoPath)
    : { ok: true, manifest: manifestOverride };
  if (!loaded.ok) {
    return { ok: false, stage: "manifest", errors: loaded.errors, changes: [] };
  }

  const rendered = renderLanes(loaded.manifest, repoPath);
  const wantedFilenames = new Set(rendered.map((r) => r.filename));
  const changes = [];

  for (const { lane, filename, content } of rendered) {
    const target = path.join(repoPath, WORKFLOWS_SUBDIR, filename);
    let existing = null;
    try {
      existing = fs.readFileSync(target, "utf8");
    } catch {
      // not installed yet
    }
    if (existing === content) {
      changes.push({ lane, filename, action: "unchanged", before: existing, diff: [] });
    } else if (existing !== null && !existing.startsWith(MANAGED_MARKER)) {
      changes.push({ lane, filename, action: "managed-conflict", before: existing, diff: [] });
    } else {
      changes.push({ lane, filename, action: existing === null ? "create" : "update", before: existing, diff: lineDiff(existing, content), content });
    }
  }

  for (const managedFilename of listManagedFiles(repoPath)) {
    if (!wantedFilenames.has(managedFilename)) {
      changes.push({ lane: null, filename: managedFilename, action: "remove", before: fs.readFileSync(path.join(repoPath, WORKFLOWS_SUBDIR, managedFilename), "utf8"), diff: [] });
    }
  }

  const conflicts = changes.filter((change) => change.action === "managed-conflict");
  return conflicts.length
    ? { ok: false, stage: "managed-conflict", errors: conflicts.map((change) => `${change.filename} is unmanaged`), changes }
    : { ok: true, stage: "planned", errors: [], changes };
}

/** Apply a plan produced by planSync: write/update/remove files on disk. */
export function applyPlan(repoPath, plan) {
  const dir = path.join(repoPath, WORKFLOWS_SUBDIR);
  for (const change of plan.changes) {
    if (change.action === "unchanged") continue;
    const target = path.join(dir, change.filename);
    let current = null;
    try { current = fs.readFileSync(target, "utf8"); } catch {}
    if (current !== change.before) {
      return { ok: false, stage: "apply-conflict", errors: [`${change.filename} changed after planning`] };
    }
  }
  fs.mkdirSync(dir, { recursive: true });
  for (const change of plan.changes) {
    const target = path.join(dir, change.filename);
    if (change.action === "create" || change.action === "update") {
      fs.writeFileSync(target, change.content, "utf8");
    } else if (change.action === "remove") {
      fs.rmSync(target, { force: true });
    }
  }
  return { ok: true, stage: "applied", errors: [] };
}

/** Full sync: plan, print diff, apply unless dryRun. Returns the plan. */
export async function syncCommand(repoPath, { dryRun = false, deps = {} } = {}) {
  const plan = await planSync(repoPath, deps);
  if (!plan.ok) return plan;

  for (const change of plan.changes) {
    if (change.action === "unchanged") continue;
    console.log(`\n${change.action.toUpperCase()} ${change.filename}`);
    for (const line of change.diff) console.log(line);
  }

  if (!dryRun) {
    const applied = applyPlan(repoPath, plan);
    if (!applied.ok) return { ...plan, ...applied };
  }
  return plan;
}
