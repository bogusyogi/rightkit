// The gate's tool route: `rightkit-gate --to tool --unit cargo:rightkit-qa [--no-broker <exception-id>]`.
//
// Release QA runs only an ENROLLED rightkit-qa runner of the exact version an app's `qa.version` names (qa-runner.mjs). The unit stages cannot
// make that binary: `--to compile|package --unit cargo:rightkit-qa` runs the unit's test plan and packs a .crate, and refuses a version that is
// already on crates.io (local-publish.mjs); `cargo install` is denied (cargo-shapes.mjs). This route is the one sanctioned way to produce it:
//
//   1. the unit's class A check (the compile tier), exactly as a unit stage runs it;
//   2. `cargo build --release --locked --bin <bin>` in the unit's own Cargo workspace of the selected commit's snapshot, through the same executor,
//      cargo wrapper or carrier and target-directory binding as every other unit compile (unit-gate.mjs / bindUnitCargo). There is NO registry-version
//      check: a tool build of a published version is the point of it, and it publishes nothing;
//   3. the produced binary is enrolled (qa-runner.mjs `enrollQaRunner`): version = the crate's COMMITTED version, answered by the binary's own
//      `--version`, a COPY recorded with its sha256 under the managed data root. The enrolled path and sha256 are printed so the owner can add the
//      admission-policy entry (`right-release qa policy approve-runner`). Admission itself is unchanged and stays the owner's.
//
// `cargo:rightkit-credentials` takes the same route and is enrolled as the managed OS credential provider (credential-provider.mjs) that updater signing runs.
// Only units named in TOOL_UNITS have this route. The route never uploads, signs or publishes, and it enrols only on a Mac or Windows host (the platforms
// a runner is enrolled for: its bytes are executed here to read their version).
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { endRun, startRun } from "./gate-stages.mjs";
import { runCargoSync } from "./managed-cargo.mjs";
import { resolveManagedRoots } from "./managed-roots.mjs";
import { CredentialProviderError, enrollCredentialProvider } from "./credential-provider.mjs";
import { QaRunnerError, enrollQaRunner } from "./qa-runner.mjs";
import { knownRedEntry, resolveUnit } from "./unit-gate.mjs";

export const TOOL_STAGE = "tool";
export const TOOL_REPORT_SCHEMA = "rightkit.tool-build-report/1";
/** The units that have a tool route: the crate, the binary its manifest declares, and what the binary is enrolled as. */
export const TOOL_UNITS = Object.freeze({
  "cargo:rightkit-qa": Object.freeze({ crate: "rightkit-qa", bin: "rightkit-qa", enrols: "qa-runner" }),
  "cargo:rightkit-credentials": Object.freeze({ crate: "rightkit-credentials", bin: "rightkit-credentials", enrols: "credential-provider" }),
});
const HOST_PLATFORM = Object.freeze({ darwin: "mac", win32: "win" });
const BUILD_TIMEOUT_MS = 30 * 60 * 1000;

export class ToolBuildError extends Error {
  constructor(code, message) { super(`${code}: ${message}`); this.name = "ToolBuildError"; this.code = code; }
}
const refuse = (code, message) => { throw new ToolBuildError(code, message); };

/** The cargo arguments of the build (no `cargo`): the one shape the broker admits as a compile and a stage grant carries (cargo-shapes.mjs). */
export const toolBuildArgs = (bin) => ["build", "--release", "--locked", "--bin", bin];

/**
 * What `--to tool --unit <selector>` builds on `platform` (a `process.platform` value), decided before ownership, a snapshot or any process:
 * `{ selector, crate, bin, enrols, cargoDir, argv, executable, runnerPlatform }`, or a typed ToolBuildError.
 */
export function toolPlan({ selector, platform }) {
  const unit = Object.hasOwn(TOOL_UNITS, selector ?? "") ? TOOL_UNITS[selector] : null;
  if (!unit) refuse("RK_TOOL_UNIT_UNSUPPORTED", `${JSON.stringify(selector ?? null)} has no tool route (${Object.keys(TOOL_UNITS).join(", ")}); \`--to tool\` builds only a unit named there`);
  const runnerPlatform = HOST_PLATFORM[platform];
  if (!runnerPlatform) refuse("RK_TOOL_HOST", `a ${unit.bin} runner is built and enrolled on a Mac or a Windows host (this is ${platform}): its bytes are executed there to read their version, and a runner is enrolled per platform`);
  return { selector, ...unit, cargoDir: `crates/${unit.crate}`, argv: toolBuildArgs(unit.bin), executable: runnerPlatform === "win" ? `${unit.bin}.exe` : unit.bin, runnerPlatform };
}

