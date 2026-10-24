// `right-update apply`: rewrite pins to the latest allowed version.
//
// Safety shape: every row is a small transaction over a known file set. The files are
// snapshotted, edited, verified, and restored if anything fails. Breaking bumps and
// held rows are reported, never applied. Nothing here commits, pushes or publishes.

import fs from "node:fs";
import path from "node:path";
import { parse as parseToml } from "smol-toml";
import { cargoDependencies, defaultExec, checkSources, buildReport } from "./check.js";
import { decideBump, parseRequirement } from "./bump.js";
import { download, extractMember, DEFAULT_MAX_BYTES } from "./download.js";
import { runCommand, type RunFn } from "./exec.js";
import { dirtyPaths, headCommit, isRepoRoot } from "./git.js";
import { loadManifest } from "./manifest.js";
import {
  loadApplyReport,
  type ApplyChange,
  type ApplyReport,
  type FileChange,
  type ToolPlatformDigest,
  type ToolStage,
  type Unapplied,
} from "./reports.js";
import { replaceProjectYmlVersion, replaceSwiftPackageVersion } from "./swift-edit.js";
import {
  APPLY_REPORT,
  HELD_REPORT,
  candidateDir,
  ensureStateDir,
  hostPlatform,
  nowIso,
  sha256Bytes,
  sha256File,
  stateFile,
  writeJsonAtomic,
} from "./state.js";
import { replaceCargoVersion } from "./toml-edit.js";
import type { CheckDeps } from "./check.js";
import type { CheckEntry, Platform, ToolSource, UpdateManifest } from "./types.js";
import { packageVersion } from "./version.js";
import type { FetchLike } from "./http.js";

export interface ApplyDeps extends CheckDeps {
  fetch?: FetchLike;
  run?: RunFn;
  /** Override the RightKit platform id of this host (tests). */
  toolPlatform?: Platform | null;
  now?: () => Date;
}

export interface ApplyOptions {
  repo: string;
  only?: string[];
  dryRun?: boolean;
}

export interface ApplyOutcome {
  /** 0 done, 1 done with rolled-back rows, 2 refused or error. */
  exitCode: 0 | 1 | 2;
  report?: ApplyReport;
  reportPath?: string;
  errors: string[];
}

const LOCK_TIMEOUT_MS = 15 * 60 * 1000;
const PNPM_TIMEOUT_MS = 15 * 60 * 1000;

interface Ctx {
  repo: string;
  run: RunFn;
  fetch?: FetchLike;
  dryRun: boolean;
  platform: Platform | null;
  /** Repo-relative paths that were already modified before apply started. */
  dirtyBefore: Set<string>;
  /** Paths this run has changed so far. */
  touched: Set<string>;
  gitKnown: boolean;
}

type RowResult = { change: ApplyChange } | { failed: string } | { skipped: string };

const toRel = (repo: string, abs: string): string => path.relative(repo, abs).split(path.sep).join("/");
const toAbs = (repo: string, rel: string): string => path.join(repo, ...rel.split("/"));

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** Nearest ancestor directory (up to the repo root) that contains `fileName`, as a repo-relative file path. */
function findUp(repo: string, fromRelDir: string, fileName: string): string | null {
  let dir = fromRelDir === "." ? "" : fromRelDir;
  for (;;) {
    const rel = dir ? `${dir}/${fileName}` : fileName;
    if (fs.existsSync(toAbs(repo, rel))) return rel;
    if (!dir) return null;
    const next = path.posix.dirname(dir);
    dir = next === "." ? "" : next;
  }
}

class Transaction {
  private snapshot = new Map<string, Buffer | null>();
  constructor(private readonly ctx: Ctx, rels: string[]) {
    for (const rel of new Set(rels)) {
      const abs = toAbs(ctx.repo, rel);
      this.snapshot.set(rel, fs.existsSync(abs) ? fs.readFileSync(abs) : null);
    }
  }

