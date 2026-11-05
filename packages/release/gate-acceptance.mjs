#!/usr/bin/env node
// End-to-end acceptance runner for the gate (rightkit#1514). It drives the real gate against the witness fixture and asserts every operative clause of
// docs/GATE-CONTRACT.md from that run's OWN artifacts: the door receipt (--receipt-out), the per-scan coverage reports, the log, the owner record and
// git. Spec: /Volumes/D/scratch/gate-acceptance/criteria.md (clause table, run sequence S0-S9, negatives, kill-and-recover).
//
//   node gate-acceptance.mjs --fixture <dir> [--host mac|dell] [--steps S0,S1,...] [--out <dir>] [--plan] [--gate <path>] [--commit <sha>] [--allow-install]
//
// Rules this file keeps:
//  * Three states per clause: "pass", "fail", "unchecked". A clause with no evidence available is "unchecked", never "pass". A skipped scan row is never a pass.
//  * Fail closed: an unknown field, a missing artifact or a malformed value is a "fail" with the reason, never skipped over.
//  * Every assertion names its clause id and the evidence path (or evidence source) it read.
//  * No cache bypass flag is ever passed to do work (N13 passes the two retired bypasses only to prove the gate refuses them) and nothing is pushed or published. Nothing is installed by default: `--to dev` and `--to qualify` install
//    apps, so they run only with `--allow-install` (which needs Adrian's approval); without it their clauses are "unchecked: needs install approval".
//    Negative inputs are ephemeral `git commit-tree` commits under refs/witness/<case> (no branch, no worktree, no checkout write); the refs are deleted
//    at the end. The one checkout write is S6's dirty-checkout setup, which refuses a dirty fixture and restores it exactly.
//  * The verdict is PASS only when no clause, step or step-only check failed and every step ran as specified; it is always printed with the
//    count of unchecked clauses ("PASS (N of 67 asserted, M unchecked)"), never as a bare PASS.
//  * Timing is relative and informational only: the contract gives no latency threshold, so wall time never fails a clause.
// The pure assertion functions are exported and tested by gate-acceptance.test.mjs; `--plan` prints the steps and clause mapping and runs nothing.
import { createHash } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { keyPath, repoKey } from "./snapshot.mjs";
import { ownershipFile, runningBrokerProcess } from "./build-ownership.mjs";
import { RUNTIME_MANIFEST_FILE } from "./broker-handshake.mjs";
import { resolveBrokerClient } from "./broker-client.mjs";
import { managedRuntimeClient } from "./managed-roots.mjs";

export const RESULT_SCHEMA = "rightkit.gate-acceptance/1";
export const COVERAGE_SCHEMA = "rightkit.gate-coverage/1";
export const RECEIPT_SCHEMA = "rightkit.gate-receipt/1";
export const SOURCE_IDENTITY_SCHEMA = "rightkit.source-identity.v1";
export const RENDER_RECEIPT_SCHEMA = "rightkit.render-views/1";
export const OWNERSHIP_SCHEMA = "rightkit.build-ownership/1";

const here = path.dirname(fileURLToPath(import.meta.url));
const sha256 = (data) => createHash("sha256").update(data).digest("hex");
const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const isSha = (v) => typeof v === "string" && /^[0-9a-f]{64}$/.test(v);
const isCommit = (v) => typeof v === "string" && /^[0-9a-f]{40}$/.test(v);

// ---------------------------------------------------------------------------------------------------------------------------------------------
// Clause registry: the 67 operative rows of criteria.md section 1 (id, evidence tag, title). `implemented` clauses have an assertion in CHECKS below;
// every other clause is reported "unchecked" with the reason from UNCHECKED_WHY (the gate writes no evidence the runner may read, or the fixture
// rows the case needs do not exist yet).
// ---------------------------------------------------------------------------------------------------------------------------------------------
const ROWS = [
  ["GC-A01", "M", "warm only; one snapshot dir; one worktree; no cache bypass"], ["GC-A02", "M", "stash never a build source, never left behind"],
  ["GC-A03", "P", "Class A covers the closure plus file-only scans"], ["GC-A04", "M", "build is from the selected commit via the snapshot"],
  ["GC-A05", "M", "broker runs no gate"], ["GC-A06", "P", "runs on the real broker of the host"], ["GC-A07", "P", "no debug build; QA marker only in dev"],
  ["GC-A08", "M", "cargo test compiles in the broker, runs outside"], ["GC-A09", "M", "no test-mode feature"], ["GC-A10", "P", "QA journeys with mandatory [snapshot]"],
  ["GC-A11", "P", "Class A and non-compiling cargo run direct"], ["GC-A12", "G", "tsc once in Class A"], ["GC-A13", "M", "unexplained rebuild never fails; cause evidence recorded"],
  ["GC-A14", "M", "every crate clippy-checked for all three OS targets"], ["GC-A15", "M", "one output and one warm cache per app per host; release profile"],
  ["GC-A16", "G", "dev builds signed, not notarized"], ["GC-A18", "P", "Class B batched per workspace, bisected"], ["GC-A19", "M", "audit scans cached by lock+config+advisory+day"],
  ["GC-A21", "M", "UTC-stamped log, stage timings, per-scan coverage JSON"], ["GC-A22-render", "M", "--to render builds and runs the declared target, PNGs and receipt"],
  ["GC-A22-classA", "M", "ghsa-audit, freshness advice, feature-coverage, secret-scan, store proof"], ["GC-A23", "M", "deterministic .rightkit-source-identity.json"],
  ["GC-A24", "M", "Class A is change-driven, rows keyed on own inputs"],
  ["GC-T01", "P", "origin host compile proves only what it compiled"], ["GC-T02", "P", "Mac and Windows artifacts built natively"], ["GC-T03", "G", "private qualification runs locally"],
  ["GC-T04", "M", "compile only through the broker, no direct fallback"], ["GC-T05", "M", "Class A completes before the first compile"], ["GC-T06", "M", "commit-only build, exact snapshot, per-repository lock"],
  ["GC-T07", "G", "qualification policy pinned by digest"], ["GC-T10", "P", "product manifest declares target kinds"], ["GC-T11", "P", "sign once, qualify sealed bytes"],
  ["GC-T12", "P", "one signing identity per product"], ["GC-T13", "P", "same gate for human and agent callers"], ["GC-T14", "M", "one checkout, one snapshot dir, one output dir; no bypass"],
  ["GC-T16", "M", "every cargo call keeps going"], ["GC-T17", "G", "broker trusts a published gate (contract conflict, unreconciled)"], ["GC-T18", "M", "fixture-files.json skips only per-file format scans"],
  ["GC-T19", "M", "rule tier by stage"], ["GC-T20", "M", "commit and push run no gate and no hook"], ["GC-T24", "M", "unit-list plans, dependency-first, broker argv shapes"],
  ["GC-I01", "M", "snapshot root and repository key"], ["GC-I02", "M", "gate creates no output dir; broker-pinned target"], ["GC-I03", "M", "Class A check route, tree id before and after"],
  ["GC-I04", "M", "request carries commit, snapshot, declaredInputs, ownership"], ["GC-I05", "M", "receipt fields"], ["GC-I06", "P", "broker lease from admission to reap"],
  ["GC-I08", "P", "native tool adapters"], ["GC-I11", "M", "clippy route keeps scope and aggregates FAILED"], ["GC-I12", "M", "broker admits only <data root>/snapshots/<repo-key>"],
  ["GC-I13", "P", "refusal at admission leaves a durable REAPED completion"], ["GC-I14", "M", "absent declared input recorded"], ["GC-I15", "P", "installed broker executable path rule"],
  ["GC-I16", "M", "broker config path"], ["GC-I17", "M", "answering broker's roots on the wire"], ["GC-I18", "M", "pinned digest is the answering broker's"],
  ["GC-I19", "M", "lease status shape and aggregate rule"], ["GC-I20", "P", "scan tools only from enrolled executables"], ["GC-I21", "P", "gate-owned dirs registered in the census"],
  ["GC-I22", "M", "vendored vectors are byte copies"],
  ["GC-Q01", "P", "origin record fields"], ["GC-Q02", "P", "compile digests apart from artifact identity"], ["GC-Q03", "G", "native build record"], ["GC-Q04", "G", "final sealed digest bound to coverage"],
  ["GC-Q05", "G", "private qualification evidence"], ["GC-1A-R2", "M", "build outside broker only with a recorded exception"], ["GC-1A-R4", "M", "committed build definition lists every Cargo workspace"],
].map(([id, ev, title]) => Object.freeze({ id, ev, title }));
export const CLAUSES = Object.freeze(Object.fromEntries(ROWS.map((r) => [r.id, r])));

const NO_EVIDENCE = "the gate writes no artifact the runner may read for this clause (criteria.md section 5)";
const NEEDS_BROKER_RECEIPTS = "needs broker receipts (argv, transformations, testExecutables); the door receipt does not carry them and the receipt-dir path of a stage run is not named by the contract";
const NEEDS_HOST_TOOL = "needs a host tool (codesign, signtool, stapler, find) or the installed app; not read by this runner";
const NEEDS_FIXTURE = "needs fixture rows (criteria.md section 2, F1-F14) that do not exist in the witness fixture yet";
export const NEEDS_INSTALL_APPROVAL = "needs install approval";
/** Clauses whose evidence comes from `--to dev` or `--to qualify`, both of which install an app: unchecked unless `--allow-install` ran them. */
export const INSTALL_CLAUSES = Object.freeze(["GC-A07", "GC-A10", "GC-A16", "GC-T03", "GC-T07", "GC-T11", "GC-T12", "GC-Q04", "GC-Q05"]);
export const UNCHECKED_WHY = Object.freeze({
  "GC-A03": NEEDS_FIXTURE, "GC-A05": NEEDS_BROKER_RECEIPTS, "GC-A06": "needs HANDSHAKE and STATUS from a live broker query; the runner reads the runtime manifest and pins only (see GC-I18)", "GC-A07": NEEDS_HOST_TOOL,
  "GC-A08": NEEDS_BROKER_RECEIPTS, "GC-A09": NEEDS_FIXTURE, "GC-A10": NEEDS_FIXTURE, "GC-A11": NO_EVIDENCE, "GC-A12": NO_EVIDENCE, "GC-A13": "needs the compile freshness object, which sits in broker receipts not in the door receipt",
  "GC-A14": NEEDS_BROKER_RECEIPTS, "GC-A16": NO_EVIDENCE, "GC-A18": NEEDS_BROKER_RECEIPTS, "GC-T01": "needs executor/target fields of a per-stage broker receipt",
  "GC-T02": NEEDS_HOST_TOOL, "GC-T03": NO_EVIDENCE, "GC-T07": NO_EVIDENCE, "GC-T10": NEEDS_FIXTURE, "GC-T11": "needs sealed digests at seal, qualify and promote points",
  "GC-T12": NEEDS_HOST_TOOL, "GC-T13": "needs a second run under an agent environment", "GC-T16": NEEDS_BROKER_RECEIPTS, "GC-T17": "contract conflict with BROKER BR30/item 18; no assertion until reconciled",
  "GC-T18": NEEDS_FIXTURE, "GC-T24": "needs a --changed or --units-file run and its receipt-dir; not part of S0-S9", "GC-I02": NEEDS_BROKER_RECEIPTS,
  "GC-I04": NEEDS_BROKER_RECEIPTS, "GC-I06": NO_EVIDENCE, "GC-I08": NEEDS_FIXTURE, "GC-I11": "needs a `rightkit-gate clippy` run and its receipt",
  "GC-I12": "needs `rustright doctor --json`; not run by this runner (read-only, but an agent hook forbids it here)", "GC-I13": "needs a refusal the gate does not pre-empt (fixture N14) and a SNAPSHOT_LEASE_STATUS query", "GC-I14": NEEDS_FIXTURE,
  "GC-I16": "needs `rustright doctor --json`; not run by this runner", "GC-I17": "needs a live STATUS query", "GC-I19": "needs a live SNAPSHOT_LEASE_STATUS query", "GC-I20": NEEDS_FIXTURE, "GC-I21": NO_EVIDENCE,
  "GC-Q01": NO_EVIDENCE, "GC-Q02": NO_EVIDENCE, "GC-Q03": NO_EVIDENCE, "GC-Q04": NO_EVIDENCE, "GC-Q05": NO_EVIDENCE, "GC-1A-R2": NEEDS_FIXTURE, "GC-1A-R4": "covered by negative N26 when --steps S7 runs",
});

// ---------------------------------------------------------------------------------------------------------------------------------------------
// Pure assertion helpers. Each returns { ok, problems[] } (or a richer record); `problems` is empty on success.
// ---------------------------------------------------------------------------------------------------------------------------------------------

/** Unknown or missing keys of `obj` against `{ required, optional }`; fail closed on both. */
export function strictKeys(obj, { required = [], optional = [] }, where) {
  if (!isObject(obj)) return [`${where}: not an object`];
  const problems = [];
  for (const k of required) if (!(k in obj)) problems.push(`${where}: missing field "${k}"`);
  const known = new Set([...required, ...optional]);
  for (const k of Object.keys(obj)) if (!known.has(k)) problems.push(`${where}: unknown field "${k}"`);
  return problems;
}

const ROW_REQUIRED = ["id", "class", "coverage", "status", "key", "reason"];
const ROW_OPTIONAL = ["code", "timing", "scanElapsedMs", "retryTiming", "batchAttempts", "batchedWith"];
const COVERAGE_VALUES = new Set(["hit", "ran", "skipped"]);

/** GC-A21: the schema of one coverage report (gate-coverage.mjs buildCoverageReport). Fail closed on any unknown field. */
export function validateCoverage(report) {
  const problems = strictKeys(report, { required: ["schema", "commit", "startedAt", "endedAt", "totals", "scans"], optional: ["cacheDisabled"] }, "coverage");
  if (problems.length && !isObject(report)) return { ok: false, problems };
  if (report.schema !== COVERAGE_SCHEMA) problems.push(`coverage.schema is ${JSON.stringify(report.schema)}, expected ${COVERAGE_SCHEMA}`);
  if ("cacheDisabled" in report) problems.push(`coverage.cacheDisabled is set (${JSON.stringify(report.cacheDisabled)}): the run bypassed the cache`);
  if (!Array.isArray(report.scans)) { problems.push("coverage.scans is not an array"); return { ok: false, problems }; }
  problems.push(...strictKeys(report.totals, { required: ["planned", "hit", "ran", "skipped"] }, "coverage.totals"));
  const count = { hit: 0, ran: 0, skipped: 0 };
  const seen = new Set();
  for (const [i, row] of report.scans.entries()) {
    const where = `coverage.scans[${i}]${isObject(row) && typeof row.id === "string" ? ` (${row.id})` : ""}`;
    const bad = strictKeys(row, { required: ROW_REQUIRED, optional: ROW_OPTIONAL }, where);
    problems.push(...bad);
    if (!isObject(row)) continue;
    if (typeof row.id !== "string" || !row.id) problems.push(`${where}: id is not a non-empty string`);
    else if (seen.has(row.id)) problems.push(`${where}: duplicate scan id`);
    else seen.add(row.id);
    if (!COVERAGE_VALUES.has(row.coverage)) { problems.push(`${where}: coverage ${JSON.stringify(row.coverage)} is not hit|ran|skipped`); continue; }
    count[row.coverage] += 1;
    if (typeof row.status !== "string" || !row.status) problems.push(`${where}: status is not a non-empty string`);
    if (typeof row.reason !== "string" || !row.reason) problems.push(`${where}: reason is not a non-empty string`);
    if (!(row.key === null || (typeof row.key === "string" && row.key))) problems.push(`${where}: key is neither null nor a string`);
    if (row.coverage === "hit" && row.status !== "pass") problems.push(`${where}: a hit row has status ${JSON.stringify(row.status)}, expected pass`);
    if (row.coverage === "ran" && row.status !== "pass" && row.status !== "fail") problems.push(`${where}: a ran row has status ${JSON.stringify(row.status)}, expected pass|fail`);
  }
  if (isObject(report.totals)) {
    if (report.totals.planned !== report.scans.length) problems.push(`coverage.totals.planned ${report.totals.planned} != ${report.scans.length} rows`);
    for (const k of ["hit", "ran", "skipped"]) if (report.totals[k] !== count[k]) problems.push(`coverage.totals.${k} ${report.totals[k]} != ${count[k]} rows`);
    if (report.totals.hit + report.totals.ran + report.totals.skipped !== report.totals.planned) problems.push("coverage.totals: hit+ran+skipped != planned");
  }
  return { ok: problems.length === 0, problems };
}

