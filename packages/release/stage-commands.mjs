// What a unit's compile and package broker jobs must have run, derived from the code that builds those commands (local-publish.mjs
// localTestPlan and the pack argv builders), so the gate (which chooses the receipts to carry) and the hosted admission (which checks
// the signed requestedArgv of each) share one definition and cannot drift from the stages. EVERY command of a stage must be covered by
// a signed receipt of its own job; a missing command is a refusal that names it.
import fs from "node:fs";
import path from "node:path";
import { CARGO_METADATA_ARGS, cargoPackageArgs, localTestPlan, npmPackArgs, qualifyFmtArgs } from "./local-publish.mjs";
import { SWAP_HELPER_CARGO_ARGS, SWAP_HELPER_METADATA_ARGS, SWAP_HELPER_PACKAGE } from "./swap-helper-bundle.mjs";
import { carriedRequestOf, lockedArgv, splitCarriedArgs } from "./snapshot-request.mjs";
import { directCargoShape } from "./cargo-target-shim.mjs";

/** Stands for the one argument that is a fresh absolute scratch path (`pnpm pack --pack-destination <mkdtemp>`). */
export const ANY_ABSOLUTE_PATH = Object.freeze({ anyAbsolutePath: true });
const PNPM = (...argvs) => ({ kind: "adapter", adapter: "rightkit-pnpm", argvs });
/**
 * A cargo command as a broker host receives it. Every Cargo call of a snapshot stage is carried (carried-cargo.mjs): `--manifest-path` is
 * not sent (the call runs from the manifest's directory) and a build or run is sent with `--locked`, so the signed `requestedArgv` is
 * that argv and the signed `workspace` is that directory of the checkout, never the spelling the stage's code wrote. `cwd` is the
 * directory the stage runs the command from, relative to the repository root; the row keeps the directory the call runs in.
 */
const CARGO = (cwd, ...argvs) => {
  const carried = argvs.map((argv) => carriedRequestOf(argv, cwd) ?? { argv, cwd });
  if (new Set(carried.map((c) => c.cwd)).size !== 1) throw new Error(`the spellings of one cargo command run in different directories: ${argvs.map((a) => a.join(" ")).join(" | ")}`);
  // GC-A11: a command of the direct shape (directCargoShape, the one list carried-cargo.mjs runs by) compiles nothing and runs directly,
  // so it makes no broker job and no signed receipt: the row is `direct`, the stage owes its recorded direct ledger entry instead.
  const direct = carried.every((c) => directCargoShape(c.argv));
  return { kind: "cargo", argvs: carried.map((c) => c.argv), cwd: carried[0].cwd, ...(direct ? { direct: true } : {}) };
};

/** Readable name of an expected command (scratch paths shown as <scratch>). */
export const describeCommand = (row) => `${row.kind === "adapter" ? "pnpm" : "cargo"} ${row.argvs[0].map((a) => (typeof a === "string" ? a : "<scratch>")).join(" ")}`;

/**
 * The commands of the compile and package stages of one unit, one { kind, adapter?, argvs } row per job the stage launches
 * (`argvs` are the accepted spellings of that one command: cargo metadata retries without --offline). Inputs are what the dispatched
 * commit holds: `config` = ci/gate.config.json, `manifest` = the npm package's package.json.
 */
export function expectedStageCommands({ kind, unit, config, manifest, platform }) {
  const dir = kind === "npm" ? `packages/${unit.slice("@rightkit/".length)}` : `crates/${unit}`;
  const plan = localTestPlan({ kind, unit, dir, config, manifest, batch: false, platform });
  // The stage's own working directories (local-publish.mjs): a crate's cargo steps run in its workspace; a consumer row's (named by
  // --manifest-path from the repository root) and the swap helper's run from the repository root.
  const crateDir = `crates/${unit}`;
  const compile = plan.steps.map((step) => (step.command === "pnpm" ? PNPM(step.args) : CARGO(step.args.includes("--manifest-path") ? "." : crateDir, step.args)));
  const pack = [];
  if (kind === "npm") {
    // The swap helper builds and signs a binary into its own directory of the snapshot (a declared stage output), then packs from there like any unit.
    // A Linux direct origin builds none: the helper is built and signed hosted (publish.yml build-swap/sign-swap).
    if (unit === SWAP_HELPER_PACKAGE && platform !== "linux") pack.push(CARGO(".", SWAP_HELPER_CARGO_ARGS), CARGO(".", SWAP_HELPER_METADATA_ARGS));
    pack.push(PNPM(npmPackArgs(dir, ANY_ABSOLUTE_PATH)));
  } else {
    compile.unshift(CARGO(crateDir, qualifyFmtArgs(unit)));
    pack.push(CARGO(crateDir, cargoPackageArgs(unit, true)), CARGO(crateDir, [...CARGO_METADATA_ARGS], CARGO_METADATA_ARGS.filter((arg) => arg !== "--offline")));
  }
  return { compile, package: pack };
}