  /** Paths that were dirty before this run and are not ours: editing them would mix changes into the commit. */
  conflicts(): string[] {
    return [...this.snapshot.keys()].filter((rel) => this.snapshot.get(rel) !== null && this.ctx.dirtyBefore.has(rel) && !this.ctx.touched.has(rel));
  }

  restore(): void {
    for (const [rel, content] of this.snapshot) {
      const abs = toAbs(this.ctx.repo, rel);
      if (content === null) fs.rmSync(abs, { force: true });
      else fs.writeFileSync(abs, content);
    }
  }

  changes(): FileChange[] {
    const out: FileChange[] = [];
    for (const [rel, content] of this.snapshot) {
      const abs = toAbs(this.ctx.repo, rel);
      const after = sha256File(abs);
      const before = content === null ? null : sha256Bytes(content);
      if (before !== after) out.push({ path: rel, sha256Before: before, sha256After: after });
    }
    return out;
  }
}

async function runChecked(ctx: Ctx, argv: string[], cwd: string, timeoutMs: number): Promise<void> {
  const result = await ctx.run(argv, { cwd, timeoutMs });
  if (result.code !== 0) {
    const why = result.error ?? (result.timedOut ? `timed out after ${Math.round(timeoutMs / 1000)}s` : result.stderr.trim().split("\n").slice(-6).join(" | ") || `exit ${result.code}`);
    throw new Error(`${argv.join(" ")} failed: ${why}`);
  }
}

// ---------------------------------------------------------------- cargo

function lockHasVersion(lockText: string, name: string, version: string): boolean {
  try {
    const doc = parseToml(lockText) as { package?: Array<{ name?: string; version?: string }> };
    return (doc.package ?? []).some((p) => p.name === name && p.version === version);
  } catch {
    return false;
  }
}

function lockedVersions(lockText: string, name: string): string[] {
  try {
    const doc = parseToml(lockText) as { package?: Array<{ name?: string; version?: string }> };
    return (doc.package ?? []).filter((p) => p.name === name && typeof p.version === "string").map((p) => p.version as string);
  } catch {
    return [];
  }
}