/**
 * Merges the reports of one run (a stage may write one per pass) into a map id -> row. A row that was really answered (hit or ran) wins over a
 * "not-in-pass" skip of the same scan in another pass; two real answers for one id keep the later one.
 */
export function mergeCoverage(reports) {
  const rows = new Map();
  const notInPass = (r) => r.coverage === "skipped" && r.status === "not-in-pass";
  for (const report of reports) for (const row of report.scans) {
    const prev = rows.get(row.id);
    if (!prev || notInPass(prev) || !notInPass(row)) rows.set(row.id, row);
  }
  return rows;
}

/** Rows of a pass that carry no answer: skipped for any reason except "class not part of this pass". They are never counted as a pass. */
export function skippedRows(rows) {
  return [...rows.values()].filter((r) => r.coverage === "skipped" && r.status !== "not-in-pass");
}

/** GC-A21: every non-JSON log line carries a UTC stamp; JSON and the one `verdict deterministic` marker line are exempt. */
const UTC_STAMP = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?Z /;
export function checkUtcStamps(text) {
  const problems = [];
  const lines = String(text).split(/\r?\n/);
  let checked = 0;
  for (const [i, line] of lines.entries()) {
    if (!line.trim()) continue;
    if (/^rightkit-gate: verdict deterministic/.test(line)) continue;
    if (/^\s*[{}\[\]"]/.test(line)) continue; // pretty-printed JSON body
    checked += 1;
    if (!UTC_STAMP.test(line)) problems.push(`log line ${i + 1} has no UTC stamp: ${line.slice(0, 120)}`);
  }
  if (checked === 0) problems.push("the log has no lines to check");
  return { ok: problems.length === 0, problems, checked };
}

/** `[stage] passed in N.Ns` and `failed in` lines of a log: [{ stage, ok, seconds }]. */
export function stageTimings(text) {
  const out = [];
  for (const m of String(text).matchAll(/\[([a-z:]+)\] (passed|failed) in (\d+(?:\.\d+)?)s/g)) out.push({ stage: m[1], ok: m[2] === "passed", seconds: Number(m[3]) });
  return out;
}

/** Coverage report paths a log names: "coverage report <file>: N planned scan(s), ...". */
export function coveragePaths(text) {
  return [...String(text).matchAll(/coverage report (.+?): \d+ planned scan/g)].map((m) => m[1]);
}

/** The reclaim line of a gate: "build ownership held <handle> (reclaimed from <from>: <proofs>)". null when the run reclaimed nothing. */
export function reclaimLine(text) {
  const m = /build ownership (held|joined) (\S+) \(reclaimed from (.+?): (.*)\)\s*$/m.exec(String(text));
  return m ? { mode: m[1], handle: m[2], from: m[3].trim(), proofs: m[4] } : null;
}

const STAGE_FIELDS = ["ok", "at", "ruleTier", "direct", "qualifyOwed", "outputs", "snapshot", "ownership", "platform", "sealed", "manifestSha256", "voided", "track", "qaReceiptDigest", "tier"];
const BINDING_FIELDS = ["tree", "locks", "gate", "toolchain", "executor"];

/**
 * GC-A04, GC-I05 (door half), GC-A05: the door receipt `--receipt-out` writes. `expectedTree` is `git rev-parse <commit>^{tree}`. Fail closed on unknown
 * fields; `verdict` and `refreshed` must be absent (the broker never writes them into a gate receipt).
 */
export function validateDoorReceipt(receipt, { commit, expectedTree, stages = [] }) {
  const problems = strictKeys(receipt, { required: ["schema", "commit", "app", "binding", "stages"], optional: ["brokerException"] }, "receipt");
  if (!isObject(receipt)) return { ok: false, problems };
  if (receipt.schema !== RECEIPT_SCHEMA) problems.push(`receipt.schema is ${JSON.stringify(receipt.schema)}, expected ${RECEIPT_SCHEMA}`);
  if (receipt.commit !== commit) problems.push(`receipt.commit ${receipt.commit} != ${commit}`);
  if ("brokerException" in receipt) problems.push("receipt.brokerException is set: the run was built outside the broker");
  for (const banned of ["verdict", "refreshed"]) if (banned in receipt) problems.push(`receipt carries "${banned}" (refused by the gate)`);
  problems.push(...strictKeys(receipt.binding, { required: BINDING_FIELDS }, "receipt.binding"));
  if (isObject(receipt.binding)) {
    if (receipt.binding.tree !== expectedTree) problems.push(`receipt.binding.tree ${receipt.binding.tree} != ${expectedTree} (commit^{tree})`);
    if (!isSha(receipt.binding.gate)) problems.push("receipt.binding.gate is not a sha256");
  }
  if (!isObject(receipt.stages)) { problems.push("receipt.stages is not an object"); return { ok: false, problems }; }
  for (const stage of stages) {
    const s = receipt.stages[stage];
    const where = `receipt.stages.${stage}`;
    if (!isObject(s)) { problems.push(`${where}: missing`); continue; }
    problems.push(...strictKeys(s, { required: ["ok", "at"], optional: STAGE_FIELDS.slice(2) }, where));
    if (s.ok !== true) problems.push(`${where}.ok is ${JSON.stringify(s.ok)}`);
    if ("voided" in s) problems.push(`${where}.voided is ${JSON.stringify(s.voided)}`);
    if (!Number.isFinite(Date.parse(s.at))) problems.push(`${where}.at is not a timestamp`);
    if (stage !== "check" && !isObject(s.snapshot)) problems.push(`${where}.snapshot is missing`);
    if (isObject(s.snapshot)) {
      if (s.snapshot.tree !== expectedTree) problems.push(`${where}.snapshot.tree ${s.snapshot.tree} != ${expectedTree}`);
      if (s.snapshot.intactAfter !== true) problems.push(`${where}.snapshot.intactAfter is ${JSON.stringify(s.snapshot.intactAfter)} (the snapshot must still be exactly the commit's bytes at stage exit)`);
      if (typeof s.snapshot.path !== "string" || !s.snapshot.path) problems.push(`${where}.snapshot.path is missing`);
    }
  }
  return { ok: problems.length === 0, problems };
}

/**
 * Whether the compile of a run really ran in that run. A recorded compile that "stands" is reused (`[compile] already passed ...`; the contract's E7
 * says no redundant compile is required, and the receipt keeps the OLD stage `at`), so only a run whose log has `[compile] passed in` and no
 * `already passed` line for the compile proves anything about ordering or about what a compile reads.
 */
export function compileRanFresh(log) {
  const t = String(log ?? "");
  return /\[compile\] passed in/.test(t) && !/\[compile\] already passed/.test(t);
}

/**
 * GC-T05: Class A (stage check) is finished before the compile stage starts. With `log` (the run's own log) the proof is: the compile really ran in
 * this run, the `[check] passed in` line's UTC stamp is not later than the receipt's compile `at` (the moment the compile stage began), and the check
 * stage is ok. A reused compile is returned as `{ ok: false, skipped: true }`: nothing about ordering can be read from another run's `at`.
 * Without `log` only the receipt timestamps are compared (kept for callers that hold no log).
 */
export function classABeforeCompile(receipt, log = null) {
  const check = Date.parse(receipt?.stages?.check?.at);
  const compile = Date.parse(receipt?.stages?.compile?.at);
  if (!Number.isFinite(check) || !Number.isFinite(compile)) return { ok: false, problems: ["receipt.stages.check.at and receipt.stages.compile.at are both needed"] };
  if (receipt.stages.check.ok !== true) return { ok: false, problems: [`receipt.stages.check.ok is ${JSON.stringify(receipt.stages.check.ok)}`] };
  if (log === null) return check <= compile ? { ok: true, problems: [] } : { ok: false, problems: [`check at ${receipt.stages.check.at} is after compile at ${receipt.stages.compile.at}`] };
  if (!compileRanFresh(log)) return { ok: false, skipped: true, problems: ["the compile was reused from an earlier run (`[compile] already passed`); its recorded `at` is not this run's, so no ordering can be read"] };
  const m = /^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z) rightkit-gate: \[check\] passed in/m.exec(String(log));
  if (!m) return { ok: false, problems: ["the log has no stamped `[check] passed in` line"] };
  const checkEnd = Date.parse(m[1]);
  return checkEnd <= compile ? { ok: true, problems: [] } : { ok: false, problems: [`Class A ended at ${m[1]}, after the compile stage began at ${receipt.stages.compile.at}`] };
}

/** GC-A15, GC-A07: the output dirs a receipt records (stage outputs.dirs) are one set across stages, and none is a debug dir. */
export function outputDirsProblems(receipt) {
  const problems = [];
  const sets = [];
  for (const [stage, s] of Object.entries(receipt?.stages ?? {})) {
    const dirs = s?.outputs?.dirs;
    if (dirs === undefined) continue;
    if (!Array.isArray(dirs) || dirs.some((d) => typeof d !== "string")) { problems.push(`receipt.stages.${stage}.outputs.dirs is malformed`); continue; }
    sets.push([stage, [...dirs].sort()]);
    if (s.outputs.truncated === true) problems.push(`receipt.stages.${stage}.outputs.truncated is true`);
    for (const d of dirs) if (/(^|[\\/])debug([\\/]|$)/.test(d)) problems.push(`receipt.stages.${stage}.outputs.dirs names a debug directory: ${d}`);
  }
  if (sets.length === 0) problems.push("no stage of the receipt records outputs.dirs");
  for (const [stage, dirs] of sets.slice(1)) if (JSON.stringify(dirs) !== JSON.stringify(sets[0][1])) problems.push(`output dirs of ${stage} differ from ${sets[0][0]}`);
  return { ok: problems.length === 0, problems };
}

/**
 * The key GC-I01 names: sha256 of the canonical repository path, first 16 hex digits, over the path as the gate keys it. On Windows that is the
 * verbatim `\\?\` spelling, so this imports the gate's own `keyPath` and `repoKey` (snapshot.mjs) instead of hashing the bare path.
 */
export const repositoryKey = (canonicalPath, platform = process.platform) => repoKey(keyPath(canonicalPath, platform));

/** GC-A23: the snapshot's `.rightkit-source-identity.json`. `trees` is `{ dir: tree id }` recomputed from git by the caller. */
export function validateSourceIdentity(identity, { commit, trees }) {
  const problems = strictKeys(identity, { required: ["schema", "commit", "trees"] }, "source-identity");
  if (!isObject(identity)) return { ok: false, problems };
  if (identity.schema !== SOURCE_IDENTITY_SCHEMA) problems.push(`source-identity.schema is ${JSON.stringify(identity.schema)}`);
  if (identity.commit !== commit) problems.push(`source-identity.commit ${identity.commit} != ${commit}`);
  if (!isObject(identity.trees)) return { ok: false, problems: [...problems, "source-identity.trees is not an object"] };
  if (!("" in identity.trees)) problems.push('source-identity.trees has no "" (root) entry');
  for (const [dir, tree] of Object.entries(trees)) if (identity.trees[dir] !== tree) problems.push(`source-identity.trees[${JSON.stringify(dir)}] is ${identity.trees[dir]}, git says ${tree}`);
  for (const dir of Object.keys(identity.trees)) if (!(dir in trees)) problems.push(`source-identity.trees has an unexpected entry ${JSON.stringify(dir)}`);
  return { ok: problems.length === 0, problems };
}

/** GC-A22 render: the receipt a `--to render` run stores. `png(name)` returns the PNG bytes of a case (or null). */
export function validateRenderReceipt(receipt, { commit, png }) {
  const problems = strictKeys(receipt, { required: ["schema", "commit", "tree", "appDir", "package", "bin", "features", "executable", "buildOutputDir", "host", "startedAt", "endedAt", "cases"], optional: ["buildRequestId"] }, "render-receipt");
  if (!isObject(receipt)) return { ok: false, problems };
  if (receipt.schema !== RENDER_RECEIPT_SCHEMA) problems.push(`render-receipt.schema is ${JSON.stringify(receipt.schema)}`);
  if (receipt.commit !== commit) problems.push(`render-receipt.commit ${receipt.commit} != ${commit}`);
  if (!isSha(receipt.executable?.sha256)) problems.push("render-receipt.executable.sha256 is not a sha256");
  if (typeof receipt.executable?.path !== "string" || typeof receipt.buildOutputDir !== "string" || !receipt.executable.path.startsWith(receipt.buildOutputDir)) problems.push("render-receipt.executable.path does not lie inside buildOutputDir");
  if (!Array.isArray(receipt.cases) || receipt.cases.length === 0) { problems.push("render-receipt.cases is empty"); return { ok: false, problems }; }
  for (const c of receipt.cases) {
    const where = `render-receipt.cases[${c?.name}]`;
    problems.push(...strictKeys(c, { required: ["name", "png", "sha256", "bytes", "width", "height"] }, where));
    const bytes = isObject(c) ? png(c.png) : null;
    if (!bytes) { problems.push(`${where}: PNG ${c?.png} cannot be read`); continue; }
    if (!(bytes.length > 24 && bytes.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])))) { problems.push(`${where}: not a PNG (bad magic)`); continue; }
    if (sha256(bytes) !== c.sha256 || bytes.length !== c.bytes) problems.push(`${where}: sha256 or size differs from the receipt`);
    if (bytes.readUInt32BE(16) !== c.width || bytes.readUInt32BE(20) !== c.height) problems.push(`${where}: PNG width/height differ from the receipt`);
  }
  return { ok: problems.length === 0, problems };
}

/** GC-I22: vendored vector files equal the sha256 their vectors-source.json pins and RustRight's own bytes; a missing upstream directory fails, it is not skipped. */
export function vectorsProblems({ source, files, upstream = null, upstreamDir = null }) {
  const problems = strictKeys(source, { required: ["schema", "note", "source", "files"] }, "vectors-source");
  if (!isObject(source?.files)) return { ok: false, problems: [...problems, "vectors-source.files missing"] };
  for (const [name, meta] of Object.entries(source.files)) {
    const bytes = files[name];
    if (!bytes) { problems.push(`${name}: pinned but not vendored`); continue; }
    if (sha256(bytes) !== meta.sha256) problems.push(`${name}: vendored sha256 ${sha256(bytes).slice(0, 12)} != pinned ${String(meta.sha256).slice(0, 12)}`);
    if (!upstream) continue;
    if (!upstream[name]) problems.push(`${name}: RustRight's docs/contracts has no copy to compare with`);
    else if (sha256(upstream[name]) !== sha256(bytes)) problems.push(`${name}: differs from RustRight's docs/contracts copy`);
  }
  if (!upstream) problems.push(`RustRight's docs/contracts${upstreamDir ? ` (${upstreamDir})` : ""} was not found: the vendored vectors cannot be compared with their source`);
  for (const name of Object.keys(files)) if (name !== "vectors-source.json" && !(name in source.files)) problems.push(`${name}: vendored but not pinned`);
  return { ok: problems.length === 0, problems };
}

