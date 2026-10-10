// `right-update land` (and `discard`): turn a verified apply into a commit.
//
// Land refuses unless the apply-report, a passing gate-report bound to it, the git tree and the
// branch all agree. It stages only the files the apply-report names, never `git add -A`.

import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { runCommand, type RunFn } from "./exec.js";
import { addPaths, commitWithMessage, currentBranch, defaultBranch, dirtyPaths, headCommit, isRepoRoot, pushCurrent } from "./git.js";
import { writeHeldReport } from "./apply.js";
import { loadManifest, MANIFEST_RELATIVE_PATH } from "./manifest.js";
import { loadApplyReport, loadGateReport, type ApplyChange, type ApplyReport } from "./reports.js";
import {
  APPLY_REPORT,
  CANDIDATES_DIR,
  GATE_REPORT,
  deliveryDir,
  sha256File,
  stateFile,
  writeFileAtomic,
  writeJsonAtomic,
  STATE_DIR,
  type HostEnv,
} from "./state.js";
import type { ToolSource, UpdateManifest } from "./types.js";

export interface LandDeps {
  run?: RunFn;
  host?: HostEnv;
  now?: () => Date;
}

export interface LandOutcome {
  /** 0 landed or nothing to land, 1 refused by a precondition (nothing changed), 2 failed part-way. */
  exitCode: 0 | 1 | 2;
  committed: boolean;
  commit?: string;
  pushed: boolean;
  pushVia?: "git";
  changes: number;
  held: number;
  delivered: string[];
  refusals: string[];
  errors: string[];
  notes: string[];
}

const TRAILER = "Co-Authored-By: right-update <noreply@rightkit.dev>";

function emptyOutcome(): LandOutcome {
  return { exitCode: 0, committed: false, pushed: false, changes: 0, held: 0, delivered: [], refusals: [], errors: [], notes: [] };
}

const toAbs = (repo: string, rel: string): string => path.join(repo, ...rel.split("/"));

export function commitMessage(changes: ApplyChange[]): string {
  const lines = changes.map((c) => {
    if (c.kind === "tool" && c.tool) return `- tool ${c.name} ${c.before} -> ${c.after} (sha256:${c.tool.sha256.slice(0, 12)}, ${c.tool.platform})`;
    return `- ${c.kind} ${c.name} ${c.before} -> ${c.after} (${c.source})`;
  });
  return `deps: ${changes.length} updates via right-update\n\n${lines.join("\n")}\n\n${TRAILER}\n`;
}

// ---------------------------------------------------------------- tool delivery

type ToolState = "candidate" | "delivered" | "missing" | "mismatch";

function toolAssetPath(base: string, change: ApplyChange): string {
  return path.join(base, change.tool!.asset);
}

function toolState(repo: string, change: ApplyChange, delivery: string): { state: ToolState; detail?: string } {
  const stage = change.tool!;
  const candidate = toAbs(repo, stage.candidateDir);
  if (fs.existsSync(candidate)) {
    const actual = sha256File(toolAssetPath(candidate, change));
    if (actual !== stage.sha256) return { state: "mismatch", detail: `staged ${stage.asset} digest ${actual ?? "missing"} differs from the apply-report ${stage.sha256}` };
    return { state: "candidate" };
  }
  if (fs.existsSync(delivery)) {
    const actual = sha256File(toolAssetPath(delivery, change));
    if (actual === stage.sha256) return { state: "delivered" };
    return { state: "mismatch", detail: `delivery directory ${delivery} exists with a different ${stage.asset}` };
  }
  return { state: "missing", detail: `no staged candidate at ${stage.candidateDir} and nothing delivered at ${delivery}` };
}

/** Move a verified candidate into the delivery directory atomically (same-filesystem rename of a sibling temp dir). */
function deliver(repo: string, change: ApplyChange, delivery: string, now: Date): void {
  const stage = change.tool!;
  const candidate = toAbs(repo, stage.candidateDir);
  const parent = path.dirname(delivery);
  fs.mkdirSync(parent, { recursive: true });
  const temp = path.join(parent, `.${path.basename(delivery)}.tmp-${crypto.randomBytes(4).toString("hex")}`);
  try {
    try {
      fs.renameSync(candidate, temp);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "EXDEV") throw error;
      fs.cpSync(candidate, temp, { recursive: true, errorOnExist: true, force: false });
    }
    const copied = sha256File(toolAssetPath(temp, change));
    if (copied !== stage.sha256) throw new Error(`digest mismatch after copy: ${copied} != ${stage.sha256}`);
    const receipt = {
      schemaVersion: 1,
      name: change.name,
      version: stage.version,
      platform: stage.platform,
      asset: stage.asset,
      binary: stage.binary,
      sha256: stage.sha256,
      ...(stage.binarySha256 ? { binarySha256: stage.binarySha256 } : {}),
      size: stage.size,
      sourceUrl: stage.sourceUrl,
      deliveredAt: now.toISOString(),
      deliveredBy: "right-update",
    };
    writeJsonAtomic(path.join(temp, "receipt.json"), receipt);
    fs.renameSync(temp, delivery);
  } catch (error) {
    fs.rmSync(temp, { recursive: true, force: true });
    throw error;
  }
  fs.rmSync(candidate, { recursive: true, force: true });
}