async function applyCargo(ctx: Ctx, entry: CheckEntry, newRequirement: string, full: string): Promise<RowResult> {
  const manifestRel = entry.source;
  const dirRel = path.posix.dirname(manifestRel);
  const lockRel = findUp(ctx.repo, dirRel, "Cargo.lock");
  // A standalone crate with no lock yet gets one next to its manifest.
  const files = [manifestRel, lockRel ?? (dirRel === "." ? "Cargo.lock" : `${dirRel}/Cargo.lock`)];
  const commands: string[][] = [];
  const notes: string[] = [];
  const argvFor = (pkg: string) => ["rustright", "cargo", "update", "-p", pkg, "--precise", full];
  commands.push(argvFor(entry.name));

  if (ctx.dryRun) {
    return {
      change: { kind: "cargo", name: entry.name, source: manifestRel, before: entry.pinned!, after: newRequirement, latest: entry.latest!, status: "planned", files: files.map((p) => ({ path: p, sha256Before: sha256File(toAbs(ctx.repo, p)), sha256After: null })), commands: lockRel ? commands : [], notes: lockRel ? notes : ["no Cargo.lock found; lockfile not refreshed"] },
    };
  }

  const tx = new Transaction(ctx, files);
  const conflicts = tx.conflicts();
  if (conflicts.length > 0) return { failed: `refusing to edit files with uncommitted changes: ${conflicts.join(", ")}` };
  try {
    const manifestAbs = toAbs(ctx.repo, manifestRel);
    const original = fs.readFileSync(manifestAbs, "utf8");
    const edited = replaceCargoVersion(original, entry.name, entry.pinned!, newRequirement);
    if (edited.count === 0) throw new Error(`could not locate version "${entry.pinned}" for ${entry.name} in ${manifestRel}`);
    verifyCargoEdit(original, edited.text, entry.name, entry.pinned!, newRequirement);
    fs.writeFileSync(manifestAbs, edited.text);

    const lockAlready = lockRel !== null && lockHasVersion(fs.readFileSync(toAbs(ctx.repo, lockRel), "utf8"), entry.name, full);
    if (lockAlready) {
      // An earlier row already moved the lockfile to this exact version.
      commands.length = 0;
      notes.push(`${lockRel} already locks ${entry.name} ${full}`);
    } else if (lockRel) {
      const cwd = path.dirname(manifestAbs);
      let result = await ctx.run(argvFor(entry.name), { cwd, timeoutMs: LOCK_TIMEOUT_MS });
      if (result.code !== 0 && /ambiguous/i.test(result.stderr)) {
        // More than one locked version of the crate: pick the one in the same compatibility line.
        const lockText = fs.readFileSync(toAbs(ctx.repo, lockRel), "utf8");
        const newMajor = full.split(".")[0];
        const candidates = lockedVersions(lockText, entry.name).filter((v) => v.split(".")[0] === newMajor);
        if (candidates.length === 1) {
          const spec = `${entry.name}@${candidates[0]}`;
          commands.push(argvFor(spec));
          result = await ctx.run(argvFor(spec), { cwd, timeoutMs: LOCK_TIMEOUT_MS });
        }
      }
      if (result.code !== 0) {
        const why = result.error ?? (result.timedOut ? "timed out" : result.stderr.trim().split("\n").slice(-6).join(" | ") || `exit ${result.code}`);
        throw new Error(`rustright cargo update failed: ${why}`);
      }
      const lockText = fs.readFileSync(toAbs(ctx.repo, lockRel), "utf8");
      if (!lockHasVersion(lockText, entry.name, full)) throw new Error(`${lockRel} does not contain ${entry.name} ${full} after the update`);
    } else {
      notes.push("no Cargo.lock found; lockfile not refreshed");
    }
    const changes = tx.changes();
    for (const f of changes) ctx.touched.add(f.path);
    return {
      change: { kind: "cargo", name: entry.name, source: manifestRel, before: entry.pinned!, after: newRequirement, latest: entry.latest!, status: "applied", files: changes, commands: lockRel ? commands : [], notes },
    };
  } catch (error) {
    tx.restore();
    return { failed: errorText(error) };
  }
}

/** The edited manifest must parse and differ from the original only in the targeted dependency versions. */
function verifyCargoEdit(before: string, after: string, name: string, oldSpec: string, newSpec: string): void {
  const a = cargoDependencies(parseToml(before) as Record<string, unknown>);
  const b = cargoDependencies(parseToml(after) as Record<string, unknown>);
  const expected = a.map((d) => (d.name === name && d.spec === oldSpec ? { ...d, spec: newSpec } : d));
  if (JSON.stringify(expected) !== JSON.stringify(b)) throw new Error("edited Cargo.toml does not match the expected dependency set");
  const beforeDoc = parseToml(before) as Record<string, unknown>;
  const afterDoc = parseToml(after) as Record<string, unknown>;
  if (JSON.stringify(Object.keys(beforeDoc)) !== JSON.stringify(Object.keys(afterDoc))) throw new Error("edited Cargo.toml changed its top-level tables");
}

// ---------------------------------------------------------------- pnpm

type PnpmSection = "dependencies" | "devDependencies" | "optionalDependencies";

function pnpmSectionOf(pkg: Record<string, unknown>, name: string): { section: PnpmSection; spec: string } | null {
  for (const section of ["dependencies", "devDependencies", "optionalDependencies"] as const) {
    const table = pkg[section];
    if (table && typeof table === "object" && typeof (table as Record<string, unknown>)[name] === "string") {
      return { section, spec: (table as Record<string, string>)[name] };
    }
  }
  return null;
}

function pnpmAddArgs(name: string, full: string, section: PnpmSection, prefix: string, isWorkspaceRoot: boolean): string[] {
  const args = ["pnpm", "add"];
  if (isWorkspaceRoot) args.push("-w");
  if (section === "devDependencies") args.push("-D");
  if (section === "optionalDependencies") args.push("-O");
  args.push(prefix === "" ? "--save-exact" : `--save-prefix=${prefix}`);
  // A new dependency version must not run lifecycle scripts unattended.
  args.push("--ignore-scripts", `${name}@${full}`);
  return args;
}