/** GC-I18 (static half): the digest the gate pins is the installed runtime's. */
export function brokerDigestProblems({ manifest, pins }) {
  const problems = [];
  if (!isObject(manifest) || !isSha(manifest.brokerDigest)) problems.push("runtime manifest has no brokerDigest sha256");
  if (!isObject(pins) || !isSha(pins.brokerDigest)) problems.push("broker pins record has no brokerDigest sha256");
  if (!problems.length && manifest.brokerDigest !== pins.brokerDigest) problems.push(`pins brokerDigest ${pins.brokerDigest.slice(0, 12)} != runtime manifest brokerDigest ${manifest.brokerDigest.slice(0, 12)}`);
  return { ok: problems.length === 0, problems };
}

const WARM_UNCACHED_CLASS_A = /^(pnpm-lockfile|advisory-exceptions)(:|$)/;

/**
 * S2 (GC-A24, GC-A19): a warm rerun of the same commit. Against the first run `prev` (id -> row) and the rerun `next`:
 *  - every scan of the rerun that is not an uncached family answered `hit` (a `ran` there is a failure, a skipped row is a failure);
 *  - the two uncached families (pnpm-lockfile, advisory-exceptions) ran, never hit;
 *  - the key of every row equals the first run's key byte for byte.
 */
export function assertWarmRerun(prev, next) {
  const problems = [];
  let hits = 0;
  for (const [id, row] of next) {
    if (row.coverage === "skipped" && row.status === "not-in-pass") continue;
    const uncached = row.class === "A" && WARM_UNCACHED_CLASS_A.test(id);
    if (row.coverage === "skipped") { problems.push(`${id}: skipped (${row.reason}); a skipped row is never a pass`); continue; }
    if (uncached) { if (row.coverage !== "ran") problems.push(`${id}: uncached family answered ${row.coverage}, expected ran`); continue; }
    if (row.class === "A" && row.coverage !== "hit") problems.push(`${id}: Class A row ${row.coverage} on an unchanged commit, expected hit${row.key === null ? " (no cache key)" : ""}`);
    if (row.class === "A" && row.coverage === "hit") hits += 1;
    const before = prev.get(id);
    if (row.class === "A" && before && before.key !== row.key) problems.push(`${id}: key changed between runs on one commit (${before.key} -> ${row.key})`);
    if (row.class === "A" && !before) problems.push(`${id}: not present in the first run`);
  }
  if (hits === 0) problems.push("no Class A row hit: the rerun proves nothing about warmth");
  return { ok: problems.length === 0, problems, hits };
}

/**
 * S3 (GC-A24): one `.rs` edit. Exactly one `rustfmt` row `ran` (that file's), plus any per-file rows that read it (secret-scan, licence); every other
 * Class A key equals the previous run. `perFile` is a RegExp of the row ids allowed to run besides rustfmt.
 */
export function assertOneFileEdit(prev, next, { perFile = /^(secret-scan|licen[cs]e)/ } = {}) {
  const problems = [];
  const ranA = [...next.values()].filter((r) => r.class === "A" && r.coverage === "ran" && !WARM_UNCACHED_CLASS_A.test(r.id));
  const rustfmt = ranA.filter((r) => r.id.startsWith("rustfmt"));
  if (rustfmt.length !== 1) problems.push(`${rustfmt.length} rustfmt rows ran, expected exactly 1 (${rustfmt.map((r) => r.id).join(", ")})`);
  for (const r of ranA) if (!r.id.startsWith("rustfmt") && !perFile.test(r.id)) problems.push(`${r.id}: ran but does not read the edited file`);
  // A row that ran because its input changed has a new key; a ran row that kept its key is an unexplained miss (and a row without a key cannot show a change).
  for (const r of ranA) {
    const before = prev.get(r.id);
    if (!before) problems.push(`${r.id}: ran but is absent from the previous run, so no changed key can be shown`);
    else if (r.key === null || before.key === r.key) problems.push(`${r.id}: ran with ${r.key === null ? "no cache key" : "the same key as the previous run"}: an edit changes the key of every row that reads it`);
  }
  for (const [id, row] of next) {
    if (row.class !== "A" || row.coverage === "skipped") continue;
    const before = prev.get(id);
    if (row.coverage === "hit" && before && before.key !== row.key) problems.push(`${id}: key changed but the row answered hit`);
    if (row.coverage === "hit" && !before) problems.push(`${id}: hit but absent from the previous run`);
  }
  return { ok: problems.length === 0, problems, ran: ranA.map((r) => r.id) };
}

const metadataLabel = (id) => (id === "cargo-metadata" ? "." : id.startsWith("cargo-metadata:") ? id.slice("cargo-metadata:".length) : null);
const packageListUnit = (id) => (id.startsWith("cargo-package-list:") ? id.slice("cargo-package-list:".length) : id === "cargo-package-list" ? "" : null);

/**
 * S4 (GC-A19): a manifest edit reruns `cargo-metadata` and `cargo-package-list` for the edited manifest's workspace ONLY. `workspace` is that
 * workspace's directory ("." for the root), `workspaces` every workspace directory of the commit and `crates` `{ workspaceDir: [crate names] }`.
 * The workspace's own `cargo-metadata` row must have run with a new key; any `cargo-metadata` or `cargo-package-list` row of another workspace that
 * ran, and any such row that cannot be attributed to a workspace, fails.
 */
export function assertManifestEdit(prev, next, { workspace, workspaces = [], crates = {} } = {}) {
  const problems = [];
  if (typeof workspace !== "string") return { ok: false, problems: ["the edited manifest's workspace is not known"], ran: [] };
  const wsOfLabel = (label) => {
    if (workspaces.includes(label)) return label;
    const byCrate = workspaces.find((w) => w === `crates/${label}`);
    return byCrate ?? null;
  };
  const crateWs = (unit) => {
    const name = unit.includes(":") ? unit.slice(unit.lastIndexOf(":") + 1) : unit;
    const owners = Object.entries(crates).filter(([, names]) => names.includes(name)).map(([w]) => w);
    return owners.length === 1 ? owners[0] : null;
  };
  const ran = [];
  let ownMetadata = false;
  for (const row of next.values()) {
    const label = metadataLabel(row.id);
    const unit = packageListUnit(row.id);
    if (label === null && unit === null) continue;
    const ws = label !== null ? (workspaces.length === 0 && label === "." ? "." : wsOfLabel(label)) : crateWs(unit);
    const own = ws === workspace;
    if (row.coverage === "skipped" && row.status !== "not-in-pass") problems.push(`${row.id}: skipped (${row.reason}); a skipped row is never a pass`);
    if (row.coverage === "ran") {
      ran.push(row.id);
      if (ws === null) problems.push(`${row.id}: ran but belongs to no known workspace`);
      else if (!own) problems.push(`${row.id}: ran for workspace ${JSON.stringify(ws)}, which the edit (workspace ${JSON.stringify(workspace)}) does not touch`);
      else {
        const before = prev?.get(row.id);
        if (!before || before.key === row.key) problems.push(`${row.id}: ran without a changed key (${before ? "same key as before" : "absent before"})`);
      }
    }
    if (own && label !== null) {
      ownMetadata = true;
      if (row.coverage !== "ran") problems.push(`${row.id}: the edited workspace's cargo-metadata answered ${row.coverage}, expected ran`);
    }
    if (own && unit !== null && row.coverage === "hit") problems.push(`${row.id}: a cargo-package-list row of the edited workspace answered hit`);
  }
  if (!ownMetadata) problems.push(`no cargo-metadata row for workspace ${JSON.stringify(workspace)} exists in the run`);
  return { ok: problems.length === 0, problems, ran };
}

/**
 * A negative case: exit non-zero, every named signal in the output, and "zero later work": no stage after the failing one passed. `signals` are
 * the exact rule codes the gate emits (RegExp or string; every one must match). `expectRows` name coverage rows the gate must have reported for the
 * scan that refused: `{ id: string|RegExp, coverage, status }`; they are read from `rows` (the run's merged coverage report, id -> row), and no report
 * is a failure. `laterStages` are stage names that must not have `passed in` lines. A bare word in the log is never a signal.
 */
export function assertNegative({ exit, text, signals = [], laterStages = [], receipt = null, rows = null, expectRows = [] }) {
  const problems = [];
  if (exit === 0) problems.push("exit 0: the gate accepted a broken input");
  if (exit === null) problems.push("the gate did not exit (killed or timed out)");
  for (const s of signals) if (!(s instanceof RegExp ? s.test(text) : text.includes(s))) problems.push(`output does not name ${s}`);
  if (expectRows.length) {
    if (!rows || rows.size === 0) problems.push("no coverage report was read, so the scan that refused cannot be named");
    else for (const want of expectRows) {
      const hit = [...rows.values()].find((r) => (want.id instanceof RegExp ? want.id.test(r.id) : r.id === want.id) && r.coverage === want.coverage && r.status === want.status);
      if (!hit) problems.push(`no coverage row ${want.id} ${want.coverage}/${want.status}: ${[...rows.values()].filter((r) => (want.id instanceof RegExp ? want.id.test(r.id) : r.id === want.id)).map((r) => `${r.id} ${r.coverage}/${r.status}`).join(", ") || "none with that id"}`);
    }
  }
  const passed = new Set(stageTimings(text).filter((t) => t.ok).map((t) => t.stage));
  for (const stage of laterStages) {
    if (passed.has(stage)) problems.push(`stage ${stage} passed after the failure`);
    if (receipt?.stages?.[stage]?.ok === true) problems.push(`receipt.stages.${stage}.ok is true after the failure`);
  }
  return { ok: problems.length === 0, problems };
}

/** The release-tier app-check rules (app-check.mjs: tierOf) and the baseline-growth finding; each prints `FAIL <rule> (<gap>): ...` / `UNAPPROVED baseline entry`. */
export const RELEASE_TIER_RULES = Object.freeze(["mandatory-foundation", "ownership-exceptions", "trust-policy-exclude", "commit-citations"]);

/** S1: a refused `--to qualify` must name a release-tier rule by the exact line the check prints, and record no ok qualify stage. */
export function qualifyRefusal({ exit, text, receipt = null }) {
  const problems = [];
  if (exit === 0) problems.push("exit 0: not a refusal");
  if (exit === null) problems.push("the gate did not exit (killed or timed out)");
  const named = RELEASE_TIER_RULES.filter((id) => new RegExp(`\\bFAIL ${id} \\(`).test(text));
  if (/\bUNAPPROVED baseline entry\b/.test(text)) named.push("UNAPPROVED");
  if (!named.length) problems.push("the refusal names no release-tier rule (expected a `FAIL <rule> (<gap>)` line for one of " + `${RELEASE_TIER_RULES.join(", ")}` + " or `UNAPPROVED baseline entry`)");
  if (receipt?.stages?.qualify?.ok === true) problems.push("the receipt records qualify ok after a refusal");
  return { ok: problems.length === 0, problems, named };
}

const SIGNUM = Object.freeze({ SIGINT: 2, SIGKILL: 9, SIGTERM: 15 });
/** Whether a stopped gate was stopped by `killSignal`: the signal itself, or 128+n (a shell wrapper); on Windows the code `taskkill /F` gives (1). */
export function exitedBySignal({ signal, exit, killSignal, platform = process.platform }) {
  if (platform === "win32") return exit === 1 && (signal === null || signal === undefined);
  if (signal) return signal === killSignal;
  return exit !== null && exit === 128 + (SIGNUM[killSignal] ?? -1000);
}

/**
 * Kill-and-recover (K01, SIGKILL; also the SIGINT variant): `before` is the owner record read right after the kill (or null when none), `dry` the
 * ownership-recover.mjs dry-run `{ exit, text }`, `next` the following gate run `{ exit, text }`, `after` the owner record read after it (null: removed).
 * Pass when the next run starts without any operator step and either reclaims (log names the reclaim, handle replaced) or finds nothing held
 * (SIGINT released it). A record that survives a SIGKILL must be explained by the dry run and reclaimed by the next run.
 */
export function assertRecovery({ signal, before, dry, next, after, exitedBySignal = true }) {
  const problems = [];
  if (!exitedBySignal) problems.push(`the gate was not stopped by ${signal}`);
  if (signal === "SIGKILL" && !before) problems.push("no owner record stood after SIGKILL: the gate released nothing, so it must have been killed before acquiring; the case proved nothing");
  if (before) {
    if (!isObject(before) || typeof before.handle !== "string" || !Number.isInteger(before.pid)) problems.push("the owner record is malformed (no handle or pid)");
    if (dry?.exit !== 0 || !/recover: recoverable:/.test(dry?.text ?? "")) problems.push(`ownership-recover.mjs dry run did not explain a recoverable record (exit ${dry?.exit}): ${(dry?.text ?? "").split("\n").filter((l) => /recover:/.test(l)).slice(-2).join(" | ").slice(0, 240)}`);
    const claim = reclaimLine(next?.text ?? "");
    if (!claim) problems.push("the next run's log has no 'reclaimed from' line");
    else if (isObject(before) && claim.handle === before.handle) problems.push("the next run holds the dead owner's own handle: the record was not replaced");
    if (isObject(before) && isObject(after) && after.handle === before.handle) problems.push("the owner record still carries the dead handle after the next run");
  }
  if (next?.exit !== 0) problems.push(`the next run exited ${next?.exit}`);
  return { ok: problems.length === 0, problems };
}

/** S9: two git listings taken at different times must be byte equal. */
export function sameListing(name, a, b) {
  return a === b ? { ok: true, problems: [] } : { ok: false, problems: [`${name} changed: ${JSON.stringify(a).slice(0, 160)} -> ${JSON.stringify(b).slice(0, 160)}`] };
}

/** S9 and GC-A01: the one-worktree rule (`git worktree list --porcelain` has one `worktree ` row). */
export const worktreeCount = (porcelain) => String(porcelain).split(/\r?\n/).filter((l) => l.startsWith("worktree ")).length;

/** GC-A01 and N13: a bypass refusal names the retired option and ran nothing. */
export function bypassRefused({ exit, text }) {
  const problems = [];
  if (exit === 0 || exit === null) problems.push(`exit ${exit}: the bypass was not refused`);
  if (!/retired/.test(text)) problems.push("the refusal text does not say the option is retired");
  if (/\[(check|compile)\] (passed|failed) in/.test(text)) problems.push("a stage ran under a refused bypass");
  return { ok: problems.length === 0, problems };
}

/** S9: no publish or upload happened (GC-T22 negative): log markers of an upload that completed. */
export function publicationMarkers(text) {
  return String(text).split(/\r?\n/).filter((l) => /\b(uploaded to|upload complete|published release|gh release create|r2 put)\b/i.test(l)).slice(0, 5);
}

/**
 * GC-A01, GC-T14: every directory of the snapshot root that belongs to this repository (its name starts with the repository key: the snapshot, a
 * leftover `.staging`, any second spelling) must be exactly `<key>`; and no directory that was not there at S0 may have appeared besides it.
 * `before` and `after` are directory-name listings of `<managed root>/snapshots` (`before` null: S0 did not run).
 */
export function snapshotDirProblems({ key, before = null, after }) {
  const problems = [];
  const ours = after.filter((n) => n.startsWith(key));
  if (!ours.includes(key)) problems.push(`the snapshot dir ${key} does not exist`);
  for (const n of ours) if (n !== key) problems.push(`a second snapshot dir for the repository: ${n}`);
  if (before) for (const n of after) if (!before.includes(n) && !n.startsWith(key)) problems.push(`a new snapshot dir appeared during the run: ${n}`);
  return { ok: problems.length === 0, problems, dirs: ours };
}

