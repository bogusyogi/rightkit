// What a unit's compile and package broker jobs must have run, derived from the code that builds those commands (local-publish.mjs
// localTestPlan and the pack argv builders), so the gate (which chooses the receipts to carry) and the hosted admission (which checks
// the signed requestedArgv of each) share one definition and cannot drift from the stages. EVERY command of a stage must be covered by
// a signed receipt of its own job; a missing command is a refusal that names it.
import path from "node:path";
import { CARGO_METADATA_ARGS, cargoPackageArgs, localTestPlan, npmPackArgs, qualifyFmtArgs } from "./local-publish.mjs";
import { SWAP_HELPER_CARGO_ARGS, SWAP_HELPER_PACKAGE } from "./swap-helper-bundle.mjs";

/** Stands for the one argument that is a fresh absolute scratch path (`pnpm pack --pack-destination <mkdtemp>`). */
export const ANY_ABSOLUTE_PATH = Object.freeze({ anyAbsolutePath: true });
const PNPM = (...argvs) => ({ kind: "adapter", adapter: "rightkit-pnpm", argvs });
const CARGO = (...argvs) => ({ kind: "cargo", argvs });

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
  const compile = plan.steps.map((step) => (step.command === "pnpm" ? PNPM(step.args) : CARGO(step.args)));
  const pack = [];
  if (kind === "npm") {
    if (unit === SWAP_HELPER_PACKAGE) {
      // The swap helper builds and signs a binary, then packs from a scratch copy of its directory.
      pack.push(CARGO(SWAP_HELPER_CARGO_ARGS), CARGO(["metadata", "--format-version", "1", "--no-deps"]), PNPM(npmPackArgs(ANY_ABSOLUTE_PATH, ANY_ABSOLUTE_PATH)));
    } else pack.push(PNPM(npmPackArgs(dir, ANY_ABSOLUTE_PATH)));
  } else {
    compile.unshift(CARGO(qualifyFmtArgs(unit)));
    pack.push(CARGO(cargoPackageArgs(unit, true)), CARGO([...CARGO_METADATA_ARGS], CARGO_METADATA_ARGS.filter((arg) => arg !== "--offline")));
  }
  return { compile, package: pack };
}

const argvMatches = (expected, actual) => Array.isArray(actual) && expected.length === actual.length
  && expected.every((want, i) => (want === ANY_ABSOLUTE_PATH ? typeof actual[i] === "string" && (path.posix.isAbsolute(actual[i]) || /^[A-Za-z]:[\\/]/.test(actual[i])) : want === actual[i]));

/** Whether a signed payload's request is this command: request.kind, adapter (when any) and requestedArgv. */
export function requestIs(payload, row) {
  const request = payload?.request;
  return Boolean(request && typeof request === "object" && request.kind === row.kind && (row.adapter === undefined || request.adapter === row.adapter) && row.argvs.some((argv) => argvMatches(argv, request.requestedArgv)));
}

/**
 * Assigns each job (`{ id, payload }`) to a distinct command of the stage. Returns { assigned: Map(id -> row), unmatched: [id...],
 * missing: [row...] }: a job that is none of the commands is unmatched; a command no job covers is missing.
 */
export function assignJobs(rows, jobs) {
  const free = rows.map((row) => ({ row, taken: false }));
  const assigned = new Map(), unmatched = [];
  for (const job of jobs) {
    const slot = free.find((s) => !s.taken && requestIs(job.payload, s.row));
    if (slot) { slot.taken = true; assigned.set(job.id, slot.row); } else unmatched.push(job.id);
  }
  return { assigned, unmatched, missing: free.filter((s) => !s.taken).map((s) => s.row) };
}