async function applyPnpm(ctx: Ctx, entry: CheckEntry, newRequirement: string, full: string): Promise<RowResult> {
  const manifestRel = entry.source;
  const dirRel = path.posix.dirname(manifestRel);
  const dirAbs = toAbs(ctx.repo, dirRel === "." ? "" : dirRel);
  const manifestAbs = toAbs(ctx.repo, manifestRel);
  let pkg: Record<string, unknown>;
  try {
    pkg = JSON.parse(fs.readFileSync(manifestAbs, "utf8")) as Record<string, unknown>;
  } catch (error) {
    return { failed: `cannot read ${manifestRel}: ${errorText(error)}` };
  }
  const found = pnpmSectionOf(pkg, entry.name);
  if (!found || found.spec !== entry.pinned) return { failed: `${entry.name} is no longer pinned at ${entry.pinned} in ${manifestRel}` };
  const req = parseRequirement(found.spec);
  if (!req || !["", "^", "~"].includes(req.prefix)) {
    return { skipped: `pnpm add cannot express the ${JSON.stringify(req?.prefix ?? found.spec)} requirement style` };
  }
  const isWorkspaceRoot = fs.existsSync(path.join(dirAbs, "pnpm-workspace.yaml"));
  const argv = pnpmAddArgs(entry.name, full, found.section, req.prefix, isWorkspaceRoot);
  const lockRel = findUp(ctx.repo, dirRel, "pnpm-lock.yaml");
  const workspaceRel = findUp(ctx.repo, dirRel, "pnpm-workspace.yaml");
  const localLock = dirRel === "." ? "pnpm-lock.yaml" : `${dirRel}/pnpm-lock.yaml`;
  const files = [...new Set([manifestRel, lockRel ?? localLock, localLock, ...(workspaceRel ? [workspaceRel] : [])])];

  if (ctx.dryRun) {
    return {
      change: { kind: "pnpm", name: entry.name, source: manifestRel, before: entry.pinned!, after: newRequirement, latest: entry.latest!, status: "planned", files: files.map((p) => ({ path: p, sha256Before: sha256File(toAbs(ctx.repo, p)), sha256After: null })), commands: [argv], notes: [] },
    };
  }

  const tx = new Transaction(ctx, files);
  const conflicts = tx.conflicts();
  if (conflicts.length > 0) return { failed: `refusing to edit files with uncommitted changes: ${conflicts.join(", ")}` };
  try {
    await runChecked(ctx, argv, dirAbs, PNPM_TIMEOUT_MS);
    const after = JSON.parse(fs.readFileSync(manifestAbs, "utf8")) as Record<string, unknown>;
    const now = pnpmSectionOf(after, entry.name);
    if (!now || now.section !== found.section || now.spec !== newRequirement) {
      throw new Error(`pnpm wrote ${now ? `${now.section}.${entry.name} = ${now.spec}` : "no entry"}, expected ${found.section}.${entry.name} = ${newRequirement}`);
    }
    const changes = tx.changes();
    for (const f of changes) ctx.touched.add(f.path);
    return {
      change: { kind: "pnpm", name: entry.name, source: manifestRel, before: entry.pinned!, after: newRequirement, latest: entry.latest!, status: "applied", files: changes, commands: [argv], notes: [] },
    };
  } catch (error) {
    tx.restore();
    return { failed: errorText(error) };
  }
}

// ---------------------------------------------------------------- swiftpm

