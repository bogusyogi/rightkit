// Ownership gate for the STAGED tree (index content), never the working tree: the manual command `right-release ownership --staged`.
// No git hook runs it any more (docs/GATE-CONTRACT.md: commit runs no local hook check).
// Authoritative enforcement stays in broker admission and the release gate.
//
//   right-release ownership --staged [--app <dir>] [--json]
//
// Block-tier findings without a valid exception fail the gate. Advise-tier findings print a
// summary and pass. A new Cargo.toml/package.json whose name or description matches a catalog
// capability keyword is advised unless it declares that capability's owner package.
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parse as parseToml } from "smol-toml";
import { spawnSync } from "./background-process.mjs";
import { checkApp, MANIFEST_FILE } from "./ownership.mjs";
import { isBlockingFinding } from "./ownership-verdict.mjs";

const releaseRoot = path.dirname(fileURLToPath(import.meta.url));
export const DEFAULT_MANIFEST = path.join(releaseRoot, MANIFEST_FILE);
const NEW_MANIFEST_NAMES = new Set(["Cargo.toml", "package.json"]);

const cleanEnv = () => Object.fromEntries(Object.entries(process.env).filter(([key]) => !/^GIT_/i.test(key)));

function git(cwd, args, { env = cleanEnv(), encoding = "buffer" } = {}) {
  const result = spawnSync("git", ["-C", cwd, ...args], { encoding, env, maxBuffer: 512 * 1024 * 1024, windowsHide: true });
  if (result?.error || result?.status !== 0) {
    throw new Error(`git ${args[0]} failed${result?.stderr ? `: ${String(result.stderr).trim().slice(0, 200)}` : ""}`);
  }
  return result.stdout;
}

/** Repository top-level for a directory, or a thrown error when it is not a Git worktree. */
export function repositoryRootFor(dir) {
  return realpathSync(String(git(path.resolve(dir), ["rev-parse", "--show-toplevel"], { encoding: "utf8" })).trim());
}

/** Staged index path for this repository; honors GIT_INDEX_FILE so pre-commit hooks see the real index. */
function stagedIndex(repoRoot) {
  if (process.env.GIT_INDEX_FILE) return path.resolve(repoRoot, process.env.GIT_INDEX_FILE);
  return path.resolve(repoRoot, String(git(repoRoot, ["rev-parse", "--git-path", "index"], { encoding: "utf8" })).trim());
}

function stagedEnv(repoRoot) {
  return { ...cleanEnv(), GIT_INDEX_FILE: stagedIndex(repoRoot) };
}

/**
 * Materialize the staged index into a throwaway worktree. The temporary repository borrows the
 * real object store through alternates, so blobs are the exact staged bytes. The real HEAD,
 * index, and working tree are never written.
 */
export function materializeStagedTree(repoRoot) {
  const root = realpathSync(repoRoot);
  const workdir = mkdtempSync(path.join(os.tmpdir(), "rightkit-staged-"));
  const tree = path.join(workdir, "tree");
  try {
    mkdirSync(tree);
    const init = spawnSync("git", ["init", "-q", tree], { env: cleanEnv(), windowsHide: true, encoding: "utf8" });
    if (init.status !== 0) throw new Error("git init failed for staged scan");
    const objects = path.resolve(root, String(git(root, ["rev-parse", "--git-path", "objects"], { encoding: "utf8" })).trim());
    writeFileSync(path.join(tree, ".git", "objects", "info", "alternates"), `${objects}\n`);
    copyFileSync(stagedIndex(root), path.join(tree, ".git", "index"));
    const origin = spawnSync("git", ["-C", root, "config", "--get", "remote.origin.url"], { env: cleanEnv(), windowsHide: true, encoding: "utf8" });
    if (origin.status === 0 && origin.stdout.trim()) git(tree, ["config", "remote.origin.url", origin.stdout.trim()], { encoding: "utf8" });
    git(tree, ["-c", "core.autocrlf=false", "-c", "core.eol=lf", "checkout-index", "-a", "-f"], { encoding: "utf8" });
    return { tree, cleanup: () => rmSync(workdir, { recursive: true, force: true }) };
  } catch (error) {
    rmSync(workdir, { recursive: true, force: true });
    throw error;
  }
}

const escapeRegExp = (value) => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** Keyword index from the catalog: each capability id matches its hyphen, space, or underscore spellings. */
export function capabilityKeywords(manifest) {
  const byCapability = new Map();
  for (const entry of manifest.capabilities ?? []) {
    if (!byCapability.has(entry.capability)) byCapability.set(entry.capability, new Set());
    byCapability.get(entry.capability).add(entry.package);
  }
  return [...byCapability].map(([capability, owners]) => ({
    capability,
    owners: [...owners],
    pattern: new RegExp(`(?<![a-z0-9])${escapeRegExp(capability).replace(/-/g, "[-_ ]")}(?![a-z0-9])`, "i"),
  }));
}

