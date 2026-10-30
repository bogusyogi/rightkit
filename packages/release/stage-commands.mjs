// What a unit's compile and package broker jobs must have run, derived from the code that builds those commands (local-publish.mjs
// localTestPlan and the pack argv builders), so the gate (which chooses the receipts to carry) and the hosted admission (which checks
// the signed requestedArgv of each) share one definition and cannot drift from the stages. EVERY command of a stage must be covered by
// a signed receipt of its own job; a missing command is a refusal that names it.
import fs from "node:fs";
import path from "node:path";
import { CARGO_METADATA_ARGS, cargoPackageArgs, localTestPlan, npmPackArgs, qualifyFmtArgs } from "./local-publish.mjs";
import { SWAP_HELPER_CARGO_ARGS, SWAP_HELPER_METADATA_ARGS, SWAP_HELPER_PACKAGE } from "./swap-helper-bundle.mjs";
import { carriedRequestOf } from "./snapshot-request.mjs";

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
  return { kind: "cargo", argvs: carried.map((c) => c.argv), cwd: carried[0].cwd };
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
    if (unit === SWAP_HELPER_PACKAGE) pack.push(CARGO(".", SWAP_HELPER_CARGO_ARGS), CARGO(".", SWAP_HELPER_METADATA_ARGS));
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
 * Assigns each job (`{ id, payload }`) to a distinct command of the stage. Returns { assigned: Map(id -> row), unmatched: [id...],
 * missing: [row...] }: a job that is none of the commands is unmatched; a command no job covers is missing.
 */
export function assignJobs(rows, jobs, options = {}) {
  const free = rows.map((row) => ({ row, taken: false }));
  const assigned = new Map(), unmatched = [];
  for (const job of jobs) {
    const slot = free.find((s) => !s.taken && requestIs(job.payload, s.row, options));
    if (slot) { slot.taken = true; assigned.set(job.id, slot.row); } else unmatched.push(job.id);
  }
  return { assigned, unmatched, missing: free.filter((s) => !s.taken).map((s) => s.row) };
}