// ---------------------------------------------------------------- manifest rewrite

/** Update `pin` and the per-platform checksums for one tool, keeping the file's formatting when possible. */
export function rewriteToolInManifest(text: string, change: ApplyChange): string {
  const stage = change.tool!;
  const doc = JSON.parse(text) as { sources: Array<Record<string, unknown>> };
  const source = doc.sources.find((s) => s.kind === "tool" && s.name === change.name) as
    | { pin: string; assets: Record<string, { checksum: string }> }
    | undefined;
  if (!source) throw new Error(`tool ${change.name} not found in ${MANIFEST_RELATIVE_PATH}`);
  if (source.pin !== change.before) throw new Error(`tool ${change.name} pin is ${source.pin}, apply-report expected ${change.before}`);

  const checksumChanges: Array<[string, string]> = [];
  const hostAsset = source.assets[stage.platform];
  if (!hostAsset || hostAsset.checksum !== stage.previousChecksum) throw new Error(`checksum for ${change.name} ${stage.platform} changed since apply`);
  checksumChanges.push([hostAsset.checksum, `sha256:${stage.sha256}`]);
  for (const other of stage.otherPlatforms) {
    const entry = source.assets[other.platform];
    if (!entry || entry.checksum !== other.previousChecksum) throw new Error(`checksum for ${change.name} ${other.platform} changed since apply`);
    checksumChanges.push([entry.checksum, `sha256:${other.sha256}`]);
  }

  // Expected document after the change.
  const expected = JSON.parse(text) as typeof doc;
  const target = expected.sources.find((s) => s.kind === "tool" && s.name === change.name) as typeof source;
  target.pin = stage.version;
  target.assets[stage.platform].checksum = `sha256:${stage.sha256}`;
  for (const other of stage.otherPlatforms) target.assets[other.platform].checksum = `sha256:${other.sha256}`;

  const canonical = `${JSON.stringify(doc, null, 2)}\n`;
  if (text === canonical) return `${JSON.stringify(expected, null, 2)}\n`;

  // Targeted text edit when every literal is unique; otherwise reformat.
  const count = (needle: string) => text.split(needle).length - 1;
  const pinRe = new RegExp(`("pin"\\s*:\\s*)"${change.before.replace(/[.+*?^${}()|[\]\\]/g, "\\$&")}"`, "g");
  const pinMatches = text.match(pinRe)?.length ?? 0;
  const unique = pinMatches === 1 && checksumChanges.every(([old]) => count(`"${old}"`) === 1);
  if (unique) {
    let out = text.replace(pinRe, `$1"${stage.version}"`);
    for (const [old, fresh] of checksumChanges) out = out.split(`"${old}"`).join(`"${fresh}"`);
    if (JSON.stringify(JSON.parse(out)) === JSON.stringify(expected)) return out;
  }
  return `${JSON.stringify(expected, null, 2)}\n`;
}

// ---------------------------------------------------------------- land