function manifestIdentity(file, bytes) {
  const text = bytes.toString("utf8");
  if (path.posix.basename(file) === "Cargo.toml") {
    const parsed = parseToml(text);
    const dependencyTables = [parsed.dependencies, parsed["dev-dependencies"], parsed["build-dependencies"], parsed.workspace?.dependencies];
    for (const target of Object.values(parsed.target ?? {})) dependencyTables.push(target.dependencies, target["dev-dependencies"], target["build-dependencies"]);
    const declared = new Set();
    for (const table of dependencyTables) for (const [key, value] of Object.entries(table ?? {})) {
      declared.add(key);
      if (value && typeof value === "object" && typeof value.package === "string") declared.add(value.package);
    }
    return { name: parsed.package?.name ?? "", description: parsed.package?.description ?? "", declared };
  }
  const parsed = JSON.parse(text);
  const declared = new Set();
  for (const field of ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"]) for (const key of Object.keys(parsed[field] ?? {})) declared.add(key);
  return { name: parsed.name ?? "", description: parsed.description ?? "", declared };
}

/** Advisory: new staged manifests whose name/description mention a capability without declaring its owner. */
export function newManifestAdvisories(repoRoot, manifest, { env = stagedEnv(repoRoot) } = {}) {
  const added = git(repoRoot, ["diff", "--cached", "--name-only", "--diff-filter=A", "-z"], { env })
    .toString("utf8").split("\0").filter(Boolean)
    .filter((file) => NEW_MANIFEST_NAMES.has(path.posix.basename(file)));
  const keywords = capabilityKeywords(manifest);
  const advisories = [];
  for (const file of added) {
    let identity;
    try { identity = manifestIdentity(file, git(repoRoot, ["show", `:${file}`], { env })); }
    catch (error) { advisories.push({ file, capability: null, message: `could not parse staged manifest: ${error.message}` }); continue; }
    const text = `${identity.name}\n${identity.description}`;
    for (const { capability, owners, pattern } of keywords) {
      if (!pattern.test(text)) continue;
      if (owners.includes(identity.name)) continue;
      if (owners.some((owner) => identity.declared.has(owner))) continue;
      advisories.push({ file, capability, owners, message: `new manifest ${file} matches capability "${capability}" without declaring its owner (${owners.join(", ")}); use the RightKit owner or declare it` });
    }
  }
  return advisories;
}

/**
 * Scan the staged tree. `scan` and `trust` are injectable for fixtures; production loads the
 * protected trust registry through the scanner itself.
 */
export function scanStaged(repoRoot, { manifest, scan = checkApp, trust, materialize = materializeStagedTree } = {}) {
  const root = realpathSync(repoRoot);
  const resolvedManifest = manifest ?? JSON.parse(readFileSync(DEFAULT_MANIFEST, "utf8"));
  const staged = materialize(root);
  let result;
  // Hooks export GIT_DIR/GIT_INDEX_FILE; the scanner's own Git calls must address the staged copy only.
  const saved = Object.fromEntries(Object.entries(process.env).filter(([key]) => /^GIT_/i.test(key)));
  for (const key of Object.keys(saved)) delete process.env[key];
  try {
    result = scan(staged.tree, resolvedManifest, trust === undefined ? {} : { trust });
  } finally {
    Object.assign(process.env, saved);
    staged.cleanup();
  }
  const blocking = result.violations.filter(isBlockingFinding);
  const advisory = result.violations.filter((finding) => !isBlockingFinding(finding));
  const keywords = newManifestAdvisories(root, resolvedManifest);
  return {
    ok: blocking.length === 0,
    blocking,
    advisory,
    keywords,
    problems: result.problems ?? [],
    warnings: result.warnings ?? [],
    coverage: result.coverage ?? [],
    enrollmentPending: result.enrollmentPending === true,
    covered: result.covered?.length ?? 0,
  };
}

export function formatStagedReport(result) {
  const lines = [];
  for (const finding of result.blocking) lines.push(`  BLOCK ${finding.fingerprint} (${finding.package}) ${finding.file}${finding.line ? `:${finding.line}` : ""}  ${finding.text ?? ""}`);
  for (const finding of result.advisory) lines.push(`  advise ${finding.fingerprint} (${finding.package}) ${finding.file}${finding.line ? `:${finding.line}` : ""}`);
  for (const item of result.keywords) lines.push(`  advise ${item.message}`);
  for (const problem of result.problems) lines.push(`  note ${problem}`);
  for (const warning of result.warnings) lines.push(`  note ${warning}`);
  if (result.coverage.length) lines.push(`  note ${result.coverage.length} staged path(s) unscanned by the gate; broker admission remains authoritative`);
  const summary = `[right-release] staged ownership: ${result.blocking.length} block, ${result.advisory.length} advise, ${result.keywords.length} new-manifest advisory, ${result.covered} excepted`;
  lines.push(result.ok ? `${summary}; passed` : `${summary}; FAILED: block-tier findings need an owner integration or a valid exception`);
  return lines.join("\n");
}

/** CLI entry used by `right-release ownership --staged`. Returns the process exit code. */
export function main(args, { print = (line) => console.log(line), scanner = scanStaged } = {}) {
  const flag = (name) => { const i = args.indexOf(name); return i >= 0 ? args[i + 1] : undefined; };
  let repoRoot;
  try { repoRoot = repositoryRootFor(flag("--app") ?? process.cwd()); }
  catch { print("[right-release] ownership --staged FAILED: not inside a Git repository"); return 1; }
  let result;
  try { result = scanner(repoRoot); }
  catch (error) {
    if (args.includes("--json")) print(JSON.stringify({ ok: false, error: { type: "staged-scan", message: error.message } }, null, 2));
    else print(`[right-release] ownership --staged FAILED: ${error.message}`);
    return 1;
  }
  if (args.includes("--json")) print(JSON.stringify({ ok: result.ok, blocking: result.blocking, advisory: result.advisory, keywords: result.keywords, problems: result.problems, coverage: result.coverage, enrollmentPending: result.enrollmentPending }, null, 2));
  else print(formatStagedReport(result));
  return result.ok ? 0 : 1;
}