/** GC-T20: no hook of the fixture's hooks directory (`.sample` files excepted) mentions the gate. `files` is `{ name: text }`. */
export function hookProblems({ files, hooksPath = null }) {
  const problems = [];
  for (const [name, text] of Object.entries(files)) {
    if (name.endsWith(".sample")) continue;
    if (/rightkit-gate|rightkit-host-gate|rightkit_gate|gate\.mjs/i.test(String(text))) problems.push(`hook ${name} runs the gate`);
  }
  return { ok: problems.length === 0, problems };
}

/**
 * S6 (GC-A04, GC-A02, GC-T06): proof, read while the checkout is dirty, that the snapshot of the compile holds the COMMIT's bytes and not the
 * checkout's. `commitBytes` is the edited file as the commit has it, `checkoutBytes` as the dirty checkout has it, `snapshotBytes` as the snapshot
 * has it (null: absent), `untrackedInSnapshot` whether the untracked file reached the snapshot, `identityProblems` the source-identity check of the
 * snapshot against the commit's trees, and `receiptTree`/`expectedTree` the tree the compile bound against `commit^{tree}`.
 */
export function dirtyProof({ commitBytes, checkoutBytes, snapshotBytes, untrackedInSnapshot, identityProblems = [], receiptTree, expectedTree }) {
  const problems = [...identityProblems];
  if (!Buffer.isBuffer(commitBytes) || !Buffer.isBuffer(checkoutBytes)) problems.push("the commit's or the checkout's bytes of the edited file are missing");
  else if (commitBytes.equals(checkoutBytes)) problems.push("the checkout file equals the commit's: the checkout was not dirty, so the case proves nothing");
  if (!Buffer.isBuffer(snapshotBytes)) problems.push("the snapshot does not hold the edited file");
  else {
    if (Buffer.isBuffer(commitBytes) && !snapshotBytes.equals(commitBytes)) problems.push("the snapshot's file differs from the commit's bytes");
    if (Buffer.isBuffer(checkoutBytes) && snapshotBytes.equals(checkoutBytes)) problems.push("the snapshot's file carries the dirty checkout's bytes");
  }
  if (untrackedInSnapshot) problems.push("the untracked checkout file reached the snapshot");
  if (receiptTree !== expectedTree) problems.push(`the compile's tree ${receiptTree} != commit^{tree} ${expectedTree}`);
  return { ok: problems.length === 0, problems };
}

/**
 * How to start a gate launcher. A `.cmd`/`.bat` launcher cannot be spawned directly on Windows (Node refuses it): it goes through `cmd.exe /d /s /c`
 * with verbatim arguments. Every argument and the launcher path must be free of cmd metacharacters (`" % ^ & | < > CR LF`); one that is not throws.
 */
