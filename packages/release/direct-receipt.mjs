// The direct (Linux) host's evidence for a snapshot compile (item 7 rows 7.09, 7.12, 7.18; GATE-CONTRACT "Broker interface"):
//   - the output directory, resolved in the PRIMARY CHECKOUT with the stage's environment before the source location changes, with
//     its inputs recorded (7.09);
//   - the receipt the gate writes itself, "the same field names minus the broker-only ones (admission, queue, reusedFrom)" (7.18),
//     and the consumer-side matrix that refuses a receipt that lacks one, carries a broker-only one, or holds no census (7.22 Linux half).
// Pure functions with injected spawn and file system: the gate (bin/rightkit-gate.mjs) wires them to the host.
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { validateFreshness } from "./freshness-census.mjs";

export const DIRECT_RECEIPT_SCHEMA = "rightkit.gate-direct-receipt/1";
export const DIAGNOSTICS_SCHEMA = "rightkit.gate-direct-diagnostics/1";
/** The names GATE-CONTRACT lists as broker-only: a direct receipt never carries them. */
export const BROKER_ONLY_FIELDS = Object.freeze(["admission", "queue", "reusedFrom"]);
/** The contract's returned-evidence names a direct receipt carries (7.18). `snapshot.path|treeAtStart|treeAtExit` are checked inside `snapshot`. */
export const DIRECT_RECEIPT_REQUIRED = Object.freeze(["requestId", "brokerVersion", "executor", "commit", "snapshot", "declaredInputDigests", "verdict", "freshness", "timingsMs", "exit", "diagnosticsPath", "outputDir", "refreshed"]);
export const TIMING_FIELDS = Object.freeze(["bind", "cargo", "afterExitBind", "diagnostics"]);
/** The environment a `cargo metadata` target resolution reads: recorded by value (the rest of the environment by digest only). */
export const TARGET_ENV_NAMES = Object.freeze(["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "CARGO_HOME", "RUSTUP_TOOLCHAIN"]);

const failed = (r) => !r || Boolean(r.error) || r.status !== 0;
const why = (r) => String(r?.error?.message ?? r?.stderr ?? "").trim().split(/\r?\n/).slice(-3).join(" | ");

/** True when `child` is `parent` or below it (lexically, after resolving both). */
export function isWithin(parent, child) {
  const rel = path.relative(path.resolve(parent), path.resolve(child));
  return rel === "" || (rel !== ".." && !rel.startsWith(`..${path.sep}`) && !path.isAbsolute(rel));
}

/** The stage environment a resolution ran with: the target-relevant names by value, the whole environment by digest. */
export function envInputs(env) {
  const record = {};
  for (const name of TARGET_ENV_NAMES) record[name] = typeof env?.[name] === "string" ? env[name] : null;
  const all = Object.keys(env ?? {}).sort().map((k) => `${k}=${env[k]}`).join("\n");
  return { ...record, digest: createHash("sha256").update(all).digest("hex") };
}

/**
 * The absolute output directory Cargo would use for one workspace, resolved where the contract says: `cargo metadata --format-version 1
 * --no-deps` run with its working directory in the PRIMARY CHECKOUT (never the snapshot) and with the stage's environment, so
 * `CARGO_TARGET_DIR`, `.cargo/config.toml` `build.target-dir` (relative to the checkout's config, or absolute) and the default `target/`
 * all resolve as they do for a developer's own build. Returns `{ dir, inputs }` (the inputs are recorded in the receipt) or throws an
 * `RK_TARGET_RESOLUTION` error whose message names why: a resolution made in the snapshot, no absolute directory, or a directory that
 * lies inside the snapshot (a second output directory).
 */
export function resolveTargetDirectory({ spawn, cargoCommand, manifest, env, checkoutRoot, snapshotRoot = null, workspace = "." }) {
  const fail = (message) => Object.assign(new Error(message), { code: "RK_TARGET_RESOLUTION" });
  const cwd = path.dirname(manifest);
  if (!isWithin(checkoutRoot, cwd)) throw fail(`the output directory of workspace ${workspace} must be resolved in the primary checkout ${checkoutRoot}, not in ${cwd}`);
  if (snapshotRoot && isWithin(snapshotRoot, cwd)) throw fail(`the output directory of workspace ${workspace} must be resolved before the source location changes, not in the snapshot ${snapshotRoot}`);
  const args = [...cargoCommand.slice(1), "metadata", "--offline", "--format-version", "1", "--no-deps", "--manifest-path", manifest];
  const r = spawn(cargoCommand[0], args, { cwd, env });
  let dir = null;
  try { dir = failed(r) ? null : JSON.parse(String(r.stdout ?? "")).target_directory; } catch { dir = null; }
  if (typeof dir !== "string" || !path.isAbsolute(dir)) throw fail(`cargo metadata in the checkout reported no absolute target_directory for ${workspace}${failed(r) ? ` (${why(r)})` : ""}`);
  const resolved = path.resolve(dir);
  if (snapshotRoot && isWithin(snapshotRoot, resolved)) throw fail(`the output directory of workspace ${workspace} resolves to ${resolved}, inside the snapshot ${snapshotRoot}; a snapshot compile writes only to the checkout's directory, never a second one under the commit`);
  return { dir: resolved, inputs: { cwd, manifest, command: [cargoCommand[0], ...args], env: envInputs(env), targetDirectory: resolved } };
}

/** The Class A and B result the receipt's `verdict` carries (a direct compile reuses nothing, so no `reusedFrom`). */
export function directVerdict({ scans = [], failures = [], deferrals = [] } = {}) {
  const byClass = (c) => { const rows = scans.filter((s) => s.class === c); return { scans: rows.length, failed: rows.filter((s) => s.status === "fail").length }; };
  return { result: failures.length ? "FAILED" : "PASSED", classA: byClass("A"), classB: byClass("B"), deferred: deferrals.length };
}

/** Writes the structured diagnostics of one direct compile and returns the file's absolute path (the receipt's `diagnosticsPath`). */
export function writeDirectDiagnostics({ dir, requestId, commit, exit, scans = [], failures = [], deferrals = [], notes = [], freshness = null, writeFile = fs.writeFileSync, mkdir = fs.mkdirSync }) {
  mkdir(dir, { recursive: true });
  const file = path.resolve(dir, `direct-diagnostics-${requestId}.json`);
  const rows = scans.map((s) => ({ id: s.id, class: s.class ?? null, status: s.status, ...(s.code ? { code: s.code } : {}), ...(s.detail ? { detail: String(s.detail).slice(0, 1000) } : {}) }));
  writeFile(file, `${JSON.stringify({ schema: DIAGNOSTICS_SCHEMA, requestId, commit, exit, scans: rows, failures, deferrals, notes, freshness }, null, 2)}\n`);
  return file;
}

/** The receipt of one direct compile: the contract's returned-evidence names, no broker-only one. Throws nothing; `verifyDirectReceipt` judges it. */
export function buildDirectReceipt({ requestId, issuedAt, commit, snapshotPath, treeAtStart, treeAtExit, ownershipHandle = null, declaredInputDigests = [], verdict, freshness, timingsMs, exit, diagnosticsPath, outputDirs = {}, outputResolution = {}, cargoWrapper = null }) {
  const dirs = [...new Set(Object.values(outputDirs))];
  return {
    schema: DIRECT_RECEIPT_SCHEMA, requestId, brokerVersion: null, executor: "direct", issuedAt,
    commit: { sha: commit.sha, tree: commit.tree },
    snapshot: { path: snapshotPath, treeAtStart, treeAtExit },
    declaredInputDigests, ownership: { handle: ownershipHandle },
    verdict, freshness, timingsMs, exit, exitStatus: exit, diagnosticsPath,
    outputDir: dirs.length === 1 ? dirs[0] : null, outputDirs: { ...outputDirs }, outputResolution,
    refreshed: [], cargoWrapper,
  };
}

/**
 * The consumer's matrix over a direct receipt (the Linux half of row 7.22): every contract name present with the right type, the
 * snapshot bound to the commit's tree at start and exit, a well formed freshness census, and none of the broker-only names.
 * Returns the list of problems; an empty list means the receipt verifies.
 */
export function verifyDirectReceipt(receipt) {
  const problems = [];
  if (!receipt || typeof receipt !== "object" || Array.isArray(receipt)) return ["the receipt is not an object"];
  if (receipt.schema !== DIRECT_RECEIPT_SCHEMA) problems.push(`schema is ${JSON.stringify(receipt.schema)}, not ${DIRECT_RECEIPT_SCHEMA}`);
  for (const name of DIRECT_RECEIPT_REQUIRED) if (!Object.hasOwn(receipt, name)) problems.push(`${name} is absent`);
  for (const name of BROKER_ONLY_FIELDS) if (Object.hasOwn(receipt, name)) problems.push(`${name} is a broker-only field; a direct receipt never carries it`);
  const text = (v) => typeof v === "string" && v !== "";
  if (Object.hasOwn(receipt, "requestId") && !text(receipt.requestId)) problems.push("requestId is not a string");
  if (Object.hasOwn(receipt, "brokerVersion") && receipt.brokerVersion !== null) problems.push("brokerVersion is not null on a direct host");
  if (Object.hasOwn(receipt, "executor") && receipt.executor !== "direct") problems.push(`executor is ${JSON.stringify(receipt.executor)}, not "direct"`);
  const commit = receipt.commit;
  if (Object.hasOwn(receipt, "commit") && !(commit && text(commit.sha) && text(commit.tree))) problems.push("commit has no sha and tree");
  const snap = receipt.snapshot;
  if (Object.hasOwn(receipt, "snapshot")) {
    if (!(snap && text(snap.path) && text(snap.treeAtStart) && text(snap.treeAtExit))) problems.push("snapshot has no path, treeAtStart and treeAtExit");
    else if (commit && text(commit.tree) && (snap.treeAtStart !== commit.tree || snap.treeAtExit !== commit.tree)) problems.push("snapshot.treeAtStart and treeAtExit are not the commit's tree");
  }
  if (Object.hasOwn(receipt, "declaredInputDigests") && !Array.isArray(receipt.declaredInputDigests)) problems.push("declaredInputDigests is not a list");
  const verdict = receipt.verdict;
  if (Object.hasOwn(receipt, "verdict") && !(verdict && (verdict.result === "PASSED" || verdict.result === "FAILED"))) problems.push("verdict has no PASSED or FAILED result");
  if (Object.hasOwn(receipt, "freshness")) for (const p of validateFreshness(receipt.freshness)) problems.push(p);
  const timings = receipt.timingsMs;
  if (Object.hasOwn(receipt, "timingsMs")) {
    if (!timings || typeof timings !== "object") problems.push("timingsMs is not an object");
    else {
      for (const name of TIMING_FIELDS) if (!(Number.isFinite(timings[name]) && timings[name] >= 0)) problems.push(`timingsMs.${name} is not a duration`);
      for (const name of BROKER_ONLY_FIELDS) if (Object.hasOwn(timings, name)) problems.push(`timingsMs.${name} is a broker-only field`);
    }
  }
  if (Object.hasOwn(receipt, "exit") && !Number.isInteger(receipt.exit)) problems.push("exit is not an exit status");
  if (Object.hasOwn(receipt, "diagnosticsPath") && !(text(receipt.diagnosticsPath) && path.isAbsolute(receipt.diagnosticsPath))) problems.push("diagnosticsPath is not an absolute path");
  if (Object.hasOwn(receipt, "outputDir")) {
    const dirs = receipt.outputDirs && typeof receipt.outputDirs === "object" ? Object.values(receipt.outputDirs) : [];
    if (text(receipt.outputDir)) { if (!path.isAbsolute(receipt.outputDir)) problems.push("outputDir is not absolute"); }
    else if (!(receipt.outputDir === null && new Set(dirs).size !== 1)) problems.push("outputDir is not the resolved absolute path (null only when the commit has no Cargo workspace, or several workspaces name several directories)");
    for (const d of dirs) if (!(text(d) && path.isAbsolute(d))) problems.push(`outputDirs holds ${JSON.stringify(d)}, not an absolute path`);
  }
  if (Object.hasOwn(receipt, "refreshed") && !Array.isArray(receipt.refreshed)) problems.push("refreshed is not a list");
  return problems;
}