/** The pinned target directory of the Cargo workspace that holds `cargoDir`: its own entry, else the nearest enclosing one (a crate that is a member of a parent workspace). Null when none is pinned. */
export function pinnedTargetDir(targetDirs, cargoDir) {
  for (let dir = cargoDir; ; dir = path.posix.dirname(dir)) {
    if (typeof targetDirs?.[dir] === "string" && targetDirs[dir]) return targetDirs[dir];
    if (dir === "." || dir === "/" || dir === "") return null;
  }
}

const sha256OfFile = (file) => crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");

/**
 * The built binary under `outputDir` (the target directory: the carried receipt's `outputDir` on a broker host, the checkout's pinned target on a direct
 * one): `<outputDir>/release/<executable>`, a regular file the build just wrote, never a link. Returns its path or throws.
 */
export function builtToolBinary({ outputDir, executable }) {
  if (typeof outputDir !== "string" || !path.isAbsolute(outputDir)) refuse("RK_TOOL_NO_OUTPUT_DIR", `the build reported no absolute output directory (${JSON.stringify(outputDir ?? null)}); the produced binary cannot be located, so nothing was enrolled`);
  const file = path.join(outputDir, "release", executable);
  let meta;
  try { meta = fs.lstatSync(file); } catch (error) { refuse("RK_TOOL_NO_BINARY", `the build succeeded but ${file} does not exist (${error.code ?? error.message}); nothing was enrolled`); }
  if (meta.isSymbolicLink() || !meta.isFile()) refuse("RK_TOOL_NO_BINARY", `${file} is not a regular file; nothing was enrolled`);
  return file;
}

/**
 * Runs the tool route for one unit of the selected commit's snapshot. `root` is the snapshot, `commonDir` the checkout's git directory. `check(tier)`
 * is the door's class A run and `stageGuard` its ledger verify around each stage (as runUnitGate takes them); `envExtra` is the Cargo binding
 * (bindUnitCargo's env) and `targetDirs` its `{ [cargoDir]: checkout target directory }` for a direct host. Everything a test replaces is a parameter:
 * `run` (the cargo call: managed-cargo's runCargoSync, which carries it through the broker or runs it direct), `enroll`, `resolve`, `dataRoot`.
 * Returns the exit code (0, or 1 with the reason printed).
 */