export function launchSpec(gate, args, { platform = process.platform, comspec = process.env.ComSpec || "cmd.exe" } = {}) {
  if (platform !== "win32" || !/\.(cmd|bat)$/i.test(gate)) return { command: gate, args, options: {} };
  const unsafe = /["%^&|<>\r\n]/;
  for (const a of [gate, ...args]) if (unsafe.test(a)) throw new Error(`argument ${JSON.stringify(a)} is not safe to pass through cmd.exe`);
  const quote = (a) => (a === "" || /\s/.test(a) ? `"${a}"` : a);
  return { command: comspec, args: ["/d", "/s", "/c", `"${[gate, ...args].map(quote).join(" ")}"`], options: { windowsVerbatimArguments: true } };
}

// ---------------------------------------------------------------------------------------------------------------------------------------------
// The run sequence (criteria.md section 3) and the clauses each step asserts.
// ---------------------------------------------------------------------------------------------------------------------------------------------
export const LADDER = Object.freeze(["check", "compile", "test", "render", "dev", "package", "qualify"]);
/** Stages that install an app on the host; they run only with `--allow-install` (Adrian's approval), never by default. */
export const INSTALL_STAGES = Object.freeze(["dev", "qualify"]);
export const ladderStages = ({ allowInstall = false } = {}) => LADDER.filter((s) => allowInstall || !INSTALL_STAGES.includes(s));
const RECEIPT_STAGES = new Set(["check", "compile", "package", "qualify"]);

export const NEGATIVES = Object.freeze([
  { id: "N01", title: "misformatted .rs", signals: [], expectRows: [{ id: /^rustfmt:/, coverage: "ran", status: "fail" }], later: ["compile"], clauses: ["GC-T05"], build: "misformat-rs" },
  { id: "N02", title: "secret-shaped string in a tracked file", signals: [], expectRows: [{ id: "secret-scan", coverage: "ran", status: "fail" }], later: ["compile"], clauses: ["GC-T05", "GC-A22-classA"], build: "secret-file" },
  { id: "N13", title: "--no-cache and RK_GATE_CACHE=0 refused", signals: [], later: ["check"], clauses: ["GC-A01", "GC-T14"], build: "bypass" },
  { id: "N15", title: "[profile.dev] in a Cargo.toml", signals: [/\[profile-extra\]/, "[profile.dev] is not allowed"], later: ["compile"], clauses: ["GC-A15"], build: "profile-dev" },
  { id: "N26", title: "extra Cargo workspace not in ci/gate.config.json", signals: [/RK_CARGO_WORKSPACE_LIST_DIFFERS/], later: ["compile"], clauses: ["GC-1A-R4"], build: "extra-workspace" },
  { id: "N27", title: "--no-broker x with no exception entry", signals: [/RK_BROKER_EXCEPTION_MISSING/], later: ["compile"], clauses: ["GC-1A-R2"], build: "no-broker", args: ["--no-broker", "acceptance-no-such-exception"] },
]);
export const NEGATIVES_UNCHECKED = Object.freeze({
  N03: "needs a Cargo.toml/Cargo.lock pair edit the fixture's resolver must agree with", N04: NEEDS_FIXTURE, N05: NEEDS_FIXTURE, N06: NEEDS_FIXTURE, N07: NEEDS_FIXTURE, N08: NEEDS_FIXTURE, N09: NEEDS_FIXTURE,
  N10: "snapshot tamper is a write into the gate's snapshot dir; run by hand under the lead's approval", N11: "covered by S6", N12: "needs a build window (the `slow` crate, F2)", N14: NEEDS_FIXTURE, N16: NEEDS_FIXTURE,
  N17: NEEDS_FIXTURE, N18: "needs a broker endpoint override the runner does not set", N19: NEEDS_FIXTURE, N20: NEEDS_FIXTURE, N21: NEEDS_FIXTURE, N22: NEEDS_FIXTURE, N23: NEEDS_FIXTURE, N24: NEEDS_FIXTURE,
  N25: NEEDS_FIXTURE, N28: NEEDS_FIXTURE, N29: NEEDS_FIXTURE, N30: NEEDS_FIXTURE, N31: NEEDS_FIXTURE, N33: NEEDS_FIXTURE, N34: NEEDS_FIXTURE, N35: NEEDS_FIXTURE, N36: NEEDS_FIXTURE, N37: NEEDS_FIXTURE,
});
export const KILLS = Object.freeze([
  { id: "K01", signal: "SIGKILL", at: "owner record exists (build ownership held) plus --kill-after-ms, during --to compile", clauses: ["GC-T06"] },
  { id: "K01-int", signal: "SIGINT", at: "same point as K01", clauses: ["GC-T06"] },
  { id: "K02", signal: "SIGKILL", at: "during --to check, after the owner record exists (ownership is held across Class A by design, build-ownership.mjs:3-4)", clauses: ["GC-A24"] },
]);
export const KILLS_UNCHECKED = Object.freeze({
  K03: "no defined kill point inside the snapshot sync is observable from outside the gate", K04: "the resident gate-serve process is not identifiable by the runner", K05: "needs a hook between broker submit and acknowledgement", K06: NEEDS_FIXTURE,
});

export const STEPS = Object.freeze([
  { id: "S0", title: "preflight (static)", runs: [], clauses: ["GC-I22", "GC-I18", "GC-I01", "GC-I15", "GC-T20"], note: "fixture clean, listings recorded, vector hashes, broker digest manifest vs pins" },
  { id: "S1", title: "baseline ladder on C0", runs: ladderStages().map((s) => `--to ${s}`), clauses: ["GC-A04", "GC-A15", "GC-A21", "GC-A22-render", "GC-A22-classA", "GC-A23", "GC-T04", "GC-T05", "GC-T19", "GC-I01", "GC-I03", "GC-I05"], note: "--to dev and --to qualify install an app: only with --allow-install (Adrian's approval); with it, --to qualify may refuse with a named release-tier rule, asserted as such" },
  { id: "S2", title: "warm rerun, same commit", runs: ["--to check", "--to compile"], clauses: ["GC-A24", "GC-A19", "GC-A21"], note: "Class A rows hit with first-run keys; only pnpm-lockfile and advisory-exceptions run" },
  { id: "S3", title: "one-file edit (ephemeral commit)", runs: ["--to check"], clauses: ["GC-A24"], note: "needs a tracked rk/**/src/lib.rs (or --leaf-file); else unchecked" },
  { id: "S4", title: "manifest edit (ephemeral commit)", runs: ["--to check"], clauses: ["GC-A19"], note: "needs a tracked Cargo.toml (or --manifest-file); else unchecked" },
  { id: "S5", title: "lock edit", runs: [], clauses: ["GC-A19", "GC-A24"], note: "unchecked by design: a lock edit needs the fixture's own resolver; not generated by this runner", byDesignUnchecked: true },
  { id: "S6", title: "dirty checkout", runs: ["--to compile"], clauses: ["GC-A02", "GC-A04", "GC-T06"], note: "refuses a dirty fixture; ephemeral commit (a compile not yet recorded) built while the checkout has a dirty tracked edit, an untracked file and a pushed stash; the snapshot is compared with the commit; the checkout and the stash list are restored exactly" },
  { id: "S7", title: "negatives", runs: NEGATIVES.map((n) => n.id), clauses: ["GC-T05", "GC-A01", "GC-T14", "GC-A15", "GC-1A-R2", "GC-1A-R4"], note: "ephemeral commits under refs/witness/*; the rest listed unchecked with their reason" },
  { id: "S8", title: "kill and recover", runs: KILLS.map((k) => k.id), clauses: ["GC-T06", "GC-A24"], note: "SIGKILL (during compile and during Class A) and SIGINT; ownership-recover.mjs dry run must explain the record; the next run reclaims it and passes" },
  { id: "S9", title: "end invariants", runs: [], clauses: ["GC-A01", "GC-A02", "GC-T14", "GC-T06", "GC-I15", "GC-T20"], note: "listings equal S0; one snapshot dir and no new one; one worktree; no upload; broker pid and start unchanged" },
]);

/** The plan `--plan` prints (and tests read): steps, runs and the clause mapping, with implemented vs unchecked. */
export function buildPlan({ steps = STEPS.map((s) => s.id), allowInstall = false } = {}) {
  const chosen = STEPS.filter((s) => steps.includes(s.id));
  const implemented = new Set(Object.keys(CHECKS));
  const clauses = Object.values(CLAUSES).map((c) => ({ id: c.id, ev: c.ev, title: c.title, implemented: implemented.has(c.id), steps: chosen.filter((s) => s.clauses.includes(c.id)).map((s) => s.id), ...(implemented.has(c.id) ? {} : { unchecked: installReason(c.id, allowInstall) ?? UNCHECKED_WHY[c.id] ?? NO_EVIDENCE }) }));
  return { schema: "rightkit.gate-acceptance-plan/1", allowInstall, installStagesSkipped: allowInstall ? [] : [...INSTALL_STAGES], steps: chosen, negativesUnchecked: NEGATIVES_UNCHECKED, killsUnchecked: KILLS_UNCHECKED, clauses };
}

/** The reason a clause that needs `--to dev` / `--to qualify` evidence is unchecked while installs are not approved; null otherwise. */
export const installReason = (id, allowInstall) => (!allowInstall && INSTALL_CLAUSES.includes(id) ? NEEDS_INSTALL_APPROVAL : null);

export function formatPlan(plan) {
  const out = [];
  for (const s of plan.steps) out.push(`${s.id}  ${s.title}${s.runs.length ? `  [${s.runs.join(", ")}]` : ""}`, `      clauses: ${s.clauses.join(" ") || "-"}`, `      ${s.note}`);
  const impl = plan.clauses.filter((c) => c.implemented);
  const un = plan.clauses.filter((c) => !c.implemented);
  out.push("", `clauses: ${plan.clauses.length} total, ${impl.length} asserted, ${un.length} unchecked by design`);
  out.push(`asserted:  ${impl.map((c) => `${c.id}(${c.ev})`).join(" ")}`);
  out.push(`unchecked: ${un.map((c) => `${c.id}(${c.ev})`).join(" ")}`);
  if (plan.installStagesSkipped?.length) out.push(`install stages not run: --to ${plan.installStagesSkipped.join(", --to ")} (they install an app; pass --allow-install only with Adrian's approval); clauses needing them: ${INSTALL_CLAUSES.join(" ")}`);
  out.push(`negatives not built: ${Object.keys(plan.negativesUnchecked).join(" ")}`, `kills not built: ${Object.keys(plan.killsUnchecked).join(" ")}`);
  return out.join("\n");
}

// ---------------------------------------------------------------------------------------------------------------------------------------------
// Clause assertions. Each CHECKS entry is `(ctx) => { status: "pass"|"fail"|"unchecked", evidence: [..], problems: [..] }` or a list of them. `ctx.runs`
// holds the runs of the sequence by label; a clause needing a run that is absent is "unchecked". The summary of a clause across checks is: any fail
// -> fail; else all real checks pass and no blocking "missing" part -> pass; else unchecked.
// ---------------------------------------------------------------------------------------------------------------------------------------------
const verdict = (res, evidence) => ({ status: res.ok ? "pass" : "fail", evidence, problems: res.problems });
const unchecked = (why) => ({ status: "unchecked", evidence: [], problems: [why] });
/** An unchecked part that keeps the clause from passing although other parts passed. */
const missing = (why) => ({ status: "unchecked", blocksPass: true, evidence: [], problems: [why] });
const needRun = (ctx, label) => ctx.runs[label] ?? null;
const expectedFailure = (r) => Boolean(r.negative) || r.refusal !== undefined;
const firstRows = (ctx, ...labels) => { for (const l of labels) if (ctx.runs[l]?.rows) return ctx.runs[l]; return null; };

/** GC-I15: the broker process (pid and start) is the same at S0 and S9; the gate launched none. */
export function brokerUnchanged(a, b) {
  if (!a || !b) return { ok: false, problems: ["the broker process was not established at S0 or at S9"] };
  return a.pid === b.pid && a.startedMs === b.startedMs ? { ok: true, problems: [] } : { ok: false, problems: [`broker pid/start changed: ${JSON.stringify(a)} -> ${JSON.stringify(b)}`] };
}

/** GC-T19: the check stage's recorded rule tier is the tier the stage owes (compile below qualify, release at qualify). */
export function ruleTierProblems(receipt, stage) {
  const tier = receipt?.stages?.check?.ruleTier;
  const want = stage === "qualify" || stage === "release" ? "release" : "compile";
  return tier === want ? { ok: true, problems: [] } : { ok: false, problems: [`receipt.stages.check.ruleTier is ${JSON.stringify(tier)}, expected ${JSON.stringify(want)} for --to ${stage}`] };
}

export const CHECKS = {
  "GC-A21": (ctx) => {
    const out = [];
    for (const [label, run] of Object.entries(ctx.runs)) {
      if (!run.ladder) continue;
      if (run.args.includes("--route")) continue;
      if (run.exit !== 0 && !expectedFailure(run)) out.push({ label, status: "fail", evidence: [run.logPath], problems: [`the run exited ${run.exit}${run.timedOut ? " (timed out)" : ""}`] });
      const stamps = checkUtcStamps(run.log);
      out.push({ label, ...verdict(stamps, [`${run.logPath} (every line)`]) });
      if (run.exit === 0 && ["check", "compile", "package", "qualify"].includes(run.stage)) {
        // A reused stage prints `[stage] already passed ...` instead of a timing line; only a stage that ran owes `passed in`.
        const reused = new RegExp(`\\[${run.stage}\\] already passed`).test(run.log);
        const timings = stageTimings(run.log).filter((t) => t.stage === run.stage);
        if (!reused || timings.length) out.push({ label, ...verdict({ ok: timings.length > 0 && timings.every((t) => t.ok), problems: timings.length ? [] : [`no "[${run.stage}] passed in" line`] }, [`${run.logPath} ([${run.stage}] passed in)`]) });
      }
      if (run.coverageProblems.length) out.push({ label, status: "fail", evidence: run.coveragePaths, problems: run.coverageProblems });
      else if (run.coveragePaths.length) out.push({ label, status: "pass", evidence: run.coveragePaths, problems: [] });
      else if (run.exit === 0 && run.stage !== "render") out.push({ label, status: "fail", evidence: [run.logPath], problems: ["the log names no coverage report"] });
    }
    return out.length ? out : unchecked("no ladder run executed");
  },
  "GC-A04": (ctx) => {
    const out = [];
    for (const [label, run] of Object.entries(ctx.runs)) {
      if (!run.receiptPath || expectedFailure(run)) continue;
      if (run.exit !== 0) { out.push({ label, status: "fail", evidence: [run.logPath], problems: [`the run exited ${run.exit}${run.timedOut ? " (timed out)" : ""}: no receipt can stand for it`] }); continue; }
      if (!run.receipt) { out.push({ label, status: "fail", evidence: [run.receiptPath], problems: ["the run exited 0 but wrote no readable receipt (--receipt-out)"] }); continue; }
      const stages = LADDER.filter((s) => RECEIPT_STAGES.has(s) && run.stageIndex >= LADDER.indexOf(s)).filter((s) => run.receipt.stages?.[s]);
      out.push({ label, ...verdict(validateDoorReceipt(run.receipt, { commit: run.commit, expectedTree: run.tree, stages }), [run.receiptPath]) });
    }
    if (ctx.s6) out.push(ctx.s6.fresh && ctx.s6.proof ? { label: "S6", ...verdict(ctx.s6.proof, ["the S6 snapshot file vs the commit's blob, the checkout's dirty file and its source identity"]) } : unchecked(`S6: ${ctx.s6.why ?? "the dirty-checkout proof was not made"}`));
    return out.length ? out : unchecked("no run wrote a receipt (--receipt-out)");
  },
  "GC-T05": (ctx) => {
    const out = []; let reused = 0;
    for (const [label, run] of Object.entries(ctx.runs)) {
      if (!run.receipt || run.exit !== 0 || !run.receipt.stages?.compile || expectedFailure(run)) continue;
      const r = classABeforeCompile(run.receipt, run.log);
      if (r.skipped) { reused += 1; continue; }
      out.push({ label, ...verdict(r, [`${run.receiptPath} (stages.compile.at), ${run.logPath} ([check] passed in stamp)`]) });
    }
    for (const [label, run] of Object.entries(ctx.runs)) if (run.negative && ["N01", "N02"].includes(run.negative)) out.push({ label, ...verdict(run.negativeResult, [run.logPath]) });
    return out.length ? out : unchecked(`no compile ran in a run of its own${reused ? ` (${reused} run(s) reused a recorded compile) ` : ""} and no Class A negative ran`);
  },
  "GC-A15": (ctx) => {
    const run = [...Object.values(ctx.runs)].reverse().find((r) => r.receipt && r.exit === 0 && !expectedFailure(r) && Object.values(r.receipt.stages ?? {}).some((s) => s?.outputs));
    const out = [];
    if (run) out.push({ status: outputDirsProblems(run.receipt).ok ? "pass" : "fail", evidence: [run.receiptPath], problems: outputDirsProblems(run.receipt).problems });
    const n15 = Object.values(ctx.runs).find((r) => r.negative === "N15");
    if (n15) out.push({ ...verdict(n15.negativeResult, [n15.logPath]) });
    return out.length ? out : unchecked("no receipt records stage outputs");
  },
  "GC-A23": (ctx) => {
    // Read right after each run (the snapshot is re-synced by every later step), never at the end.
    const out = [];
    for (const [label, run] of Object.entries(ctx.runs)) {
      if (!run.sourceIdentity) continue;
      const si = run.sourceIdentity;
      if (si.error) { out.push({ label, status: "fail", evidence: [si.file], problems: [si.error] }); continue; }
      out.push({ label, ...verdict(validateSourceIdentity(si.identity, { commit: run.commit, trees: si.trees }), [`${si.file} (read right after ${label})`, "git rev-parse <commit>:<dir>"]) });
    }
    return out.length ? out : unchecked("no run produced a receipt that names a snapshot path");
  },
  "GC-A22-render": (ctx) => {
    const run = needRun(ctx, "S1:render");
    if (!run) return unchecked("--to render was not run");
    if (run.exit !== 0) return { status: "fail", evidence: [run.logPath], problems: [`--to render exited ${run.exit}`] };
    const dir = (/(?:render(?:ed)?|stored|wrote|receipt)[^\n]*?(\S*rightkit-gate-render\S*)/.exec(run.log) ?? [])[1]?.replace(/[,.;)]+$/, "");
    if (!dir) return { status: "fail", evidence: [run.logPath], problems: ["the log names no rightkit-gate-render directory"] };
    const file = path.join(dir.endsWith(".json") ? path.dirname(dir) : dir, "render-receipt.json");
    let receipt;
    try { receipt = JSON.parse(fs.readFileSync(file, "utf8")); } catch (e) { return { status: "fail", evidence: [file], problems: [`cannot read ${file}: ${e.message}`] }; }
    return verdict(validateRenderReceipt(receipt, { commit: run.commit, png: (rel) => { try { return fs.readFileSync(path.join(path.dirname(file), rel)); } catch { return null; } } }), [file]);
  },
  "GC-I22": (ctx) => {
    const dir = path.join(here, "test-support", "vectors");
    let source; const files = {};
    try { source = JSON.parse(fs.readFileSync(path.join(dir, "vectors-source.json"), "utf8")); for (const n of fs.readdirSync(dir)) if (n.endsWith(".json")) files[n] = fs.readFileSync(path.join(dir, n)); } catch (e) { return { status: "fail", evidence: [dir], problems: [e.message] }; }
    const up = ctx.rustrightContracts;
    let upstream = null;
    if (up && fs.existsSync(up)) { upstream = {}; for (const n of Object.keys(files)) { try { upstream[n] = fs.readFileSync(path.join(up, n)); } catch { /* reported as "no copy" by vectorsProblems */ } } }
    return verdict(vectorsProblems({ source, files: { ...files }, upstream, upstreamDir: up }), [path.join(dir, "vectors-source.json"), up ?? "RustRight docs/contracts"]);
  },
  "GC-I18": (ctx) => {
    const m = ctx.broker.manifest; const p = ctx.broker.pins;
    if (!m || !p) return { status: "fail", evidence: [ctx.broker.manifestPath ?? "runtime-manifest.json", ctx.broker.pinsPath ?? "pins/broker-<platform>.json"], problems: [ctx.broker.problem ?? "runtime manifest or pins record unreadable"] };
    return verdict(brokerDigestProblems({ manifest: m, pins: p }), [ctx.broker.manifestPath, ctx.broker.pinsPath]);
  },
  "GC-I01": (ctx) => {
    const key = repositoryKey(ctx.git.canonical, ctx.platform);
    const dir = path.join(ctx.managedRoot, "snapshots", key);
    const out = [];
    out.push({ ...verdict({ ok: fs.existsSync(dir), problems: fs.existsSync(dir) ? [] : [`${dir} does not exist`] }, [dir]) });
    const run = Object.values(ctx.runs).find((r) => r.receipt?.stages?.check?.snapshot?.path);
    if (run) { const sp = run.receipt.stages.check.snapshot.path; const ok = path.resolve(sp).startsWith(path.resolve(dir)); out.push({ ...verdict({ ok, problems: ok ? [] : [`receipt snapshot path ${sp} is not under ${dir}`] }, [run.receiptPath]) }); }
    return out;
  },
  "GC-A01": (ctx) => {
    const out = [];
    const n13 = Object.values(ctx.runs).filter((r) => r.negative === "N13");
    for (const r of n13) out.push({ ...verdict(r.negativeResult, [r.logPath]) });
    if (ctx.s9) {
      const wt = worktreeCount(ctx.s9.worktree);
      out.push({ ...verdict({ ok: wt === 1, problems: wt === 1 ? [] : [`${wt} worktrees listed, expected 1`] }, ["git worktree list --porcelain"]) });
      out.push({ ...verdict(ctx.s9.snapshotDirCheck, [path.join(ctx.managedRoot, "snapshots")]) });
    }
    return out.length ? out : unchecked("neither N13 nor S9 ran");
  },
  "GC-T14": (ctx) => CHECKS["GC-A01"](ctx),
  "GC-A02": (ctx) => {
    const out = [];
    if (ctx.s9) out.push({ ...sameVerdict(sameListing("git stash list", ctx.s0?.stash, ctx.s9.stash), ["git stash list at S0 and S9"], ctx.s0) });
    if (ctx.s6?.stash) out.push({ ...verdict(ctx.s6.stash, ["git stash list before and after S6"]) });
    if (ctx.s6?.restore) out.push({ ...verdict(ctx.s6.restore, ["the S6 checkout restore"]) });
    return out.length ? out : unchecked("S6 and S9 did not run");
  },
  "GC-T06": (ctx) => {
    const out = [];
    for (const [label, run] of Object.entries(ctx.runs)) if (run.recovery) out.push({ label, ...verdict(run.recovery, [run.logPath, run.ownerPath ?? "owner.json"]) });
    if (ctx.s6?.fresh && ctx.s6.proof) out.push({ label: "S6", ...verdict(ctx.s6.proof, ["S6: a dirty tree and an unpushed commit build the commit's bytes"]) });
    if (ctx.s9) out.push({ ...verdict(ctx.s9.ownerClean, [ctx.s9.ownerPath]) });
    return out.length ? out : unchecked("S6, S8 and S9 did not run");
  },
  "GC-A19": (ctx) => {
    const out = [];
    const s4 = needRun(ctx, "S4:check");
    const base = firstRows(ctx, "S2:check", "S1:check");
    if (s4?.rows && base && s4.manifestInfo) out.push({ ...verdict(assertManifestEdit(base.rows, s4.rows, s4.manifestInfo), [...base.coveragePaths, ...s4.coveragePaths]) });
    const s2 = needRun(ctx, "S2:check");
    if (s2?.rows) {
      const aud = [...s2.rows.values()].filter((r) => /^(cargo-deny|cargo-audit|pnpm-audit|ghsa-audit)/.test(r.id));
      const bad = aud.filter((r) => r.coverage === "ran");
      out.push({ ...verdict({ ok: aud.length > 0 && bad.length === 0, problems: aud.length === 0 ? ["no audit rows in the warm rerun"] : bad.map((r) => `${r.id}: ran on an unchanged commit (key ${r.key})`) }, s2.coveragePaths) });
    }
    return out.length ? out : unchecked("S2 and S4 did not run");
  },
  "GC-A24": (ctx) => {
    const out = [];
    const s1 = needRun(ctx, "S1:check"); const s2 = needRun(ctx, "S2:check"); const s3 = needRun(ctx, "S3:check");
    if (s1?.rows && s2?.rows) out.push({ ...verdict(assertWarmRerun(s1.rows, s2.rows), [...s1.coveragePaths, ...s2.coveragePaths]) });
    if (s2?.rows && s3?.rows) out.push({ ...verdict(assertOneFileEdit(s2.rows, s3.rows), [...s2.coveragePaths, ...s3.coveragePaths]) });
    for (const [label, run] of Object.entries(ctx.runs)) if (run.kill === "K02") out.push({ label, ...verdict(run.recovery, [run.logPath]) });
    return out.length ? out : unchecked("S1/S2/S3 coverage pairs are missing");
  },
  "GC-T04": (ctx) => {
    const out = [];
    for (const [label, run] of Object.entries(ctx.runs)) {
      if (!run.receipt || run.exit !== 0 || expectedFailure(run)) continue;
      const problems = [];
      if (run.receipt.binding?.executor !== "broker") problems.push(`receipt.binding.executor is ${JSON.stringify(run.receipt.binding?.executor)}, expected "broker"`);
      if ("brokerException" in run.receipt) problems.push("receipt.brokerException is set");
      out.push({ label, ...verdict({ ok: problems.length === 0, problems }, [`${run.receiptPath} (binding.executor)`]) });
    }
    return out.length ? out : unchecked("no run wrote a receipt");
  },
  "GC-T19": (ctx) => {
    const out = []; let release = false;
    for (const [label, run] of Object.entries(ctx.runs)) {
      if (!run.ladder || expectedFailure(run) && run.refusal === undefined) continue;
      if (run.refusal !== undefined) { if (run.refusal.ok) { release = true; out.push({ label, status: "pass", evidence: [run.logPath], problems: [] }); } else out.push({ label, ...verdict(run.refusal, [run.logPath]) }); continue; }
      if (!run.receipt || run.exit !== 0 || !run.receipt.stages?.check) continue;
      if (run.stage === "qualify") release = true;
      out.push({ label, ...verdict(ruleTierProblems(run.receipt, run.stage), [`${run.receiptPath} (stages.check.ruleTier)`]) });
    }
    if (!out.length) return unchecked("no run recorded a check stage");
    if (!release) out.push(missing(ctx.allowInstall ? "no --to qualify run: the release-tier half is unchecked" : `release-tier half (--to qualify): ${NEEDS_INSTALL_APPROVAL}`));
    return out;
  },
  "GC-A22-classA": (ctx) => {
    const src = firstRows(ctx, "S1:check", "S1:compile", "S2:check");
    const out = [];
    if (src) {
      const find = (re) => [...src.rows.values()].filter((r) => re.test(r.id));
      for (const [name, re, need] of [["ghsa-audit", /^ghsa-audit(:|$)/, true], ["secret-scan", /^secret-scan$/, true], ["feature-coverage", /^feature-coverage$/, false]]) {
        const rows = find(re);
        if (!rows.length) { out.push(need ? { status: "fail", evidence: src.coveragePaths, problems: [`no ${name} row in the coverage report`] } : missing(`no ${name} row: the fixture has no crate with declared features (${NEEDS_FIXTURE})`)); continue; }
        const bad = rows.filter((r) => !(["hit", "ran"].includes(r.coverage) && r.status === "pass")).map((r) => `${r.id}: ${r.coverage}/${r.status}${r.reason ? ` (${r.reason})` : ""}`);
        out.push({ status: bad.length ? "fail" : "pass", evidence: src.coveragePaths, problems: bad });
      }
    } else out.push(unchecked("no coverage report (S1 check did not run)"));
    const n02 = Object.values(ctx.runs).find((r) => r.negative === "N02");
    if (n02) out.push({ ...verdict(n02.negativeResult, [n02.logPath]) });
    return out;
  },
  "GC-I03": (ctx) => {
    const out = [];
    for (const [label, run] of Object.entries(ctx.runs)) {
      if (!run.receipt || run.exit !== 0 || expectedFailure(run)) continue;
      const problems = [];
      for (const stage of ["check", "compile"]) {
        const sn = run.receipt.stages?.[stage]?.snapshot;
        if (stage === "check" && !run.receipt.stages?.check) { problems.push("receipt.stages.check is missing"); continue; }
        if (run.receipt.stages?.[stage] === undefined) continue;
        if (!isObject(sn)) problems.push(`receipt.stages.${stage}.snapshot is missing`);
        else { if (sn.tree !== run.tree) problems.push(`${stage}: snapshot tree ${sn.tree} != commit^{tree} ${run.tree}`); if (sn.intactAfter !== true) problems.push(`${stage}: snapshot.intactAfter is ${JSON.stringify(sn.intactAfter)}`); }
      }
      out.push({ label, ...verdict({ ok: problems.length === 0, problems }, [`${run.receiptPath} (door half: snapshot tree at stage exit vs commit^{tree})`]) });
    }
    return out.length ? out : unchecked("no run wrote a receipt");
  },
  "GC-I05": (ctx) => {
    const out = [];
    for (const [label, run] of Object.entries(ctx.runs)) {
      if (!run.receipt || run.exit !== 0 || expectedFailure(run) || !run.receiptPath) continue;
      const stages = LADDER.filter((s) => RECEIPT_STAGES.has(s) && run.stageIndex >= LADDER.indexOf(s)).filter((s) => run.receipt.stages?.[s]);
      out.push({ label, ...verdict(validateDoorReceipt(run.receipt, { commit: run.commit, expectedTree: run.tree, stages }), [`${run.receiptPath} (door half: no verdict/refreshed, strict fields; the broker's receipt fields are not read)`]) });
    }
    return out.length ? out : unchecked("no run wrote a receipt");
  },
  "GC-I15": (ctx) => {
    if (!ctx.s0 || !ctx.s9) return unchecked("S0 and S9 did not both run");
    return verdict(brokerUnchanged(ctx.s0.broker, ctx.s9.broker), ["runningBrokerProcess() at S0 and at S9 (pid and start time)"]);
  },
  "GC-T20": (ctx) => (ctx.hooks ? verdict(hookProblems(ctx.hooks), [`${ctx.hooks.dir} (the hooks half only; the no-gate-process-on-commit half is not run)`]) : unchecked("the fixture's hooks were not read")),
};
const negativeCheck = (negId) => (ctx) => {
  const run = Object.values(ctx.runs).find((r) => r.negative === negId);
  return run ? { ...verdict(run.negativeResult, [run.logPath]) } : unchecked(`negative ${negId} was not run`);
};
CHECKS["GC-1A-R2"] = negativeCheck("N27");
CHECKS["GC-1A-R4"] = negativeCheck("N26");
const sameVerdict = (res, evidence, s0) => (s0 ? verdict(res, evidence) : unchecked("S0 listing missing"));

