// Release source identity (item 7 row 7.04; signed/item7-sage-decisions-2.md Q2; docs/GATE-CONTRACT.md "Commit-only build").
//
// Every right-release command that builds, qualifies or ships a door stage asks one question before it trusts anything it reads: which
// source is this, and what proves it? There are exactly two answers.
//
//   checkout  RK_GATE_CHECKOUT is not set (hosted CI, no door, or the Mac/Dell route before the cutover). The source is the checkout's
//             HEAD, proven the way it always was: the primary worktree, a branch-attached HEAD and a clean status (R1). Nothing here
//             changes for that route.
//   snapshot  RK_GATE_CHECKOUT is set: this process is a child of a live door run (source-split.mjs `readSourceSplit`). The identity is
//             the SELECTED COMMIT's immutable objects plus the gate's start and end verification of the snapshot under held ownership.
//             It is never the checkout's HEAD, status or branch (GC:24 "no dirty-tree condition", GC:156 item 1, GC:182 E1): the
//             checkout may be dirty, ahead, behind or detached.
//
// A process that names a split it cannot prove (no live run, an ended run, another checkout, a cwd outside the snapshot) is refused
// (R2); it never falls back to the checkout checks, because that would build bytes the identity does not describe.
//
// `resolveReleaseSource` is the single owner. It returns a frozen record that only it can mint (release-source-brand.mjs), so the
// evidence, QA and upload code can refuse a hand-built one. The record's fields are frozen by the decision record:
//   { mode: "snapshot", commit, tree, checkout, checkoutAppRoot, snapshot, appRoot, commonDir }
//   { mode: "checkout", commit, tree, repoRoot, appRoot }
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { resolveSelectedCommit } from "./committed-source.mjs";
import { canonicalRepositoryIdentity } from "./ownership-approval.mjs";
import { assertPrimaryReleaseCheckout } from "./release-invocation.mjs";
import { dependencyClosure } from "./release-qa-receipt.mjs";
import { brandReleaseSource, isReleaseSource } from "./release-source-brand.mjs";
import { assertCleanSource } from "./source-gate.mjs";
import { SOURCE_SPLIT_CODE, SourceSplitError, inSnapshot, readSourceSplit } from "./source-split.mjs";

export const RELEASE_SOURCE_IDENTITY_CODE = "RK_SOURCE_IDENTITY";
export const NO_REPOSITORY_CODE = "RK_SOURCE_NO_REPOSITORY";

/** A release source whose identity does not hold (a commit or tree that is not the selected one, a file outside the snapshot). */
export class ReleaseSourceError extends Error {
  constructor(message, code = RELEASE_SOURCE_IDENTITY_CODE) { super(message); this.name = "ReleaseSourceError"; this.code = code; }
}

// What each resolved source was resolved with, so `assertReleaseSource` can prove the same facts again (a run that has ended since is no
// split any longer) without the caller threading the context through.
const contexts = new WeakMap();

const OBJECT_ID = /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/;