export async function runLand(options: { repo: string }, deps: LandDeps = {}): Promise<LandOutcome> {
  const repo = path.resolve(options.repo);
  const run = deps.run ?? runCommand;
  const host = deps.host ?? {};
  const now = deps.now?.() ?? new Date();
  const out = emptyOutcome();
  const refuse = (reason: string): LandOutcome => {
    out.exitCode = 1;
    out.refusals.push(reason);
    return out;
  };

  const loaded = loadManifest(repo);
  if (!loaded.valid || !loaded.manifest) {
    out.exitCode = 2;
    out.errors.push(...loaded.errors);
    return out;
  }
  const manifest: UpdateManifest = loaded.manifest;

  const applyFile = stateFile(repo, APPLY_REPORT);
  const apply = loadApplyReport(applyFile);
  if (!apply.ok) return refuse(`no usable apply-report: ${apply.error}`);
  const report: ApplyReport = apply.value;
  if (report.dryRun) return refuse("the apply-report is from a --dry-run");
  if (report.landed) return refuse(`the apply-report was already landed in ${report.landed.commit}`);
  out.held = report.needsMigration.length + report.held.length;
  writeHeldReport(repo, report);

  if (report.changes.length === 0) {
    out.notes.push("nothing to land: the apply-report has no changes");
    return out;
  }
  out.changes = report.changes.length;

  const gate = loadGateReport(stateFile(repo, GATE_REPORT));
  if (!gate.ok) return refuse(`no usable gate-report: ${gate.error}`);
  if (!gate.value.passed || gate.value.exitCode !== 0) return refuse(`the gate did not pass (exit ${gate.value.exitCode})`);
  if (gate.value.applyReportSha256 !== apply.sha256) return refuse("the gate-report was produced for a different apply-report; run gate again");

  const git = { run, repo };
  if (!(await isRepoRoot(git))) return refuse(`${repo} is not the top level of a git work tree`);
  const branch = await currentBranch(git);
  const primary = await defaultBranch(git);
  if (!branch) return refuse("HEAD is detached; land only commits on the default branch");
  if (!primary || branch !== primary) return refuse(`current branch ${branch} is not the default branch (${primary ?? "unknown"})`);
  const head = await headCommit(git);
  if (report.baseCommit && head !== report.baseCommit) return refuse(`HEAD moved since apply (${report.baseCommit} -> ${head}); run apply again`);

  // Integrity of the files apply touched.
  const touched = new Set<string>();
  for (const change of report.changes) {
    for (const file of change.files) {
      touched.add(file.path);
      const actual = sha256File(toAbs(repo, file.path));
      if (actual !== file.sha256After) return refuse(`${file.path} changed since apply (expected ${file.sha256After ?? "absent"}, found ${actual ?? "absent"})`);
    }
  }
  const toolChanges = report.changes.filter((c) => c.kind === "tool" && c.tool);
  if (toolChanges.length > 0) touched.add(MANIFEST_RELATIVE_PATH);

  const dirty = await dirtyPaths(git);
  if (!dirty.ok) {
    out.exitCode = 2;
    out.errors.push(`cannot read git status: ${dirty.error}`);
    return out;
  }
  const stateDirPrefix = `${STATE_DIR}/`;
  const foreign = dirty.paths.filter((p) => !touched.has(p) && !p.startsWith(stateDirPrefix));
  if (toolChanges.length > 0 && dirty.paths.includes(MANIFEST_RELATIVE_PATH)) foreign.push(MANIFEST_RELATIVE_PATH);
  if (foreign.length > 0) {
    return refuse(`working tree has changes outside the apply-report: ${[...new Set(foreign)].slice(0, 20).join(", ")}${foreign.length > 20 ? ", ..." : ""}`);
  }

  // Tool candidates: verify each digest again right before delivery.
  const manifestSources = new Map(manifest.sources.filter((s): s is ToolSource => s.kind === "tool").map((s) => [s.name, s]));
  const deliveries = new Map<string, string>();
  for (const change of toolChanges) {
    const source = manifestSources.get(change.name);
    if (!source) return refuse(`tool ${change.name} is no longer in the manifest`);
    if (source.pin !== change.before) return refuse(`tool ${change.name} pin changed since apply (${change.before} -> ${source.pin})`);
    const delivery = deliveryDir(change.name, change.tool!.version, host);
    deliveries.set(change.name, delivery);
    const state = toolState(repo, change, delivery);
    if (state.state === "mismatch" || state.state === "missing") return refuse(`tool ${change.name}: ${state.detail}`);
  }

  // ---- point of no return for external effects: deliver, rewrite the manifest, commit.
  const manifestFile = stateFile(repo, MANIFEST_RELATIVE_PATH);
  const originalManifest = fs.readFileSync(manifestFile, "utf8");
  const pathsToAdd = new Set<string>();
  for (const file of touched) if (file !== MANIFEST_RELATIVE_PATH) pathsToAdd.add(file);
  let manifestChanged = false;

  try {
    for (const change of toolChanges) {
      const delivery = deliveries.get(change.name)!;
      if (toolState(repo, change, delivery).state === "candidate") {
        deliver(repo, change, delivery, now);
        out.delivered.push(delivery);
      } else {
        out.notes.push(`tool ${change.name} ${change.tool!.version} was already delivered to ${delivery}`);
      }
    }
    if (toolChanges.length > 0) {
      let next = originalManifest;
      for (const change of toolChanges) next = rewriteToolInManifest(next, change);
      writeFileAtomic(manifestFile, next);
      manifestChanged = true;
      pathsToAdd.add(MANIFEST_RELATIVE_PATH);
    }
  } catch (error) {
    if (manifestChanged) writeFileAtomic(manifestFile, originalManifest);
    out.exitCode = 2;
    out.errors.push(`delivery failed: ${(error as Error).message}`);
    return out;
  }

  const paths = [...pathsToAdd].sort();
  const rollback = async () => {
    if (manifestChanged) writeFileAtomic(manifestFile, originalManifest);
    await run(["git", "reset", "-q", "--", ...paths], { cwd: repo, timeoutMs: 60_000 });
  };

  const added = await addPaths(git, paths);
  if (added.code !== 0) {
    await rollback();
    out.exitCode = 2;
    out.errors.push(`git add failed: ${added.stderr.trim() || added.error || added.code}`);
    return out;
  }
  const committed = await commitWithMessage(git, commitMessage(report.changes));
  if (committed.code !== 0) {
    await rollback();
    out.exitCode = 2;
    out.errors.push(`git commit failed: ${committed.stderr.trim() || committed.stdout.trim() || committed.error || committed.code}`);
    return out;
  }
  const sha = await headCommit(git);
  out.committed = true;
  out.commit = sha ?? undefined;

  report.landed = { commit: sha ?? "unknown", at: now.toISOString() };
  try {
    writeJsonAtomic(applyFile, report);
  } catch (error) {
    out.notes.push(`committed, but could not mark the apply-report landed: ${(error as Error).message}`);
  }
  // Staged candidates for landed tools are gone; drop empty leftovers.
  fs.rmSync(path.join(repo, ...CANDIDATES_DIR.split("/")), { recursive: true, force: true });

  if ((manifest.policy.land ?? "commit") === "push") {
    if (fs.existsSync(path.join(repo, ".rightgit.json"))) {
      out.notes.push(".rightgit.json is present; @rightkit/git exposes no lane-trigger API, so a plain `git push` was used and the CI lanes run from the push");
    }
    const pushed = await pushCurrent(git);
    if (pushed.code !== 0) {
      out.exitCode = 2;
      out.errors.push(`git push failed (the commit ${sha} is local): ${pushed.stderr.trim().split("\n").slice(-4).join(" | ") || pushed.error || pushed.code}`);
      return out;
    }
    out.pushed = true;
    out.pushVia = "git";
  }
  return out;
}