/** The per-clause verdict out of its checks: any fail -> fail; all real checks pass and nothing blocks -> pass; otherwise unchecked (never pass without evidence). */
export function summarizeClause(id, checks, { allowInstall = true } = {}) {
  const list = checks ?? [];
  const ev = list.flatMap((c) => c.evidence ?? []);
  const problems = list.flatMap((c) => (c.problems ?? []).map((p) => (c.label ? `${c.label}: ${p}` : p)));
  const real = list.filter((c) => c.status !== "unchecked");
  const blockers = list.filter((c) => c.status === "unchecked" && c.blocksPass);
  let status = "unchecked";
  if (list.some((c) => c.status === "fail")) status = "fail";
  else if (real.length > 0 && real.every((c) => c.status === "pass") && blockers.length === 0) status = "pass";
  let reason = null;
  if (status === "unchecked") reason = installReason(id, allowInstall) ?? (list.length ? problems.join("; ") : (UNCHECKED_WHY[id] ?? NO_EVIDENCE));
  return { id, ev: CLAUSES[id]?.ev ?? null, status, evidence: [...new Set(ev)], problems: status === "unchecked" ? [] : problems, ...(reason ? { reason } : {}), assertions: real.length };
}

/** Steps a PASS tolerates as unchecked: unchecked by design (S5). Any other selected step that did not pass blocks the verdict. */
const BY_DESIGN_UNCHECKED = new Set(STEPS.filter((s) => s.byDesignUnchecked).map((s) => s.id));

export function buildResult({ host, gate, broker, commit, steps, clauseChecks, startedAt, endedAt, notes = [], allowInstall = true, stepChecks = [] }) {
  const clauses = Object.keys(CLAUSES).map((id) => summarizeClause(id, clauseChecks[id], { allowInstall }));
  const counts = { pass: 0, fail: 0, unchecked: 0 };
  for (const c of clauses) counts[c.status] += 1;
  const stepsOut = steps.map((s) => ({ ...s, status: s.checks.some((c) => c === "fail") ? "fail" : s.checks.length && s.checks.every((c) => c === "pass") ? "pass" : "unchecked" }));
  const blockers = [
    ...clauses.filter((c) => c.status === "fail").map((c) => `clause ${c.id} failed`),
    ...stepsOut.filter((s) => s.status === "fail").map((s) => `step ${s.id} failed`),
    ...stepsOut.filter((s) => s.status === "unchecked" && !BY_DESIGN_UNCHECKED.has(s.id)).map((s) => `step ${s.id} did not run as specified`),
    ...stepChecks.filter((c) => !c.ok).map((c) => `${c.step} check "${c.name}" failed`),
  ];
  if (counts.pass === 0) blockers.push("no clause passed");
  const partial = stepsOut.length < STEPS.length;
  const ok = blockers.length === 0;
  const asserted = counts.pass + counts.fail;
  const tail = `${asserted} of ${clauses.length} asserted, ${counts.unchecked} unchecked`;
  const verdictText = ok ? `PASS (${tail}${partial ? `; partial run: steps ${stepsOut.map((s) => s.id).join(",")} only` : ""})` : `NOT PASSED (${tail}; ${counts.fail} failed clause(s); ${blockers.length} blocker(s))`;
  return { schema: RESULT_SCHEMA, host, gate, broker, fixtureCommit: commit, startedAt, endedAt, steps: stepsOut, stepChecks, clauses, totals: { clauses: clauses.length, ...counts }, ok, verdict: verdictText, blockers, partial, notes };
}

export function formatSummary(result) {
  const lines = [`gate acceptance ${result.verdict}  host ${result.host}  commit ${String(result.fixtureCommit).slice(0, 12)}`, `clauses: ${result.totals.pass} pass, ${result.totals.fail} fail, ${result.totals.unchecked} unchecked of ${result.totals.clauses}`];
  for (const b of result.blockers) lines.push(`  BLOCKER ${b}`);
  for (const c of result.clauses.filter((x) => x.status === "fail")) lines.push(`  FAIL ${c.id}: ${c.problems.slice(0, 2).join(" | ").slice(0, 200)}  [${c.evidence.slice(0, 2).join(", ")}]`);
  for (const sc of (result.stepChecks ?? []).filter((x) => !x.ok)) lines.push(`  FAIL ${sc.step} ${sc.name}: ${sc.problems.slice(0, 2).join(" | ").slice(0, 200)}`);
  for (const s of result.steps) lines.push(`  ${s.id} ${s.status} ${s.title}`);
  return lines.join("\n");
}

// ---------------------------------------------------------------------------------------------------------------------------------------------
// Runtime: git, the gate, the kill cases. Nothing below runs during `--plan` or in the pure tests (the dirty-checkout helpers are exported and tested
// against a temporary repository).
// ---------------------------------------------------------------------------------------------------------------------------------------------
const IDENT = Object.freeze({ GIT_AUTHOR_NAME: "gate-acceptance", GIT_AUTHOR_EMAIL: "gate-acceptance@invalid", GIT_COMMITTER_NAME: "gate-acceptance", GIT_COMMITTER_EMAIL: "gate-acceptance@invalid" });

function git(cwd, args, { env = {}, input, buffer = false } = {}) {
  const r = spawnSync("git", args, { cwd, encoding: buffer ? "buffer" : "utf8", env: { ...process.env, ...env }, input, maxBuffer: 1 << 28 });
  if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${String(r.stderr || r.stdout || "").trim() || `exit ${r.status}`}`);
  return r.stdout;
}

/** Workspace directory a Cargo manifest belongs to: the nearest ancestor directory in `wsDirs`, else the manifest's own directory (a standalone package). */
export function cargoWorkspaceOf(file, wsDirs) {
  const dir = path.posix.dirname(file);
  for (let d = dir; ; d = path.posix.dirname(d)) {
    if (wsDirs.includes(d)) return d;
    if (d === "." || d === "/") break;
  }
  return dir;
}

function gitTools(fixture, scratch) {
  const canonical = fs.realpathSync.native(fixture);
  const created = [];
  const self = {
    canonical,
    run: (args, o) => git(canonical, args, o),
    head: () => git(canonical, ["rev-parse", "HEAD"]).trim(),
    tree: (commit) => git(canonical, ["rev-parse", `${commit}^{tree}`]).trim(),
    stash: () => git(canonical, ["stash", "list"]),
    worktrees: () => git(canonical, ["worktree", "list", "--porcelain"]),
    status: () => git(canonical, ["status", "--short"]),
    tracked: (commit) => git(canonical, ["ls-tree", "-r", "--name-only", commit]).split("\n").filter(Boolean),
    sourceTrees(commit) {
      const dirs = new Set([""]);
      for (const f of self.tracked(commit)) if (/(^|\/)(Cargo\.toml|Package\.swift|package\.json)$/.test(f)) dirs.add(path.posix.dirname(f) === "." ? "" : path.posix.dirname(f));
      const trees = {};
      for (const d of dirs) trees[d] = git(canonical, ["rev-parse", d === "" ? `${commit}^{tree}` : `${commit}:${d}`]).trim();
      return trees;
    },
    show: (commit, file) => git(canonical, ["show", `${commit}:${file}`]),
    blob: (commit, file) => git(canonical, ["cat-file", "blob", `${commit}:${file}`], { buffer: true }),
    /** The Cargo layout of `commit`: workspace directories and `{ workspaceDir: [crate names] }` (for S4's "that workspace only"). */
    cargoLayout(commit, tracked = self.tracked(commit)) {
      const manifests = tracked.filter((f) => /(^|\/)Cargo\.toml$/.test(f));
      const texts = new Map(manifests.map((f) => [f, self.show(commit, f)]));
      const wsDirs = manifests.filter((f) => /^\s*\[workspace\]/m.test(texts.get(f))).map((f) => path.posix.dirname(f));
      const crates = {};
      for (const f of manifests) {
        const name = /\[package\][^[]*?\bname\s*=\s*"([^"]+)"/s.exec(texts.get(f))?.[1];
        if (!name) continue;
        (crates[cargoWorkspaceOf(f, wsDirs)] ??= []).push(name);
      }
      return { manifests, wsDirs, crates, texts };
    },
    /** An ephemeral commit on `base` with `edits` ({path, content}); referenced under refs/witness/<name>; no branch, no checkout, no worktree. The temporary index lives under the scratch root. */
    witnessCommit(base, name, edits) {
      fs.mkdirSync(scratch, { recursive: true });
      const idx = path.join(fs.mkdtempSync(path.join(scratch, "idx-")), "index");
      const env = { GIT_INDEX_FILE: idx };
      try {
        git(canonical, ["read-tree", base], { env });
        for (const e of edits) {
          const blob = git(canonical, ["hash-object", "-w", "--stdin"], { input: e.content }).trim();
          git(canonical, ["update-index", "--add", "--cacheinfo", `100644,${blob},${e.path}`], { env });
        }
        const tree = git(canonical, ["write-tree"], { env }).trim();
        const commit = git(canonical, ["commit-tree", tree, "-p", base, "-m", `gate-acceptance ${name}`], { env: IDENT }).trim();
        git(canonical, ["update-ref", `refs/witness/${name}`, commit]);
        created.push(name);
        return commit;
      } finally { fs.rmSync(path.dirname(idx), { recursive: true, force: true }); }
    },
    /** Deletes every refs/witness/<name> this run created; returns the problems (empty when all are gone). */
    dropWitnessRefs() {
      const problems = [];
      for (const name of created.splice(0)) { try { git(canonical, ["update-ref", "-d", `refs/witness/${name}`]); } catch (e) { problems.push(e.message); } }
      return problems;
    },
  };
  return self;
}

// --- S6: the dirty checkout ---------------------------------------------------------------------------------------------------------------------
const stashEntries = (repo) => git(repo, ["stash", "list", "--format=%H %gd"]).split("\n").filter(Boolean).map((l) => { const [sha, gd] = l.split(" "); return { sha, gd }; });

/** A checkout is clean when `git status --porcelain --untracked-files=all` prints nothing. */
export function assertCleanCheckout(repo) {
  const st = git(repo, ["status", "--porcelain", "--untracked-files=all"]);
  return st.trim() === "" ? { ok: true, problems: [] } : { ok: false, problems: [`the checkout is not clean: ${st.trim().split("\n")[0]}${st.trim().split("\n").length > 1 ? ` (+${st.trim().split("\n").length - 1} more)` : ""}`] };
}

/**
 * Makes `repo` dirty for S6: refuses a dirty checkout; edits tracked `file`, adds the untracked file, pushes both into a new stash, then edits and adds
 * them again so the checkout is dirty AND a stash of ours stands. Returns the state `restoreDirtyCheckout` needs. A failure part-way restores before throwing.
 */
export function makeDirtyCheckout(repo, { file, untracked }) {
  const clean = assertCleanCheckout(repo);
  if (!clean.ok) throw new Error(`refusing to make a dirty checkout: ${clean.problems[0]}`);
  const abs = path.join(repo, file); const absU = path.join(repo, untracked);
  if (fs.existsSync(absU)) throw new Error(`refusing: ${untracked} exists`);
  const state = { repo, file, untracked, original: fs.readFileSync(abs), stashBefore: stashEntries(repo).map((e) => e.sha), stashSha: null };
  try {
    fs.writeFileSync(abs, Buffer.concat([state.original, Buffer.from("\ngate-acceptance dirty edit\n")]));
    fs.writeFileSync(absU, "untracked\n");
    git(repo, ["stash", "push", "--include-untracked", "-m", "gate-acceptance-s6"], { env: IDENT });
    const after = stashEntries(repo);
    if (after.length !== state.stashBefore.length + 1) throw new Error("git stash push did not create a stash");
    state.stashSha = after[0].sha;
    fs.writeFileSync(abs, Buffer.concat([state.original, Buffer.from("\ngate-acceptance second dirty edit\n")]));
    fs.writeFileSync(absU, "untracked again\n");
    return state;
  } catch (e) {
    const r = restoreDirtyCheckout(repo, state);
    e.message += r.ok ? " (the checkout was restored)" : ` (the restore also failed: ${r.problems.join("; ")})`;
    throw e;
  }
}

/**
 * Restores what `makeDirtyCheckout` changed: removes the untracked file, restores the tracked file from the index, drops OUR stash only (found by its
 * hash, so a stash that stood before is never touched), then verifies the checkout is clean, the stash list holds exactly the hashes it held before
 * and the tracked file's bytes are the original ones.
 */
export function restoreDirtyCheckout(repo, state) {
  const problems = [];
  const attempt = (what, fn) => { try { fn(); } catch (e) { problems.push(`${what}: ${e.message}`); } };
  attempt("remove the untracked file", () => fs.rmSync(path.join(repo, state.untracked), { force: true }));
  attempt("restore the tracked file", () => git(repo, ["checkout", "--", state.file]));
  if (state.stashSha) attempt("drop the gate-acceptance stash", () => {
    const entry = stashEntries(repo).find((e) => e.sha === state.stashSha);
    if (!entry) throw new Error("it is not in the stash list");
    git(repo, ["stash", "drop", entry.gd]);
  });
  attempt("verify", () => {
    const clean = assertCleanCheckout(repo);
    if (!clean.ok) problems.push(...clean.problems);
    const now = stashEntries(repo).map((e) => e.sha);
    if (JSON.stringify(now) !== JSON.stringify(state.stashBefore)) problems.push(`the stash list is ${JSON.stringify(now)}, it held ${JSON.stringify(state.stashBefore)}`);
    if (!fs.readFileSync(path.join(repo, state.file)).equals(state.original)) problems.push(`${state.file} differs from its original bytes`);
  });
  return { ok: problems.length === 0, problems };
}

const managedRootOf = (host) => (host === "dell" ? "D:\\.rightkit-managed" : "/Volumes/D/.rightkit-managed");
const scratchRootOf = (host) => (host === "dell" ? "D:\\scratch" : "/Volumes/D/scratch");
const defaultGate = (host) => (host === "dell" ? "D:\\.rightkit-managed\\agent-bin\\rightkit-host-gate.cmd" : "/Volumes/D/.rightkit-managed/agent-bin/rightkit-host-gate");
/** The workspace root that holds rightkit/ and rustright/ (this file is rightkit/packages/release/gate-acceptance.mjs). */
export const workspaceRootOf = (dir = here) => path.resolve(dir, "..", "..", "..");

const listDirs = (dir) => { try { return fs.readdirSync(dir, { withFileTypes: true }).filter((e) => e.isDirectory()).map((e) => e.name).sort(); } catch { return []; } };

function launch({ gate, args, cwd, env }) {
  const spec = launchSpec(gate, args);
  return spawn(spec.command, spec.args, { cwd, env, stdio: ["ignore", "pipe", "pipe"], detached: process.platform !== "win32", windowsHide: true, ...spec.options });
}

/** Signals the launcher and everything it started (its process group; taskkill /T on Windows, where only the kill variant exists). */
function signalGroup(child, signal) {
  try {
    if (process.platform === "win32") spawnSync("taskkill", ["/T", "/F", "/PID", String(child.pid)]);
    else process.kill(-child.pid, signal);
  } catch { try { child.kill(signal); } catch { /* already gone */ } }
}

/** Runs the gate to completion: `{ exit, signal, log, ms, timedOut }`. The environment is the caller's own; no flag or variable weakens the gate. A timeout kills the whole process group. */
function runGate({ gate, cwd, args, env = process.env, timeoutMs = 3_600_000 }) {
  return new Promise((resolve) => {
    const t0 = Date.now(); let log = ""; let timedOut = false; let child;
    try { child = launch({ gate, args, cwd, env }); } catch (e) { resolve({ exit: null, signal: null, log: `spawn error: ${e.message}`, ms: 0, timedOut: false }); return; }
    const timer = setTimeout(() => { timedOut = true; signalGroup(child, "SIGKILL"); }, timeoutMs);
    child.stdout.on("data", (d) => { log += d; }); child.stderr.on("data", (d) => { log += d; });
    child.on("close", (exit, signal) => { clearTimeout(timer); resolve({ exit, signal, log: timedOut ? `${log}\ngate-acceptance: timed out after ${timeoutMs} ms; the process group was killed` : log, ms: Date.now() - t0, timedOut }); });
    child.on("error", (e) => { clearTimeout(timer); resolve({ exit: null, signal: null, log: `${log}\nspawn error: ${e.message}`, ms: Date.now() - t0, timedOut }); });
  });
}

/** Starts the gate, waits for `waitFor()` to return truthy (the owner record) plus `afterMs`, then signals its process group. */
function killDuring({ gate, cwd, args, signal, waitFor, afterMs }) {
  return new Promise((resolve) => {
    let log = ""; let done = false; let child;
    try { child = launch({ gate, args, cwd, env: process.env }); } catch (e) { resolve({ exit: null, signal: null, log: `spawn error: ${e.message}` }); return; }
    child.stdout.on("data", (d) => { log += d; }); child.stderr.on("data", (d) => { log += d; });
    child.on("close", (exit, sig) => { done = true; clearInterval(poll); clearTimeout(cap); resolve({ exit, signal: sig, log }); });
    child.on("error", (e) => { done = true; clearInterval(poll); clearTimeout(cap); resolve({ exit: null, signal: null, log: `${log}\nspawn error: ${e.message}` }); });
    let armedAt = null;
    const poll = setInterval(() => {
      if (done) return;
      if (armedAt === null && waitFor()) armedAt = Date.now();
      if (armedAt !== null && Date.now() - armedAt >= afterMs) { clearInterval(poll); signalGroup(child, signal); }
    }, 100);
    const cap = setTimeout(() => { if (!done) signalGroup(child, "SIGKILL"); }, 30 * 60_000); // the owner record never appeared: the case proves nothing
  });
}

function readJson(file) { try { return { value: JSON.parse(fs.readFileSync(file, "utf8")) }; } catch (e) { return { error: `${file}: ${e.message}` }; } }
const readOrNull = (file) => { try { return fs.readFileSync(file); } catch { return null; } };

/** The installed broker's runtime manifest and the gate's pins record. The client and its manifest are found the way the gate finds them (broker-handshake.mjs): `managedRuntimeClient`, the agent shim resolved, the manifest beside the real client. */
function brokerIdentity(host, managedRoot) {
  const platform = host === "dell" ? "win32" : "darwin";
  const pinsPath = path.join(managedRoot, "pins", `broker-${platform}.json`);
  const pins = readJson(pinsPath);
  let manifestPath = null; const problems = [];
  try {
    const client = managedRuntimeClient(process.env, platform);
    if (!client) throw new Error("no installed RustRight client was found");
    const exe = resolveBrokerClient(client, { platform }).file;
    const real = platform === "win32" ? exe : fs.realpathSync.native(exe);
    const paths = platform === "win32" ? path.win32 : path.posix;
    manifestPath = paths.join(paths.dirname(real), RUNTIME_MANIFEST_FILE);
  } catch (e) { problems.push(`cannot locate the installed client's runtime manifest: ${e.message}`); }
  const manifest = manifestPath ? readJson(manifestPath) : { error: problems.join("; ") };
  return { pinsPath, pins: pins.value ?? null, manifestPath, manifest: manifest.value ?? null, problem: [pins.error, manifest.error].filter(Boolean).join("; ") || undefined };
}