function plainGit(git, cwd, args) {
  const result = git("git", args, { cwd, encoding: "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024 });
  if (result?.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${String(result?.stderr ?? "").trim() || "no output"}`);
  return String(result.stdout);
}

/**
 * The source this process runs under. `git` runs git (a spawnSync-shaped function; tests inject a recorder). `readFile` and `alive` are
 * passed to the door-run reader (tests only). Throws SourceSplitError (R2, R3) when a split is named but cannot be proven; throws
 * an Error outside a git repository when no split is named.
 */
export function resolveReleaseSource({ cwd = process.cwd(), env = process.env, git = spawnSync, readFile, alive } = {}) {
  const context = Object.freeze({ cwd, env, git, ...(readFile ? { readFile } : {}), ...(alive ? { alive } : {}) });
  // 1. A live door run under the named checkout, the same checkout, and a cwd inside the run's snapshot. Throws when anything is off.
  const split = readSourceSplit({ env, cwd, run: git, ...(readFile ? { readFile } : {}), ...(alive ? { alive } : {}) });
  if (split) {
    // 2. The tree is re-derived from the object database. It is never taken from the run file alone (R3).
    let objects;
    try { objects = resolveSelectedCommit(split.checkout, split.commit, { runner: git }); }
    catch (error) { throw new SourceSplitError(`the door run's commit ${split.commit.slice(0, 12)} cannot be read from the object database of ${split.checkout}: ${error.message}`); }
    if (objects.commit !== split.commit) throw new SourceSplitError(`the object database of ${split.checkout} answers ${objects.commit} for the door run's commit ${split.commit}`);
    if (objects.tree !== split.tree) throw new SourceSplitError(`the door run records tree ${split.tree.slice(0, 12)} for commit ${split.commit.slice(0, 12)}, but the object database says ${objects.tree.slice(0, 12)} (R3)`);
    const source = brandReleaseSource(Object.freeze({
      mode: "snapshot", commit: objects.commit, tree: objects.tree, checkout: split.checkout, checkoutAppRoot: split.checkoutAppRoot,
      snapshot: split.snapshot, appRoot: split.appRoot, commonDir: split.commonDir,
    }));
    contexts.set(source, context);
    return source;
  }
  // The checkout route: HEAD of the repository at cwd, read exactly as each command always read it.
  let repoRoot = "";
  let detail = "";
  try { repoRoot = plainGit(git, cwd, ["rev-parse", "--show-toplevel"]).trim(); } catch (error) { detail = ` (${error.message})`; }
  if (!repoRoot) throw new ReleaseSourceError(`${cwd} is not inside a Git repository${detail}`, NO_REPOSITORY_CODE);
  const commit = plainGit(git, repoRoot, ["rev-parse", "HEAD"]).trim();
  const tree = plainGit(git, repoRoot, ["rev-parse", `${commit}^{tree}`]).trim();
  if (!OBJECT_ID.test(commit) || !OBJECT_ID.test(tree)) throw new Error("the checkout's HEAD is not a commit");
  const source = brandReleaseSource(Object.freeze({ mode: "checkout", commit, tree, repoRoot, appRoot: path.resolve(cwd) }));
  contexts.set(source, context);
  return source;
}

/** The record `resolveReleaseSource` minted, or a refusal: a hand-built look-alike never carries an identity. */
export function requireReleaseSource(source) {
  if (!isReleaseSource(source)) throw new SourceSplitError("the release source was not resolved by resolveReleaseSource; a hand-built source carries no identity");
  return source;
}

export const isSnapshotSource = (source) => requireReleaseSource(source).mode === "snapshot";
/** The root every git object read and every output, state, lock and sealed directory belongs to: the checkout (`.right-release` lives there). */
export const gitRootOf = (source) => (requireReleaseSource(source).mode === "snapshot" ? source.checkout : source.repoRoot);
/** The root every source, config, input and QA file is read from: the snapshot in a split, else the checkout. */
export const sourceRootOf = (source) => (requireReleaseSource(source).mode === "snapshot" ? source.snapshot : source.repoRoot);

/**
 * Proves the source's identity holds now, for the command `commandId`.
 *   checkout  R1, byte for byte as before: the primary worktree (branch-attached HEAD) and a clean status. `skipPrimaryCheckout` and
 *             `skipCleanCheck` are the two existing seams (a test double; a phase resume that reads no unrelated source).
 *   snapshot  The door run is proven again (live, this checkout, this cwd, the tree from the object database), then the checkout must
 *             still be the primary worktree. No status, no HEAD and no branch is read: a dirty or moved checkout is irrelevant.
 *             Writes into the snapshot are decided by the door's start and end verification, not here.
 * Returns the source.
 */
export function assertReleaseSource(source, { commandId = "release", env, git, skipPrimaryCheckout = false, skipCleanCheck = false } = {}) {
  requireReleaseSource(source);
  const context = contexts.get(source);
  const runGit = git ?? context.git;
  const environment = env ?? context.env;
  if (source.mode === "checkout") {
    if (!skipPrimaryCheckout) assertPrimaryReleaseCheckout(source.repoRoot, environment);
    if (!skipCleanCheck) assertCleanSource({ status: plainGit(runGit, source.repoRoot, ["status", "--porcelain=v1", "-z", "--untracked-files=all"]), commandId });
    return source;
  }
  const again = resolveReleaseSource({ ...context, env: environment, git: runGit });
  if (again.mode !== "snapshot") throw new SourceSplitError("the door run's source split is gone; nothing may continue from a snapshot that is no longer proven");
  for (const key of ["commit", "tree", "checkout", "snapshot", "appRoot", "checkoutAppRoot"]) {
    if (again[key] !== source[key]) throw new SourceSplitError(`the door run's source.${key} changed while ${commandId} ran`);
  }
  // The primary worktree identity stays (GC:31); only the branch-attached refusal, which reads HEAD, is skipped (GC:156).
  if (!skipPrimaryCheckout) assertPrimaryReleaseCheckout(source.checkout, environment, { split: true });
  return source;
}

/**
 * R5: in a split, a config, build input, QA plan or test file must resolve inside the snapshot (symlinks resolved). In the checkout
 * route there is no containment to prove and this does nothing.
 */
export function assertInsideSource(source, file, label = "a source file") {
  if (!isSnapshotSource(source)) return;
  if (typeof file !== "string" || !file) throw new ReleaseSourceError(`${label} has no path; a split build reads only the snapshot of ${source.commit.slice(0, 12)}`);
  if (!inSnapshot({ snapshot: source.snapshot }, path.resolve(file))) {
    throw new ReleaseSourceError(`${label} ${file} is outside the snapshot ${source.snapshot} of ${source.commit.slice(0, 12)}; a split build reads the commit's snapshot and never the checkout (R5)`);
  }
}

/**
 * R4 (and R8): a record that claims a source (a sealed manifest, ownership evidence, a resumed build's state) is for the selected
 * commit and, when it names a tree, that commit's tree. Another commit or tree is refused, never reinterpreted.
 */
export function assertBindsSource(source, record, label = "the record") {
  requireReleaseSource(source);
  if (!record || typeof record !== "object") throw new ReleaseSourceError(`${label} is missing, so it cannot be bound to commit ${source.commit.slice(0, 12)}`);
  if (record.commit !== source.commit) throw new ReleaseSourceError(`${label} is for commit ${String(record.commit ?? "(none)").slice(0, 12)}, not the selected commit ${source.commit.slice(0, 12)} (R4)`);
  if (record.tree !== undefined && record.tree !== source.tree) throw new ReleaseSourceError(`${label} is for tree ${String(record.tree).slice(0, 12)}, not the selected commit's tree ${source.tree.slice(0, 12)} (R4)`);
}

/**
 * The `{ commit, tree, repositoryIdentity }` a source QA receipt binds, for a snapshot source: the selected commit and tree, and the
 * repository identity from the checkout's origin. Reads no HEAD. (The checkout route keeps release-qa-receipt.mjs `sourceIdentity`.)
 */
export function snapshotSourceIdentity(source, git = spawnSync) {
  if (!isSnapshotSource(source)) throw new ReleaseSourceError("snapshotSourceIdentity is for a door source split");
  const origin = plainGit(git, source.checkout, ["remote", "get-url", "origin"]).trim();
  return { commit: source.commit, tree: source.tree, repositoryIdentity: canonicalRepositoryIdentity(origin) };
}

/**
 * The `{ source, closure }` options `admitSourceQa` / `assertSourceQaAdmission` / `assertInstalledQaAdmission` take so that the receipt they
 * verify is compared with the SELECTED commit (its tree, its repository identity, its lockfile closure read from its objects) instead of the
 * checkout's HEAD. `{}` for the checkout route: those functions then read HEAD exactly as before. Spread it into their options.
 */
export function qaAdmissionSourceOptions(source, git = spawnSync) {
  if (!isSnapshotSource(source)) return {};
  return { source: snapshotSourceIdentity(source, git), closure: dependencyClosure(source.checkout, source.commit, git) };
}

export { SOURCE_SPLIT_CODE };