const argvMatches = (expected, actual) => Array.isArray(actual) && expected.length === actual.length
  && expected.every((want, i) => (want === ANY_ABSOLUTE_PATH ? typeof actual[i] === "string" && (path.posix.isAbsolute(actual[i]) || /^[A-Za-z]:[\\/]/.test(actual[i])) : want === actual[i]));

const posixOf = (p) => String(p ?? "").replaceAll("\\", "/").replace(/\/+$/, "");
const realOf = (p) => { try { return fs.realpathSync.native(p); } catch { return path.resolve(p); } };

/**
 * The repository root a job ran under, given the directory its row runs in: the signed `workspace` of a carried cargo call is that
 * directory of the checkout (`crates/<x>`), so the root is the workspace with the row's `cwd` removed. Null when the workspace is not that
 * directory. A row with no `cwd` (a pnpm job) ran at the root itself.
 */
export function rootOfWorkspace(workspace, cwd) {
  const where = posixOf(workspace);
  if (cwd === undefined || cwd === ".") return where;
  return where.endsWith(`/${cwd}`) ? where.slice(0, -(cwd.length + 1)) : null;
}

/** Whether the signed workspace is the directory the row runs in. With `checkout` (the gate's own primary checkout) it must be exactly that directory of it. */
function workspaceIs(workspace, row, checkout) {
  if (row.cwd === undefined) return true;
  if (typeof workspace !== "string" || !workspace) return false;
  if (checkout) {
    const want = path.join(checkout, ...(row.cwd === "." ? [] : row.cwd.split("/")));
    return posixOf(workspace) === posixOf(want) || posixOf(realOf(workspace)) === posixOf(realOf(want));
  }
  return rootOfWorkspace(workspace, row.cwd) !== null;
}

/**
 * Whether a signed payload's request is this command: request.kind, adapter (when any), requestedArgv, and for a cargo command the
 * directory it ran in (`row.cwd`: a different crate's call cannot satisfy it). `checkout` pins that directory exactly (the gate knows its
 * checkout; the hosted admission does not and checks the suffix).
 */
export function requestIs(payload, row, { checkout = null } = {}) {
  const request = payload?.request;
  return Boolean(request && typeof request === "object" && request.kind === row.kind && (row.adapter === undefined || request.adapter === row.adapter) && row.argvs.some((argv) => argvMatches(argv, request.requestedArgv)) && workspaceIs(request.workspace, row, checkout));
}

/**
 * Assigns each job (`{ id, payload }`) to a distinct command of the stage that runs in the broker (a `direct` row has none). Returns { assigned: Map(id -> row), unmatched: [id...],
 * missing: [row...] }: a job that is none of the commands is unmatched; a command no job covers is missing.
 */
export function assignJobs(rows, jobs, options = {}) {
  // A direct row (GC-A11) is never a broker job: it is neither matched to a receipt nor reported missing; directRowsNotRun checks it.
  const free = rows.filter((row) => row.direct !== true).map((row) => ({ row, taken: false }));
  const assigned = new Map(), unmatched = [];
  for (const job of jobs) {
    const slot = free.find((s) => !s.taken && requestIs(job.payload, s.row, options));
    if (slot) { slot.taken = true; assigned.set(job.id, slot.row); } else unmatched.push(job.id);
  }
  return { assigned, unmatched, missing: free.filter((s) => !s.taken).map((s) => s.row) };
}

/**
 * The direct rows (GC-A11) of a stage that the carrier's ledger does not show as run, each with why. `entries` are the stage's ledger
 * records (cargo-ledger.mjs readLedger). A direct row is satisfied by a record of a direct call (`direct: true`, status `ran`) with exit 0
 * whose cargo words, locked as the stage's are, are one of the row's spellings and whose directory (the process cwd, or the
 * `--manifest-path` directory) is the row's `cwd` of the checkout the stage ran in. One record satisfies one row.
 */
export function directRowsNotRun(rows, entries) {
  const pool = (entries ?? []).filter((e) => e?.direct === true && e.status === "ran");
  const problems = [];
  for (const row of rows.filter((r) => r.direct === true)) {
    const matching = pool.filter((e) => {
      const split = splitCarriedArgs(Array.isArray(e.argv) ? e.argv : []);
      if (split.refusal) return false;
      const dir = posixOf(split.manifestPath === null ? e.cwd : path.resolve(String(e.cwd), path.dirname(split.manifestPath)));
      const words = lockedArgv(split.words);
      return row.argvs.some((argv) => argv.length === words.length && argv.every((w, i) => w === words[i])) && (row.cwd === "." ? dir !== "" : dir.endsWith(`/${row.cwd}`) || dir === row.cwd);
    });
    const ran = matching.find((e) => e.exit === 0);
    if (ran) { pool.splice(pool.indexOf(ran), 1); continue; }
    problems.push({ row, why: matching.length ? `ran with exit ${matching[0].exit}` : "has no recorded direct run" });
  }
  return problems;
}
