// The right-release source split (docs/GATE-CONTRACT.md "Commit-only build"; item 7 rows 7.03 to 7.05).
//
// Under the commit-only build the gate selects a commit, syncs it into a gate-owned snapshot and runs every stage from there. A
// right-release child the door starts for a stage (dev, test, stage-sidecar) therefore runs with its cwd inside the SNAPSHOT: its
// sources, its config and every command it spawns resolve there. What stays with the CHECKOUT is the git context the door run was
// made in (the run file, the primary-checkout rule, the commit's object database) and every output or cache right-release owns
// (`.right-release/...`).
//
// The split is never taken from a flag or from the child's own idea of the repository. The door writes it into its live run file
// (gate-stages.mjs startRun `source`) and names the checkout in RK_GATE_CHECKOUT; this module re-reads the run file, requires the
// run alive, and requires the process to sit inside the snapshot that run names. Anything inconsistent throws (fail closed): a
// child told it is split that cannot prove it never builds from where it stands.
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { CHECKOUT_ENV, readDoorRun } from "./gate-stages.mjs";

export { CHECKOUT_ENV };
export const SOURCE_SPLIT_CODE = "RK_SOURCE_SPLIT";
export const SOURCE_SPLIT_UNSUPPORTED = "RK_SOURCE_SPLIT_UNSUPPORTED";

export class SourceSplitError extends Error {
  constructor(message, code = SOURCE_SPLIT_CODE) { super(message); this.name = "SourceSplitError"; this.code = code; }
}

const real = (p) => { try { return fs.realpathSync.native(p); } catch { return path.resolve(p); } };
const fold = (p) => (process.platform === "win32" || process.platform === "darwin" ? p.toLowerCase() : p);
const contains = (parent, child) => { const rel = path.relative(fold(real(parent)), fold(real(child))); return rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel)); };

/**
 * The split this process runs under, or null when RK_GATE_CHECKOUT is not set (the checkout route: hosted CI, or no door).
 * Throws SourceSplitError when it is set and anything about it cannot be proven. `run` runs git (tests inject a double).
 */
export function readSourceSplit({ env = process.env, cwd = process.cwd(), run = spawnSync, readFile, alive } = {}) {
  const named = env[CHECKOUT_ENV];
  if (named === undefined || named === "") return null;
  if (!path.isAbsolute(named)) throw new SourceSplitError(`${CHECKOUT_ENV} must be an absolute path (got ${JSON.stringify(named)})`);
  const common = run("git", ["rev-parse", "--git-common-dir"], { cwd: named, encoding: "utf8", windowsHide: true });
  if (common.status !== 0 || !String(common.stdout).trim()) throw new SourceSplitError(`${CHECKOUT_ENV} ${named} is not a git checkout`);
  const commonDir = path.resolve(named, String(common.stdout).trim());
  const found = readDoorRun({ env, commonDir, ...(readFile ? { readFile } : {}), ...(alive ? { alive } : {}) });
  if (found.problem) throw new SourceSplitError(`no live door run ${found.problem === "ended" ? "(it has ended)" : "(RK_GATE_RUN names none)"} under ${named}; a source split exists only inside one`);
  const source = found.run.source;
  if (!source || typeof source !== "object") throw new SourceSplitError("the live door run carries no source split");
  for (const key of ["checkout", "snapshot"]) if (typeof source[key] !== "string" || !path.isAbsolute(source[key])) throw new SourceSplitError(`the door run's source.${key} is not an absolute path`);
  for (const key of ["commit", "tree"]) if (!/^[0-9a-f]{40}([0-9a-f]{24})?$/.test(source[key] ?? "")) throw new SourceSplitError(`the door run's source.${key} is not an object id`);
  if (typeof source.appDir !== "string" || source.appDir.startsWith("/") || source.appDir.split("/").includes("..") || source.appDir.includes("\\")) throw new SourceSplitError("the door run's source.appDir is not a contained relative path");
  if (real(source.checkout) !== real(named)) throw new SourceSplitError(`the door run was made in ${source.checkout}, not in ${named}`);
  if (found.run.commit !== source.commit) throw new SourceSplitError("the door run's commit and its source split's commit differ");
  const appRoot = source.appDir === "." ? source.snapshot : path.join(source.snapshot, ...source.appDir.split("/"));
  if (!contains(source.snapshot, cwd)) throw new SourceSplitError(`this process runs in ${cwd}, outside the snapshot ${source.snapshot} its door run names; a split child runs only inside the snapshot`);
  return Object.freeze({
    checkout: source.checkout, snapshot: source.snapshot, commit: source.commit, tree: source.tree, appDir: source.appDir, commonDir,
    appRoot, checkoutAppRoot: source.appDir === "." ? source.checkout : path.join(source.checkout, ...source.appDir.split("/")),
    stage: found.run.stage,
  });
}

/** Whether `file` lies inside the split's snapshot. */
export const inSnapshot = (split, file) => contains(split.snapshot, file);

/**
 * The git context a door-checked step needs: `{ commonDir, commit }` of the live run for a split process, else of the git
 * repository at `cwd` (HEAD), else null. door.mjs, web-build and the right-release CLI ask the same question, so they share it.
 */
export function doorContextFor({ cwd = process.cwd(), env = process.env, run = spawnSync } = {}) {
  const split = readSourceSplit({ env, cwd, run });
  if (split) return { commonDir: split.commonDir, commit: split.commit, split };
  const git = (args) => { const r = run("git", args, { cwd, encoding: "utf8" }); return r.status === 0 ? String(r.stdout).trim() : ""; };
  const common = git(["rev-parse", "--git-common-dir"]);
  if (!common) return null;
  return { commonDir: path.resolve(cwd, common), commit: git(["rev-parse", "HEAD"]), split: null };
}