async function applySwiftpm(ctx: Ctx, entry: CheckEntry, newRequirement: string): Promise<RowResult> {
  const manifestRel = entry.source;
  const dirRel = path.posix.dirname(manifestRel);
  const sibling = path.posix.basename(manifestRel) === "Package.swift" ? "project.yml" : "Package.swift";
  const siblingRel = dirRel === "." ? sibling : `${dirRel}/${sibling}`;
  const rels = [manifestRel, ...(fs.existsSync(toAbs(ctx.repo, siblingRel)) ? [siblingRel] : [])];
  const resolvedRel = findUp(ctx.repo, dirRel, "Package.resolved");
  const notes = resolvedRel ? [`${resolvedRel} was not refreshed; run swift package resolve in a build lane`] : [];

  const edit = (rel: string, text: string) =>
    path.posix.basename(rel) === "Package.swift"
      ? replaceSwiftPackageVersion(text, entry.name, entry.pinned!, newRequirement)
      : replaceProjectYmlVersion(text, entry.name, entry.pinned!, newRequirement);

  if (ctx.dryRun) {
    return {
      change: { kind: "swiftpm", name: entry.name, source: manifestRel, before: entry.pinned!, after: newRequirement, latest: entry.latest!, status: "planned", files: rels.map((p) => ({ path: p, sha256Before: sha256File(toAbs(ctx.repo, p)), sha256After: null })), commands: [], notes },
    };
  }

  const tx = new Transaction(ctx, rels);
  const conflicts = tx.conflicts();
  if (conflicts.length > 0) return { failed: `refusing to edit files with uncommitted changes: ${conflicts.join(", ")}` };
  try {
    let total = 0;
    for (const rel of rels) {
      const abs = toAbs(ctx.repo, rel);
      const result = edit(rel, fs.readFileSync(abs, "utf8"));
      if (result.count > 0) fs.writeFileSync(abs, result.text);
      if (rel === manifestRel && result.count === 0) throw new Error(`could not locate ${entry.name} requirement "${entry.pinned}" in ${manifestRel}`);
      total += result.count;
    }
    if (total === 0) throw new Error(`no edits made for ${entry.name}`);
    const changes = tx.changes();
    for (const f of changes) ctx.touched.add(f.path);
    return {
      change: { kind: "swiftpm", name: entry.name, source: manifestRel, before: entry.pinned!, after: newRequirement, latest: entry.latest!, status: "applied", files: changes, commands: [], notes },
    };
  } catch (error) {
    tx.restore();
    return { failed: errorText(error) };
  }
}

// ---------------------------------------------------------------- tool

const PIN_DIR_RE = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$/;

function assetUrl(source: ToolSource, tag: string, asset: string): string {
  if (source.upstream.type === "github-release") {
    return `https://github.com/${source.upstream.repo}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(asset)}`;
  }
  // `url` upstreams carry one download template; `{asset}` is optional and names the platform asset.
  return source.upstream.download.split("{version}").join(tag).split("{asset}").join(asset);
}