/** The fixture's hooks as GC-T20 reads them: `{ dir, hooksPath, files }`. */
function readHooks(g) {
  let hooksPath = null; let dir = null; const files = {};
  try { hooksPath = g.run(["config", "--get", "core.hooksPath"]).trim() || null; } catch { /* not set */ }
  try { dir = path.resolve(g.canonical, g.run(["rev-parse", "--git-path", "hooks"]).trim()); } catch { /* unreadable: reported by the empty dir */ }
  const read = hooksPath ? path.resolve(g.canonical, hooksPath) : dir;
  if (read) for (const e of (() => { try { return fs.readdirSync(read, { withFileTypes: true }); } catch { return []; } })()) if (e.isFile()) { try { files[e.name] = fs.readFileSync(path.join(read, e.name), "utf8").slice(0, 1 << 20); } catch { files[e.name] = ""; } }
  return { dir: read ?? "<git hooks dir>", hooksPath, files };
}

/** The snapshot's `.rightkit-source-identity.json`, read at the moment of the run (every later step re-syncs the snapshot). */
function readSourceIdentity(snapshotPath, commit, g) {
  const file = path.join(snapshotPath, ".rightkit-source-identity.json");
  try { return { file, identity: JSON.parse(fs.readFileSync(file, "utf8")), trees: g.sourceTrees(commit) }; } catch (e) { return { file, error: `cannot read ${file}: ${e.message}` }; }
}

async function main(argv) {
  const opt = (name) => { const i = argv.indexOf(name); return i < 0 ? undefined : argv[i + 1]; };
  const known = new Set(["--fixture", "--host", "--steps", "--out", "--plan", "--gate", "--commit", "--leaf-file", "--manifest-file", "--kill-after-ms", "--allow-install", "--help"]);
  for (const a of argv) if (a.startsWith("--") && !known.has(a)) { console.error(`gate-acceptance: unknown option ${a} (no cache bypass or other gate flag is ever passed)`); return 64; }
  const host = opt("--host") ?? (process.platform === "win32" ? "dell" : "mac");
  if (!["mac", "dell"].includes(host)) { console.error("gate-acceptance: --host must be mac or dell"); return 64; }
  const wanted = (opt("--steps") ?? STEPS.map((s) => s.id).join(",")).split(",").map((s) => s.trim()).filter(Boolean);
  for (const s of wanted) if (!STEPS.some((x) => x.id === s)) { console.error(`gate-acceptance: unknown step ${s}`); return 64; }
  const allowInstall = argv.includes("--allow-install");
  if (allowInstall) console.error("gate-acceptance: --allow-install: --to dev and --to qualify INSTALL an app on this host. Installs need Adrian's approval; this flag is not that approval.");
  if (argv.includes("--plan")) { console.log(formatPlan(buildPlan({ steps: wanted, allowInstall }))); return 0; }
  const fixture = opt("--fixture");
  if (!fixture) { console.error("usage: node gate-acceptance.mjs --fixture <dir> [--host mac|dell] [--steps S0,S1,...] [--out <dir>] [--plan] [--gate <path>] [--commit <sha>] [--allow-install]"); return 64; }
  return execute({ fixture, host, wanted, gate: opt("--gate") ?? defaultGate(host), out: opt("--out"), commitArg: opt("--commit"), leafFile: opt("--leaf-file"), manifestFile: opt("--manifest-file"), killAfterMs: Number(opt("--kill-after-ms") ?? 3000), allowInstall });
}

async function execute(options) {
  const scratch = path.join(scratchRootOf(options.host), "gate-acceptance");
  fs.mkdirSync(scratch, { recursive: true });
  const g = gitTools(options.fixture, scratch);
  try { return await executeRun({ ...options, g, scratch }); } finally {
    const problems = g.dropWitnessRefs();
    if (problems.length) console.error(`gate-acceptance: could not delete a refs/witness ref: ${problems.join("; ")}`);
  }
}