export async function runUnitTool({
  selector, root, commonDir, commit, tree, executor, env, platform, check, source = null, stageGuard = null, envExtra = {}, targetDirs = {},
  resolve = resolveUnit, run = runCargoSync, enroll = enrollQaRunner, enrollProvider = enrollCredentialProvider, dataRoot = undefined, readKnownRed = null,
  out = console.log, err = console.error, writeReport = null,
}) {
  let plan; let unit;
  try {
    plan = toolPlan({ selector, platform });
    unit = resolve(selector, root);
    const red = knownRedEntry({ unit, root, platform, ...(readKnownRed ? { readText: readKnownRed } : {}) });
    if (red) refuse("RK_TOOL_KNOWN_RED", `${selector} is listed in ci/known-red.json for ${red.host} (${red.key}: ${red.reason}); a known-red unit is not built. Fix the defect and remove the entry first`);
  } catch (error) { err(`rightkit-gate: BLOCKED: ${error.message}`); return 1; }
  const report = { schema: TOOL_REPORT_SCHEMA, unit, commit, tree, executor: executor?.kind ?? null, stages: {} };
  const save = () => { if (writeReport) writeReport(report); };
  let stage = "check";
  try {
    if (stageGuard && !(await stageGuard.start("check"))) throw new Error("RK_CHECK_ERROR: the snapshot is not the commit's bytes plus the ledger before the check stage");
    if (await check("compile") !== 0) throw new Error("class A check (compile tier) failed");
    if (stageGuard && !(await stageGuard.end("check"))) throw new Error("RK_CHECK_ERROR: the check stage's result covers no known bytes");
    report.stages.check = { ok: true }; save();
    out(`rightkit-gate: [check] ${selector} passed`);

    stage = TOOL_STAGE;
    if (stageGuard && !(await stageGuard.start(TOOL_STAGE))) throw new Error(`RK_CHECK_ERROR: the snapshot is not the commit's bytes plus the ledger before the ${TOOL_STAGE} stage`);
    const doorRun = startRun({ commonDir, commit, tree, stage: TOOL_STAGE });
    let built;
    try {
      const childEnv = { ...env, ...envExtra, ...doorRun.env };
      const cwd = path.join(root, ...plan.cargoDir.split("/"));
      out(`rightkit-gate: [tool] ${selector}@${unit.version}: cargo ${plan.argv.join(" ")} (from ${plan.cargoDir}; executor ${executor?.kind ?? "?"})`);
      const result = await run(plan.argv, { cwd, env: childEnv, encoding: "utf8", maxBuffer: 512 * 1024 * 1024, timeout: BUILD_TIMEOUT_MS, windowsHide: true });
      if (result?.error || result?.status !== 0) {
        throw new Error(`${selector}: cargo ${plan.argv.join(" ")} failed (${result?.error?.message ?? `exit ${result?.status}`})\nstdout:\n${String(result?.stdout ?? "").slice(-4000)}\nstderr:\n${String(result?.stderr ?? "").slice(-4000)}`);
      }
      // A carried (broker) call names the receipt's output directory; a direct call wrote the checkout target the gate pinned for this workspace.
      built = builtToolBinary({ outputDir: result.outputDir ?? pinnedTargetDir(targetDirs, plan.cargoDir), executable: plan.executable });
    } finally { endRun(doorRun); }
    if (stageGuard && !(await stageGuard.end(TOOL_STAGE))) throw new Error(`RK_CHECK_ERROR: the ${TOOL_STAGE} stage's result covers no known bytes (a snapshot byte changed, a write is outside the ledger, or a cargo call did not complete)`);
    const builtSha256 = sha256OfFile(built);
    report.stages.tool = { ok: true, binary: built, binarySha256: builtSha256, version: unit.version }; save();
    out(`rightkit-gate: [tool] built ${built} (sha256 ${builtSha256})`);

    stage = "enrol";
    let enrolled;
    try {
      const root_ = dataRoot ?? resolveManagedRoots({ env }).dataRoot;
      enrolled = plan.enrols === "credential-provider"
        ? enrollProvider({ dataRoot: root_, platform: plan.runnerPlatform, binary: built, version: unit.version })
        : enroll({ dataRoot: root_, platform: plan.runnerPlatform, runner: built, version: unit.version });
    } catch (error) {
      if (error instanceof CredentialProviderError || error?.code?.startsWith?.("RK_CRED_PROVIDER_")) throw new Error(`${error.code}: ${error.message}\nthe built binary is ${built} (sha256 ${builtSha256}); a deliberate replacement of an enrolled version is \`node packages/release/credential-provider.mjs enroll --binary ${built} --version ${unit.version} --platform ${plan.runnerPlatform} --rotate\``);
      if (error instanceof QaRunnerError || error?.code?.startsWith?.("RK_QA_RUNNER_")) throw new Error(`${error.code}: ${error.message}\nthe built binary is ${built} (sha256 ${builtSha256}); a deliberate replacement of an enrolled version is \`right-release qa runner enroll --runner ${built} --version ${unit.version} --platform ${plan.runnerPlatform} --rotate\``);
      throw error;
    }
    // The copy that was recorded must be the very bytes that were built: a binary swapped between the digest and the copy is refused.
    if (enrolled.entry.sha256 !== builtSha256) throw new Error(`RK_TOOL_DIGEST_DRIFT: the enrolled copy ${enrolled.runnerFile} has sha256 ${enrolled.entry.sha256}, the build produced ${builtSha256}; the binary changed while it was enrolled, so it is not trusted (enrol it again from a fresh build)`);
    report.stages.enrol = { ok: true, runner: enrolled.runnerFile, sha256: enrolled.entry.sha256, version: enrolled.entry.version, platform: plan.runnerPlatform, record: enrolled.file, rotated: enrolled.rotated };
    save();
    out(`rightkit-gate: [tool] enrolled ${plan.bin} ${enrolled.entry.version} for ${plan.runnerPlatform}: ${enrolled.runnerFile} sha256 ${enrolled.entry.sha256} (record ${enrolled.file})`);
    if (plan.enrols === "credential-provider") out("rightkit-gate: [tool] updater signing and the admission check resolve this enrolled provider (credential-provider.mjs); PATH is not used on a Mac or Windows host");
    else out(`rightkit-gate: [tool] the owner admits it with: right-release qa policy approve-runner --runner ${enrolled.runnerFile} --version ${enrolled.entry.version} --platform ${plan.runnerPlatform} --set-by <name>`);
    out(`rightkit-gate: [tool] ${selector} passed`);
    return 0;
  } catch (error) {
    report.stages[stage] = { ok: false }; save();
    err(`rightkit-gate: tool ${selector} stopped at ${stage}: ${error.message}`);
    return 1;
  }
}