async function applyTool(ctx: Ctx, source: ToolSource, entry: CheckEntry): Promise<RowResult> {
  const platform = ctx.platform;
  if (!platform) return { skipped: `host platform ${process.platform}-${process.arch} is not a RightKit platform` };
  const asset = source.assets[platform];
  if (!asset) return { skipped: `tool ${source.name} has no asset for host platform ${platform}` };
  const tag = entry.latest!;
  const newPin = source.pin.startsWith("v") ? (tag.startsWith("v") ? tag : `v${tag}`) : tag.replace(/^v/, "");
  if (!PIN_DIR_RE.test(newPin)) return { failed: `latest version ${JSON.stringify(newPin)} is not a safe directory name` };

  const maxFor = (a: { maxBytes?: number }) => a.maxBytes ?? DEFAULT_MAX_BYTES;
  const hostUrl = assetUrl(source, tag, asset.asset);
  const candRel = toRel(ctx.repo, candidateDir(ctx.repo, source, newPin));
  const notes = ["no upstream-published checksum was verified; digests are observed from the download and are not trusted until a journey passes"];

  if (ctx.dryRun) {
    return {
      change: { kind: "tool", name: source.name, source: entry.source, before: source.pin, after: newPin, latest: tag, status: "planned", files: [], commands: [], notes: [`would download ${hostUrl} for ${platform}`, ...notes] },
    };
  }

  const toolRoot = path.join(ctx.repo, ...candRel.split("/").slice(0, -1));
  const candAbs = toAbs(ctx.repo, candRel);
  try {
    // Fresh staging area for this tool: older candidates are never reused.
    fs.rmSync(toolRoot, { recursive: true, force: true });
    fs.mkdirSync(candAbs, { recursive: true });
    const assetPath = path.join(candAbs, asset.asset);
    const got = await download(hostUrl, assetPath, { fetch: ctx.fetch, maxBytes: maxFor(asset) });
    let binary = asset.asset;
    let binarySha: string | undefined;
    if (asset.extract) {
      const extracted = await extractMember(assetPath, candAbs, asset.extract.member, ctx.run);
      binary = toRel(candAbs, extracted);
      binarySha = sha256File(extracted) ?? undefined;
      fs.chmodSync(extracted, 0o755);
    } else {
      fs.chmodSync(assetPath, 0o755);
    }

    // Other platforms are hashed without being stored, so the manifest checksums stay consistent per pin.
    const others: ToolPlatformDigest[] = [];
    for (const [key, other] of Object.entries(source.assets) as Array<[Platform, NonNullable<ToolSource["assets"][Platform]>]>) {
      if (key === platform) continue;
      const digest = await download(assetUrl(source, tag, other.asset), null, { fetch: ctx.fetch, maxBytes: maxFor(other) });
      others.push({ platform: key, asset: other.asset, size: digest.size, sha256: digest.sha256, trust: "observed", previousChecksum: other.checksum });
    }

    const stage: ToolStage = {
      platform,
      tag,
      version: newPin,
      asset: asset.asset,
      sourceUrl: hostUrl,
      candidateDir: candRel,
      binary,
      size: got.size,
      sha256: got.sha256,
      trust: "observed",
      previousChecksum: asset.checksum,
      ...(binarySha ? { binarySha256: binarySha } : {}),
      otherPlatforms: others,
    };
    return {
      change: { kind: "tool", name: source.name, source: entry.source, before: source.pin, after: newPin, latest: tag, status: "staged", files: [], commands: [], notes, tool: stage },
    };
  } catch (error) {
    fs.rmSync(toolRoot, { recursive: true, force: true });
    return { failed: errorText(error) };
  }
}

// ---------------------------------------------------------------- driver

function unapplied(entry: CheckEntry, reason: string): Unapplied {
  return { kind: entry.kind, name: entry.name, source: entry.source, before: entry.pinned, latest: entry.latest, reason, policy: entry.policy };
}

/** Write `held.json`: rows an agent lane or a person must handle. Never committed. */
export function writeHeldReport(repo: string, report: ApplyReport): string {
  const file = stateFile(repo, HELD_REPORT);
  writeJsonAtomic(file, {
    schemaVersion: 1,
    repo,
    createdAt: nowIso(),
    needsMigration: report.needsMigration,
    held: report.held,
    skipped: report.skipped,
    failed: report.failed,
  });
  return file;
}