async function executeRun({ fixture, host, wanted, gate, out, commitArg, leafFile, manifestFile, killAfterMs, allowInstall, g, scratch }) {
  const startedAt = new Date().toISOString();
  const platform = host === "dell" ? "win32" : "darwin";
  const managedRoot = managedRootOf(host);
  const outDir = out ?? path.join(scratch, "runs", `${host}-${startedAt.replace(/[:.]/g, "-")}`);
  fs.mkdirSync(outDir, { recursive: true });
  const commit = commitArg ?? g.head();
  if (!isCommit(commit)) throw new Error(`commit ${commit} is not a 40 hex sha`);
  const clauseChecks = Object.fromEntries(Object.keys(CLAUSES).map((id) => [id, []]));
  const runs = {};
  const stepChecks = [];
  const stepRecords = STEPS.filter((s) => wanted.includes(s.id)).map((s) => ({ id: s.id, title: s.title, checks: [], notes: [] }));
  const stepOf = (id) => stepRecords.find((s) => s.id === id);
  const ctx = { runs, git: g, managedRoot, platform, allowInstall, broker: brokerIdentity(host, managedRoot), rustrightContracts: path.join(workspaceRootOf(), "rustright", "docs", "contracts"), hooks: readHooks(g), s0: null, s6: null, s9: null };
  const stages = ladderStages({ allowInstall });

  const gateFile = path.join(here, "bin", "rightkit-gate.mjs");
  const gateIdentity = { launcher: gate, launcherSha256: (() => { try { return sha256(fs.readFileSync(gate)); } catch { return null; } })(), runnerPackageVersion: readJson(path.join(here, "package.json")).value?.version ?? null, packageGateSha256: (() => { try { return sha256(fs.readFileSync(gateFile)); } catch { return null; } })(), receiptGateSha256: null };

  const key = repositoryKey(g.canonical, platform);
  const ownerFile = ownershipFile({ platform, repository: key });
  const readOwner = () => { try { return JSON.parse(fs.readFileSync(ownerFile, "utf8")); } catch { return null; } };
  const snapRoot = path.join(managedRoot, "snapshots");

  /** A step-only check: it feeds the step status AND the verdict (stepChecks), so it can never be dropped. */
  const check = (step, name, res) => {
    stepOf(step)?.checks.push(res.ok ? "pass" : "fail");
    stepChecks.push({ step, name, ok: res.ok, problems: res.problems });
    if (!res.ok) stepOf(step)?.notes.push(`${name}: ${res.problems[0]}`);
  };
  const exitOk = (r) => ({ ok: r.exit === 0, problems: r.exit === 0 ? [] : [`exit ${r.exit}${r.timedOut ? " (timed out)" : ""}`] });

  async function doRun({ label, stage, commit: c = commit, args = [], ladder = false, step, extra = {} }) {
    const receiptPath = RECEIPT_STAGES.has(stage) ? path.join(outDir, `${label.replace(/[^\w.-]/g, "_")}.receipt.json`) : null;
    const full = ["--to", stage, "--commit", c, ...(receiptPath ? ["--receipt-out", receiptPath] : []), ...args];
    const res = await runGate({ gate, cwd: g.canonical, args: full });
    return finishRun({ label, stage, commit: c, full, res, receiptPath, ladder, step, extra });
  }
  function finishRun({ label, stage, commit: c, full, res, receiptPath, ladder = false, step, extra = {} }) {
    const logPath = path.join(outDir, `${label.replace(/[^\w.-]/g, "_")}.log`);
    fs.writeFileSync(logPath, res.log);
    const cov = coveragePaths(res.log); const reports = []; const coverageProblems = [];
    for (const p of cov) { const j = readJson(p); if (j.error) { coverageProblems.push(`cannot read coverage ${j.error}`); continue; } const v = validateCoverage(j.value); if (!v.ok) coverageProblems.push(...v.problems.map((x) => `${p}: ${x}`)); reports.push(j.value); }
    let receipt = null;
    if (receiptPath && fs.existsSync(receiptPath)) { const j = readJson(receiptPath); if (j.value) receipt = j.value; else coverageProblems.push(`cannot read receipt ${j.error}`); }
    if (receipt?.binding?.gate) gateIdentity.receiptGateSha256 = receipt.binding.gate;
    const snapPath = receipt?.stages?.check?.snapshot?.path ?? receipt?.stages?.compile?.snapshot?.path;
    const sourceIdentity = res.exit === 0 && snapPath ? readSourceIdentity(snapPath, c, g) : null;
    const run = { label, stage, commit: c, tree: g.tree(c), args: full, exit: res.exit, signal: res.signal, timedOut: res.timedOut === true, ms: res.ms, log: res.log, logPath, receipt, receiptPath, coveragePaths: cov, coverageProblems, rows: reports.length ? mergeCoverage(reports) : null, ladder, stageIndex: LADDER.indexOf(stage), timings: stageTimings(res.log), sourceIdentity, ...extra };
    runs[label] = run; if (step) stepOf(step)?.notes.push(`${label}: exit ${res.exit}, ${(res.ms / 1000).toFixed(1)}s`);
    return run;
  }

  // S0 -------------------------------------------------------------------------------------------------------------------------------------
  if (wanted.includes("S0") || wanted.includes("S9")) {
    const brokerNow = runningBrokerProcess() ?? null;
    ctx.s0 = { stash: g.stash(), status: g.status(), worktree: g.worktrees(), broker: brokerNow, owner: readOwner(), snapshots: listDirs(snapRoot) };
  }
  if (wanted.includes("S0")) {
    for (const id of ["GC-I22", "GC-I18", "GC-I01"]) { const r = CHECKS[id](ctx); for (const c of Array.isArray(r) ? r : [r]) { clauseChecks[id].push(c); if (id !== "GC-I01") { stepOf("S0").checks.push(c.status === "unchecked" ? "unchecked" : c.status); stepChecks.push({ step: "S0", name: id, ok: c.status === "pass", problems: c.problems }); } } }
    check("S0", "the fixture checkout is clean", ctx.s0.status.trim() === "" ? { ok: true, problems: [] } : { ok: false, problems: [`fixture checkout is not clean: ${ctx.s0.status.trim().split("\n")[0]}`] });
    check("S0", "no owner record stands before the run", ctx.s0.owner ? { ok: false, problems: [`an owner record stands before the run: ${JSON.stringify({ handle: ctx.s0.owner.handle, pid: ctx.s0.owner.pid })}`] } : { ok: true, problems: [] });
  }

  // S1 --------------------------------------------------------------------------------------------------------------------------------------
  if (wanted.includes("S1")) {
    for (const stage of stages) {
      const r = await doRun({ label: `S1:${stage}`, stage, ladder: true, step: "S1" });
      if (stage === "qualify" && r.exit !== 0) {
        r.refusal = qualifyRefusal({ exit: r.exit, text: r.log, receipt: r.receipt });
        check("S1", "--to qualify refuses by naming a release-tier rule", r.refusal);
      } else check("S1", `--to ${stage} exits 0`, exitOk(r));
      if (r.exit !== 0 && stage !== "qualify") { stepOf("S1").notes.push(`ladder stopped at ${stage}: exit ${r.exit}`); break; }
    }
    if (!allowInstall) stepOf("S1").notes.push(`--to ${INSTALL_STAGES.join(" and --to ")} not run: ${NEEDS_INSTALL_APPROVAL}`);
  }
  // S2 --------------------------------------------------------------------------------------------------------------------------------------
  if (wanted.includes("S2")) for (const stage of ["check", "compile"]) { const r = await doRun({ label: `S2:${stage}`, stage, ladder: true, step: "S2" }); check("S2", `--to ${stage} exits 0`, exitOk(r)); }

  // S3, S4 (ephemeral edits) ----------------------------------------------------------------------------------------------------------------
  const tracked = g.tracked(commit);
  const layout = (wanted.includes("S4") || wanted.includes("S7")) ? g.cargoLayout(commit, tracked) : null;
  const editStep = async (id, name, file, edit, label, extraOf = () => ({})) => {
    if (!wanted.includes(id)) return;
    if (!file) { stepOf(id).notes.push(`unchecked: ${NEEDS_FIXTURE}`); return; }
    const c = g.witnessCommit(commit, name, [{ path: file, content: edit(g.show(commit, file)) }]);
    const r = await doRun({ label, stage: "check", commit: c, ladder: true, step: id, extra: extraOf(file) });
    check(id, `--to check on the edited commit exits 0`, exitOk(r));
  };
  await editStep("S3", "s3-one-file", leafFile ?? tracked.find((f) => /^rk\/.*\/src\/lib\.rs$/.test(f)), (t) => `${t}${t.endsWith("\n") ? "" : "\n"}// gate-acceptance one-file edit\n`, "S3:check");
  const manifest = manifestFile ?? (layout ? layout.manifests.find((f) => /^rk\/.*\/Cargo\.toml$/.test(f) && !layout.wsDirs.includes(path.posix.dirname(f))) : undefined);
  await editStep("S4", "s4-manifest", manifest, (t) => `${t}${t.endsWith("\n") ? "" : "\n"}\n[package.metadata.gate-acceptance]\nedit = "manifest"\n`, "S4:check", (file) => {
    const ws = cargoWorkspaceOf(file, layout.wsDirs);
    return { manifestInfo: { workspace: ws, workspaces: [...new Set([...layout.wsDirs, ws])], crates: layout.crates } };
  });
  if (wanted.includes("S5")) stepOf("S5").notes.push("unchecked by design: a lock edit needs the fixture's own resolver; not generated by this runner");

  // S6 dirty checkout ------------------------------------------------------------------------------------------------------------------------
  if (wanted.includes("S6")) {
    const s6 = { fresh: false, proof: null, why: null, stash: null, restore: null };
    ctx.s6 = s6;
    const file = tracked.includes("README.md") ? "README.md" : tracked.find((f) => /\.(md|txt)$/.test(f));
    const untrackedName = ".gate-acceptance-untracked";
    const clean = assertCleanCheckout(g.canonical);
    if (!file) { s6.why = "no tracked README.md or text file to make dirty"; stepOf("S6").notes.push(`unchecked: ${s6.why}`); }
    else if (!clean.ok) { s6.why = clean.problems[0]; check("S6", "the fixture is clean before S6 (S6 refuses to start otherwise)", clean); }
    else {
      const stashBefore = g.stash(); let state = null;
      try {
        // A commit whose compile is not recorded yet, so the dirty checkout meets a REAL compile (a recorded compile would be reused, not run).
        const c6 = g.witnessCommit(commit, "s6-dirty", [{ path: "docs/gate-acceptance-s6.txt", content: "gate-acceptance S6: a commit whose compile is not recorded\n" }]);
        state = makeDirtyCheckout(g.canonical, { file, untracked: untrackedName });
        const r = await doRun({ label: "S6:compile", stage: "compile", commit: c6, ladder: true, step: "S6" });
        s6.fresh = r.exit === 0 && compileRanFresh(r.log);
        const snapPath = r.receipt?.stages?.compile?.snapshot?.path;
        if (r.exit === 0 && snapPath) {
          const si = r.sourceIdentity;
          const identityProblems = si?.error ? [si.error] : si ? validateSourceIdentity(si.identity, { commit: c6, trees: si.trees }).problems : ["no source identity was read"];
          s6.proof = dirtyProof({ commitBytes: g.blob(c6, file), checkoutBytes: readOrNull(path.join(g.canonical, file)), snapshotBytes: readOrNull(path.join(snapPath, file)), untrackedInSnapshot: fs.existsSync(path.join(snapPath, untrackedName)), identityProblems, receiptTree: r.receipt?.binding?.tree, expectedTree: g.tree(c6) });
        }
        s6.why = s6.fresh ? null : r.exit !== 0 ? `the compile exited ${r.exit}` : "the compile was already recorded for this commit (`[compile] already passed`): no compile ran on the dirty checkout";
        check("S6", "--to compile on the dirty checkout exits 0", exitOk(r));
        if (s6.proof) check("S6", "the snapshot holds the commit's bytes, not the dirty checkout's", s6.proof);
        if (r.exit === 0 && !s6.fresh) stepOf("S6").checks.push("unchecked");
      } catch (e) { s6.why = e.message; check("S6", "dirty-checkout setup and run", { ok: false, problems: [e.message] }); }
      finally {
        if (state) { s6.restore = restoreDirtyCheckout(g.canonical, state); check("S6", "the checkout and stash list are restored exactly", s6.restore); }
        s6.stash = sameListing("git stash list", stashBefore, g.stash());
        if (!s6.stash.ok) check("S6", "git stash list is unchanged by S6", s6.stash);
      }
    }
  }

  // S7 negatives ----------------------------------------------------------------------------------------------------------------------------
  if (wanted.includes("S7")) for (const n of NEGATIVES) {
    const rs = tracked.find((f) => /\.rs$/.test(f)); const toml = (layout?.manifests ?? tracked.filter((f) => /(^|\/)Cargo\.toml$/.test(f))).find((f) => !/\[workspace\]/.test(g.show(commit, f)));
    const recipes = {
      "misformat-rs": rs && [{ path: rs, content: `${g.show(commit, rs)}\nfn   gate_acceptance_misformatted ( ) {let x=1;}\n` }],
      "secret-file": [{ path: "docs/gate-acceptance-secret.txt", content: `token = "${["gh", "p_"].join("")}${"A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8"}"\n` }],
      "profile-dev": toml && [{ path: toml, content: `${g.show(commit, toml)}\n[profile.dev]\nopt-level = 1\n` }],
      "extra-workspace": [{ path: "gate-acceptance-extra/Cargo.toml", content: "[workspace]\nmembers = []\n" }],
      bypass: null, "no-broker": null,
    };
    const recipe = recipes[n.build];
    let c = commit;
    if (n.build === "bypass") {
      const a = await runGate({ gate, cwd: g.canonical, args: ["--to", "check", "--commit", commit, "--no-cache"] });
      const envRun = await runGate({ gate, cwd: g.canonical, args: ["--to", "check", "--commit", commit], env: { ...process.env, RK_GATE_CACHE: "0" } });
      const r1 = finishRun({ label: "N13:flag", stage: "check", commit, full: ["--no-cache"], res: a, receiptPath: null, step: "S7", extra: { negative: "N13", negativeResult: bypassRefused({ exit: a.exit, text: a.log }) } });
      const r2 = finishRun({ label: "N13:env", stage: "check", commit, full: ["RK_GATE_CACHE=0"], res: envRun, receiptPath: null, step: "S7", extra: { negative: "N13", negativeResult: bypassRefused({ exit: envRun.exit, text: envRun.log }) } });
      check("S7", "N13 --no-cache refused", r1.negativeResult); check("S7", "N13 RK_GATE_CACHE=0 refused", r2.negativeResult);
      continue;
    }
    if (recipe === undefined || recipe === null && n.build !== "no-broker") { stepOf("S7").notes.push(`${n.id}: unchecked: no tracked file to edit (${NEEDS_FIXTURE})`); stepOf("S7").checks.push("unchecked"); continue; }
    if (recipe) c = g.witnessCommit(commit, n.id.toLowerCase(), recipe);
    const r = await doRun({ label: `${n.id}`, stage: "compile", commit: c, args: n.args ?? [], ladder: false, step: "S7", extra: { negative: n.id } });
    r.negativeResult = assertNegative({ exit: r.exit, text: r.log, signals: n.signals, laterStages: n.later, receipt: r.receipt, rows: r.rows, expectRows: n.expectRows ?? [] });
    check("S7", `${n.id} ${n.title}`, r.negativeResult);
  }

  // S8 kill and recover ---------------------------------------------------------------------------------------------------------------------
  // Build ownership is held across Class A by design (build-ownership.mjs:3-4), so a SIGKILL during --to check leaves a record exactly like one
  // during --to compile: every case asserts that the NEXT run reclaims it (the log's reclaim line) and passes.
  if (wanted.includes("S8")) for (const k of KILLS) {
    const label = `S8:${k.id}`; const logPath = path.join(outDir, `${label.replace(/[^\w.-]/g, "_")}.log`);
    const stage = k.id === "K02" ? "check" : "compile"; const receiptPath = path.join(outDir, `${label.replace(/[^\w.-]/g, "_")}.receipt.json`);
    const killed = await killDuring({ gate, cwd: g.canonical, args: ["--to", stage, "--commit", commit, "--receipt-out", receiptPath], signal: k.signal, waitFor: readOwner, afterMs: killAfterMs });
    fs.writeFileSync(logPath, killed.log);
    const before = readOwner();
    const dry = before ? runDry(ownerFile) : { exit: null, text: "" };
    const next = await doRun({ label: `${label}:next`, stage, ladder: true, step: "S8" });
    const after = readOwner();
    const recovery = assertRecovery({ signal: k.signal, before, dry, next: { exit: next.exit, text: next.log }, after, exitedBySignal: exitedBySignal({ signal: killed.signal, exit: killed.exit, killSignal: k.signal, platform: process.platform }) });
    runs[label] = { label, kill: k.id, stage, recovery, logPath, ownerPath: ownerFile, exit: killed.exit, signal: killed.signal, log: killed.log, ladder: false, args: [] };
    check("S8", `${k.id} ${k.signal}: dead owner reclaimed by the next run, which passes`, recovery);
  }

  // S9 ---------------------------------------------------------------------------------------------------------------------------------------
  if (wanted.includes("S9")) {
    const brokerEnd = runningBrokerProcess() ?? null; const owner = readOwner();
    const snapshotDirCheck = snapshotDirProblems({ key, before: ctx.s0?.snapshots ?? null, after: listDirs(snapRoot) });
    ctx.s9 = { stash: g.stash(), status: g.status(), worktree: g.worktrees(), snapshotDirCheck, broker: brokerEnd, ownerPath: ownerFile, ownerClean: { ok: owner === null, problems: owner === null ? [] : [`owner record stands after the run: handle ${owner.handle}, pid ${owner.pid}`] } };
    check("S9", "git status --short equals S0", sameListing("git status --short", ctx.s0.status, ctx.s9.status));
    check("S9", "git worktree list equals S0", sameListing("git worktree list", ctx.s0.worktree, ctx.s9.worktree));
    check("S9", "git stash list equals S0", sameListing("git stash list", ctx.s0.stash, ctx.s9.stash));
    check("S9", "no owner record stands", ctx.s9.ownerClean);
    check("S9", "one snapshot dir for the repository and no new one", snapshotDirCheck);
    check("S9", "the broker process is the one of S0", brokerUnchanged(ctx.s0.broker, brokerEnd));
    const markers = Object.values(runs).flatMap((r) => publicationMarkers(r.log ?? ""));
    check("S9", "no upload or publish marker in any log", { ok: markers.length === 0, problems: markers.map((m) => `upload or publish marker in a log: ${m.slice(0, 120)}`) });
  }

  // clause evaluation -----------------------------------------------------------------------------------------------------------------------
  for (const id of Object.keys(CHECKS)) {
    if (["GC-I22", "GC-I18"].includes(id) && wanted.includes("S0")) continue; // already evaluated at S0
    let r; try { r = CHECKS[id](ctx); } catch (e) { r = { status: "fail", evidence: [], problems: [`assertion threw: ${e.message}`] }; }
    clauseChecks[id].push(...(Array.isArray(r) ? r : [r]));
  }
  const notes = [`out ${outDir}`, "timing is relative and informational: the contract gives no latency threshold"];
  if (!allowInstall) notes.push(`--to ${INSTALL_STAGES.join(" and --to ")} not run: ${NEEDS_INSTALL_APPROVAL}`);
  const result = buildResult({ host, gate: gateIdentity, broker: { runtimeManifest: ctx.broker.manifest ? { path: ctx.broker.manifestPath, brokerDigest: ctx.broker.manifest.brokerDigest, version: ctx.broker.manifest.version } : null, pinsBrokerDigest: ctx.broker.pins?.brokerDigest ?? null, processAtStart: ctx.s0?.broker ?? null }, commit, steps: stepRecords, clauseChecks, startedAt, endedAt: new Date().toISOString(), notes, allowInstall, stepChecks });
  result.timings = Object.fromEntries(Object.entries(runs).map(([k, v]) => [k, { ms: v.ms, stages: v.timings }]));
  fs.writeFileSync(path.join(outDir, "result.json"), `${JSON.stringify(result, null, 2)}\n`);
  const summary = formatSummary(result); fs.writeFileSync(path.join(outDir, "summary.txt"), `${summary}\n`);
  console.log(summary); console.log(`result: ${path.join(outDir, "result.json")}`);
  return result.ok ? 0 : 1;
}

function runDry(ownerFile) {
  const r = spawnSync(process.execPath, [path.join(here, "ownership-recover.mjs"), ownerFile], { encoding: "utf8" });
  return { exit: r.status, text: `${r.stdout ?? ""}${r.stderr ?? ""}` };
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).then((code) => process.exit(code), (e) => { console.error(`gate-acceptance: ${e.stack ?? e}`); process.exit(2); });
}