// ---------------------------------------------------------------- discard

export interface DiscardOutcome {
  exitCode: 0 | 2;
  restored: string[];
  errors: string[];
}

/** Undo an unlanded apply: restore the files it touched (only if untouched since) and drop staged candidates. */
export async function runDiscard(options: { repo: string }, deps: { run?: RunFn } = {}): Promise<DiscardOutcome> {
  const repo = path.resolve(options.repo);
  const run = deps.run ?? runCommand;
  const apply = loadApplyReport(stateFile(repo, APPLY_REPORT));
  if (!apply.ok) return { exitCode: 2, restored: [], errors: [apply.error] };
  if (apply.value.landed) return { exitCode: 2, restored: [], errors: [`already landed in ${apply.value.landed.commit}; nothing to discard`] };
  const files = new Map<string, { before: string | null; after: string | null }>();
  for (const change of apply.value.changes) for (const f of change.files) files.set(f.path, { before: f.sha256Before, after: f.sha256After });
  for (const [rel, h] of files) {
    const actual = sha256File(toAbs(repo, rel));
    if (actual !== h.after) return { exitCode: 2, restored: [], errors: [`${rel} changed after apply; refusing to discard over unrelated edits`] };
  }
  if (!(await isRepoRoot({ run, repo })) && files.size > 0) return { exitCode: 2, restored: [], errors: ["not a git work tree; cannot restore files"] };
  const restored: string[] = [];
  for (const [rel, h] of files) {
    if (h.before === null) {
      fs.rmSync(toAbs(repo, rel), { force: true });
    } else {
      const result = await run(["git", "restore", "--worktree", "--source=HEAD", "--", rel], { cwd: repo, timeoutMs: 60_000 });
      if (result.code !== 0) return { exitCode: 2, restored, errors: [`git restore ${rel} failed: ${result.stderr.trim() || result.error}`] };
      if (sha256File(toAbs(repo, rel)) !== h.before) return { exitCode: 2, restored, errors: [`${rel} does not match its pre-apply digest after restore`] };
    }
    restored.push(rel);
  }
  fs.rmSync(path.join(repo, ...CANDIDATES_DIR.split("/")), { recursive: true, force: true });
  fs.rmSync(stateFile(repo, APPLY_REPORT), { force: true });
  fs.rmSync(stateFile(repo, GATE_REPORT), { force: true });
  return { exitCode: 0, restored, errors: [] };
}