export async function runApply(options: ApplyOptions, deps: ApplyDeps = {}): Promise<ApplyOutcome> {
  const repo = path.resolve(options.repo);
  const run = deps.run ?? runCommand;
  const loaded = loadManifest(repo);
  if (!loaded.valid || !loaded.manifest) return { exitCode: 2, errors: loaded.errors };
  const manifest: UpdateManifest = loaded.manifest;
  const dryRun = options.dryRun === true;
  const reportFile = stateFile(repo, APPLY_REPORT);

  // A previous apply that was never landed still owns its files; do not stack another run on top.
  if (!dryRun && fs.existsSync(reportFile)) {
    const previous = loadApplyReport(reportFile);
    if (previous.ok && !previous.value.dryRun && !previous.value.landed && previous.value.changes.length > 0) {
      return {
        exitCode: 2,
        errors: [`a previous apply (${previous.value.createdAt}) has ${previous.value.changes.length} change(s) that are not landed; run \`right-update land\` or \`right-update discard\` first`],
      };
    }
  }

  const check = buildReport(repo, await checkSources(repo, manifest, { fetch: deps.fetch, exec: deps.exec ?? defaultExec, drift: deps.drift }));
  const only = options.only && options.only.length > 0 ? new Set(options.only) : null;
  const rows = check.entries.filter((e) => !only || only.has(e.name));
  if (only) {
    const missing = [...only].filter((n) => !check.entries.some((e) => e.name === n));
    if (missing.length > 0) return { exitCode: 2, errors: [`--only names not found in the manifest: ${missing.join(", ")}`] };
  }
  const lookupErrors = rows.filter((e) => e.error !== undefined).map((e) => `${e.kind}:${e.name}: ${e.error}`);
  if (lookupErrors.length > 0) {
    return { exitCode: 2, errors: ["lookup errors; apply changes nothing until check is clean (use --only to scope)", ...lookupErrors] };
  }

  const git = { run, repo };
  const gitKnown = await isRepoRoot(git);
  let dirtyBefore = new Set<string>();
  if (gitKnown) {
    const dirty = await dirtyPaths(git);
    if (!dirty.ok) return { exitCode: 2, errors: [`cannot read git status: ${dirty.error}`] };
    dirtyBefore = new Set(dirty.paths);
  }
  const ctx: Ctx = {
    repo,
    run,
    fetch: deps.fetch,
    dryRun,
    platform: deps.toolPlatform !== undefined ? deps.toolPlatform : hostPlatform(),
    dirtyBefore,
    touched: new Set(),
    gitKnown,
  };

  const report: ApplyReport = {
    schemaVersion: 1,
    tool: "right-update",
    version: packageVersion(),
    repo,
    createdAt: (deps.now?.() ?? new Date()).toISOString(),
    dryRun,
    baseCommit: gitKnown ? await headCommit(git) : null,
    changes: [],
    needsMigration: [],
    held: [],
    skipped: [],
    failed: [],
    landed: null,
  };

  if (!dryRun) ensureStateDir(repo);

  const toolSources = new Map(manifest.sources.filter((s): s is ToolSource => s.kind === "tool").map((s) => [s.name, s]));
  for (const entry of rows) {
    if (entry.status !== "outdated") continue;
    if (entry.policy === "held") {
      report.held.push(unapplied(entry, "policy: held"));
      continue;
    }
    if (entry.policy === "agent-pr") {
      report.needsMigration.push(unapplied(entry, "policy: agent-pr (migration by an agent lane)"));
      continue;
    }
    if (entry.kind === "rightkit") {
      report.skipped.push(unapplied(entry, "rightkit sources are updated through right-release, not right-update"));
      continue;
    }

    let result: RowResult;
    if (entry.kind === "tool") {
      const source = toolSources.get(entry.name);
      result = source ? await applyTool(ctx, source, entry) : { skipped: "tool source not found in the manifest" };
    } else {
      const decision = decideBump(entry.pinned ?? "", entry.latest ?? "");
      if (decision.action === "needs-migration") {
        report.needsMigration.push(unapplied(entry, decision.reason!));
        continue;
      }
      if (decision.action === "skip") {
        report.skipped.push(unapplied(entry, decision.reason!));
        continue;
      }
      if (decision.newRequirement === entry.pinned) {
        report.skipped.push(unapplied(entry, "new requirement equals the current one"));
        continue;
      }
      if (entry.kind === "cargo") result = await applyCargo(ctx, entry, decision.newRequirement!, decision.fullVersion!);
      else if (entry.kind === "pnpm") result = await applyPnpm(ctx, entry, decision.newRequirement!, decision.fullVersion!);
      else result = await applySwiftpm(ctx, entry, decision.newRequirement!);
    }

    if ("change" in result) report.changes.push(result.change);
    else if ("failed" in result) report.failed.push(unapplied(entry, result.failed));
    else report.skipped.push(unapplied(entry, result.skipped));
  }

  let reportPath: string | undefined;
  if (!dryRun) {
    reportPath = reportFile;
    writeJsonAtomic(reportFile, report);
    writeHeldReport(repo, report);
  }
  return { exitCode: report.failed.length > 0 ? 1 : 0, report, reportPath, errors: [] };
}
