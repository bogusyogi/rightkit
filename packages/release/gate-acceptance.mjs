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
//  * The 19 non-negotiables NN01-NN19 (GATE-CONTRACT [GC-A27]) are asserted on every run from the run's own evidence (coverage, log, spawn ledger, budgets
//    file). Every gate invocation is started with RK_GATE_SPAWN_LEDGER naming a per-run file under --out. A selected non-negotiable with missing evidence
//    FAILS; it is never "unchecked". Wall time is judged only by NN18 against ci/gate-acceptance-budgets.json (a null budget fails: "no budget measured").
// The pure assertion functions are exported and tested by gate-acceptance.test.mjs; `--plan` prints the steps and clause mapping and runs nothing.
import { createHash } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
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
const ROW_OPTIONAL = ["code", "timing", "scanElapsedMs", "retryTiming", "batchAttempts", "batchedWith", "evidence", "filesRead"];
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
    // A member whose own result is a cached pass still fails when its grouped invocation failed (rightkit#1458: BATCH_REQUIREMENT_FAILED keeps
    // the grouped failure as evidence), so hit + fail is valid exactly when a batch attempt of that row failed.
    const batchFailed = Array.isArray(row.batchAttempts) && row.batchAttempts.some((a) => a?.status === "fail");
    if (row.coverage === "hit" && row.status !== "pass" && !(row.status === "fail" && (batchFailed || row.class === "B"))) problems.push(`${where}: a hit row has status ${JSON.stringify(row.status)}, expected pass`);
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

/**
 * S2 (GC-A24, GC-A19, NN01): a warm rerun of the same commit. Against the first run `prev` (id -> row) and the rerun `next`:
 *  - every Class A scan of the rerun answered `hit` (a `ran` there is a failure, a skipped row is a failure). There is no uncached family any more:
 *    pnpm-lockfile and advisory-exceptions took GC-A19/GC-A24 keys (commit af0704a05, 2026-11-05), so NN01 ("any Class A row is ran") covers them too;
 *  - the key of every row equals the first run's key byte for byte.
 */
export function assertWarmRerun(prev, next) {
  const problems = [];
  let hits = 0;
  for (const [id, row] of next) {
    if (row.coverage === "skipped" && row.status === "not-in-pass") continue;
    if (row.coverage === "skipped") { problems.push(`${id}: skipped (${row.reason}); a skipped row is never a pass`); continue; }
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
  const ranA = [...next.values()].filter((r) => r.class === "A" && r.coverage === "ran");
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
// ---------------------------------------------------------------------------------------------------------------------------------------------
// Non-negotiables NN01-NN19 (GATE-CONTRACT [GC-A27]). Adrian, 2026-11-05: "each implement like cache etc should be written into part of an e2e test i.e.
// final state. so anything that breaks it next time fails"; "no reruns, no repeat calls. no going over things that haven't changed. fully optimizations".
// `steps` are the S-steps that produce each one's evidence. A non-negotiable is never "unchecked" on a host that can run it: when its step ran, missing
// evidence is a failure with the reason; when its step was not selected (a `--steps` subset) it is "not-evaluated" and the verdict says the run is partial.
// ---------------------------------------------------------------------------------------------------------------------------------------------
export const NN = Object.freeze([
  { id: "NN01", steps: ["S2"], title: "An unchanged commit runs no Class A row", evidence: "warm --to check and --to compile coverage, twice" },
  { id: "NN02", steps: ["S2"], title: "An unchanged commit runs no Class B row", evidence: "warm --to compile coverage, twice" },
  { id: "NN03", steps: ["S3"], title: "One edited .rs file reruns only the rows that read it", evidence: "S3 coverage" },
  { id: "NN04", steps: ["S4"], title: "One edited manifest reruns only its workspace's rows", evidence: "S4 coverage" },
  { id: "NN05", steps: ["S2"], title: "Keys are stable across identical runs", evidence: "coverage keys of two warm runs of check and of compile" },
  { id: "NN06", steps: ["S1", "S2"], title: "Only executing rows carry the environment digest", evidence: "coverage keys, the log's env digest" },
  { id: "NN07", steps: ["S1", "S2", "S3", "S4", "S10"], title: "No identical child process twice in one run", evidence: "spawn ledger" },
  { id: "NN08", steps: ["S1", "S2", "S3", "S4", "S10"], title: "rustfmt is one process per bucket; no cargo fmt", evidence: "spawn ledger, coverage" },
  { id: "NN09", steps: ["S1", "S2", "S3", "S4", "S10"], title: "Non-compiling cargo never goes through the broker", evidence: "spawn ledger" },
  { id: "NN10", steps: ["S2"], title: "Inside a snapshot the resident gate answers cargo metadata", evidence: "spawn ledger of the warm runs" },
  { id: "NN11", steps: ["S1", "S2"], title: "A warm run proves the snapshot from the resident index, without re-hashing it", evidence: "gate log (`snapshot proof:` line)" },
  { id: "NN12", steps: ["S2"], title: "Installed dependencies are identified by pnpm's install record and reused", evidence: "gate log (reuse-match=hit), spawn ledger, env digest" },
  { id: "NN13", steps: ["S1", "S2", "S3", "S4", "S10"], title: "Class A takes no build-ownership lease", evidence: "gate log of every --to check" },
  { id: "NN14", steps: ["S2"], title: "Preflight and app rules run once per tree", evidence: "gate log of the warm runs" },
  { id: "NN15", steps: ["S2"], title: "Advice scans are cached per lock set and UTC day or hour", evidence: "warm coverage" },
  { id: "NN16", steps: ["S10"], title: "A unit list shares one Class A and runs no per-unit format check", evidence: "--units-file log, spawn ledger" },
  { id: "NN17", steps: ["S1", "S2"], title: "Class A lanes equal the host's cores; Class B batches compatible rows", evidence: "gate log, spawn ledger" },
  { id: "NN18", steps: ["S2"], title: "Warm time stays inside the host's budget", evidence: "run wall time and stamped log lines against ci/gate-acceptance-budgets.json" },
  { id: "NN19", steps: ["S0"], always: true, title: "The gate under test is the installed sealed gate", evidence: "--version, launcher" },
]);
export const NN_IDS = Object.freeze(NN.map((n) => n.id));
/** NN20-NN42 (GC-A27 as extended 2026-11-05). `post` ones are judged from the finished result (NN34, NN35). */
export const NN_EXT = Object.freeze([
  { id: "NN20", steps: ["S12"], title: "One edited crate recompiles only that crate and its dependents", evidence: "S12 crate-edit compile coverage" },
  { id: "NN21", steps: ["S12"], title: "One edited lockfile reruns only the rows that read it", evidence: "S12 lock-edit coverage" },
  { id: "NN22", steps: ["S2"], title: "A warm snapshot sync writes and removes nothing", evidence: "sync lines and spawn ledger of the warm runs" },
  { id: "NN23", steps: ["S13"], title: "A gate upgrade that changes no key-relevant code keeps every hit", evidence: "coverage of the previous and the newest sealed gate" },
  { id: "NN24", steps: ["S14"], title: "The cache survives the resident gate's restart and idle exit", evidence: "coverage of a run after the resident gate was killed" },
  { id: "NN25", steps: ["S2"], title: "A warm run starts no git-after-proof, version probe, pnpm, ownership probe or real cargo", evidence: "spawn ledger of the warm runs" },
  { id: "NN26", steps: ["S2"], title: "The gate asks the resident gate over its socket, in process", evidence: "spawn ledger" },
  { id: "NN27", steps: ["S1"], title: "Never two compiler passes over one crate, target and feature set", evidence: "S1 compile coverage" },
  { id: "NN28", steps: ["S2"], title: "Change detection comes from RightKit's file index and watcher", evidence: "gate log (change-source), spawn ledger" },
  { id: "NN29", steps: ["S16"], title: "Per-run identities never enter a key or a reuse record", evidence: "coverage keys of a run with different identities" },
  { id: "NN30", steps: ["S0"], always: true, title: "Both hosts pass", evidence: "the other host's contract-test receipt" },
  { id: "NN31", steps: ["S0"], always: true, title: "No gate is released or installed as trusted without a passing contract test", evidence: "release and install tooling calls --verify-release" },
  { id: "NN32", steps: ["S8"], title: "A killed run's ownership recovers by itself on both hosts", evidence: "S8 kill cases" },
  { id: "NN33", steps: ["S0"], always: true, title: "The contract and the test match", evidence: "docs/GATE-CONTRACT.md NN table" },
  { id: "NN34", steps: ["S0"], always: true, post: true, title: "Missing evidence fails; a partial run never passes", evidence: "the finished result" },
  { id: "NN35", steps: ["S0"], always: true, post: true, title: "The verdict shows coverage, and a release needs all of it", evidence: "the finished result, receipt" },
  { id: "NN36", steps: ["S0"], always: true, title: "The test is locked", evidence: "git history of the runner, the contract table, the budgets" },
  { id: "NN37", steps: ["S0"], always: true, title: "The test's author is not the implementer", evidence: "budgets.reviews" },
  { id: "NN38", steps: ["S1"], title: "Cross-target rows built with stand-ins are limited evidence", evidence: "coverage and receipt of the compile" },
  { id: "NN39", steps: ["S11"], title: "The parallelism is real, not configured", evidence: "spawn ledger start and end times of the cold runs" },
  { id: "NN40", steps: ["S2"], title: "Docs build as one command per workspace and feature round, on every compile", evidence: "spawn ledger and doc coverage rows of every compile run" },
  { id: "NN41", steps: ["S17"], title: "A deterministic failure is cached like a pass", evidence: "coverage of the failing commit run three times (same inputs twice, then a changed toolchain identity)" },
  { id: "NN42", steps: ["S2"], title: "Cross targets of one stage go in one broker command naming every --target", evidence: "spawn ledger and log of every compile run" },
]);
export const NN_ALL = Object.freeze([...NN, ...NN_EXT]);
export const NN_ALL_IDS = Object.freeze(NN_ALL.map((n) => n.id));
export const nnForStep = (stepId) => NN.filter((n) => n.steps.includes(stepId)).map((n) => n.id);
export const nnForStepAll = (stepId) => NN_ALL.filter((n) => n.steps.includes(stepId)).map((n) => n.id);
/** The labels of the four warm runs of S2 (two of check, two of compile), in run order. */
export const WARM_LABELS = Object.freeze(["S2:check", "S2:compile", "S2:check2", "S2:compile2"]);
export const BUDGETS_SCHEMA = "rightkit.gate-acceptance-budgets/1";
export const BUDGETS_FILE = path.resolve(here, "..", "..", "ci", "gate-acceptance-budgets.json");

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
  { id: "K02-int", signal: "SIGINT", at: "same point as K02", clauses: ["GC-A24"] },
]);
export const KILLS_UNCHECKED = Object.freeze({
  K03: "no defined kill point inside the snapshot sync is observable from outside the gate", K04: "the resident gate-serve process is not identifiable by the runner", K05: "needs a hook between broker submit and acknowledgement", K06: NEEDS_FIXTURE,
});

export const STEPS = Object.freeze([
  { id: "S0", nn: nnForStep("S0"), title: "preflight (static)", runs: [], clauses: ["GC-I22", "GC-I18", "GC-I01", "GC-I15", "GC-T20"], note: "fixture clean, listings recorded, vector hashes, broker digest manifest vs pins" },
  { id: "S1", nn: nnForStep("S1"), title: "baseline ladder on C0", runs: ladderStages().map((s) => `--to ${s}`), clauses: ["GC-A04", "GC-A15", "GC-A21", "GC-A22-render", "GC-A22-classA", "GC-A23", "GC-T04", "GC-T05", "GC-T19", "GC-I01", "GC-I03", "GC-I05"], note: "--to dev and --to qualify install an app: only with --allow-install (Adrian's approval); with it, --to qualify may refuse with a named release-tier rule, asserted as such" },
  { id: "S2", nn: nnForStep("S2"), title: "warm rerun, same commit (twice)", runs: ["--to check", "--to compile", "--to check", "--to compile"], clauses: ["GC-A24", "GC-A19", "GC-A21"], note: "every Class A row hits with its first-run key; no Class B row runs; the second pair of runs is the identical rerun NN05 compares" },
  { id: "S3", nn: nnForStep("S3"), title: "one-file edit (ephemeral commit)", runs: ["--to check"], clauses: ["GC-A24"], note: "needs a tracked rk/**/src/lib.rs (or --leaf-file); else unchecked" },
  { id: "S4", nn: nnForStep("S4"), title: "manifest edit (ephemeral commit)", runs: ["--to check"], clauses: ["GC-A19"], note: "needs a tracked Cargo.toml (or --manifest-file); else unchecked" },
  { id: "S5", nn: nnForStep("S5"), title: "lock edit", runs: [], clauses: ["GC-A19", "GC-A24"], note: "unchecked by design: a lock edit needs the fixture's own resolver; not generated by this runner", byDesignUnchecked: true },
  { id: "S6", nn: nnForStep("S6"), title: "dirty checkout", runs: ["--to compile"], clauses: ["GC-A02", "GC-A04", "GC-T06"], note: "refuses a dirty fixture; ephemeral commit (a compile not yet recorded) built while the checkout has a dirty tracked edit, an untracked file and a pushed stash; the snapshot is compared with the commit; the checkout and the stash list are restored exactly" },
  { id: "S7", nn: nnForStep("S7"), title: "negatives", runs: NEGATIVES.map((n) => n.id), clauses: ["GC-T05", "GC-A01", "GC-T14", "GC-A15", "GC-1A-R2", "GC-1A-R4"], note: "ephemeral commits under refs/witness/*; the rest listed unchecked with their reason" },
  { id: "S8", nn: nnForStep("S8"), title: "kill and recover", runs: KILLS.map((k) => k.id), clauses: ["GC-T06", "GC-A24"], note: "SIGKILL (during compile and during Class A) and SIGINT; ownership-recover.mjs dry run must explain the record; the next run reclaims it and passes" },
  { id: "S10", nn: nnForStep("S10"), title: "unit list (--units-file, two fixture units)", runs: ["--to check --units-file"], clauses: [], note: "NN16: the list shares one Class A and runs no cargo fmt -p; needs two units of the gate's unit grammar (crates/rightkit-<x>, packages/<x>) in the fixture, else NN16 fails naming the gap" },
  { id: "S11", nn: nnForStepAll("S11"), title: "cold run (the gate's cache dir moved aside under scratch, then restored)", runs: ["--to check", "--to compile"], clauses: [], note: "NN18 cold budgets; real-condition controls that NN01, NN02, NN03, NN04, NN14, NN15, NN20, NN21, NN24, NN29 fail on a run where every row really ran" },
  { id: "S12", nn: nnForStepAll("S12"), title: "compile-side edits: one crate, one lockfile, one manifest", runs: ["--to compile (crate edit)", "--to check (lock edit)", "--to compile (manifest edit)"], clauses: [], note: "ephemeral commits under refs/witness/*; NN20, NN21, NN04 on --to compile" },
  { id: "S13", nn: nnForStepAll("S13"), title: "gate upgrade: the two newest sealed gates run the same commit", runs: ["--to check (previous gate)", "--to check (newest gate)"], clauses: [], note: "NN23; with fewer than two sealed versions it FAILS: needs two installed versions" },
  { id: "S14", nn: nnForStepAll("S14"), title: "resident gate restart", runs: ["--to check"], clauses: [], note: "kills the resident gate (SIGTERM, a plain process stop), then a warm check must run no row (NN24)" },
  { id: "S15", nn: nnForStepAll("S15"), title: "workloads: RightKit and HeardRight warm check (read-only), Pulse compile with --pulse-compile", runs: ["--to check (rightkit)", "--to check (heardright)"], clauses: [], note: "NN18 per-workload budgets; no compile unless --pulse-compile" },
  { id: "S16", nn: nnForStepAll("S16"), title: "per-run identities differ", runs: ["--to check"], clauses: [], note: "NN29: a fake tool-call id, terminal session, temp dir and RK_GATE_* value; every key equals the warm run's" },
  { id: "S17", nn: nnForStepAll("S17"), title: "cached failure: a commit with a real compile error, run twice, then with another toolchain", runs: ["--to compile (failing commit)", "--to compile (same commit again)", "--to compile (RUSTUP_TOOLCHAIN changed)"], clauses: [], note: "NN41; ephemeral commit under refs/witness/* that adds compile_error! to one fixture crate; the second run's failing rows must be hit, the third (another installed toolchain) must run them again" },
  { id: "S9", nn: nnForStep("S9"), title: "end invariants", runs: [], clauses: ["GC-A01", "GC-A02", "GC-T14", "GC-T06", "GC-I15", "GC-T20"], note: "listings equal S0; one snapshot dir and no new one; one worktree; no upload; broker pid and start unchanged" },
]);

/** The plan `--plan` prints (and tests read): steps, runs and the clause mapping, with implemented vs unchecked. */
export function buildPlan({ steps = STEPS.map((s) => s.id), allowInstall = false } = {}) {
  const chosen = STEPS.filter((s) => steps.includes(s.id));
  const implemented = new Set(Object.keys(CHECKS));
  const clauses = Object.values(CLAUSES).map((c) => ({ id: c.id, ev: c.ev, title: c.title, implemented: implemented.has(c.id), steps: chosen.filter((s) => s.clauses.includes(c.id)).map((s) => s.id), ...(implemented.has(c.id) ? {} : { unchecked: installReason(c.id, allowInstall) ?? UNCHECKED_WHY[c.id] ?? NO_EVIDENCE }) }));
  const nonNegotiables = NN.map((n) => ({ id: n.id, title: n.title, steps: n.steps, evidence: n.evidence, selected: Boolean(n.always) || n.steps.every((x) => chosen.some((c) => c.id === x)) }));
  return { schema: "rightkit.gate-acceptance-plan/1", allowInstall, installStagesSkipped: allowInstall ? [] : [...INSTALL_STAGES], steps: chosen, nonNegotiables, negativesUnchecked: NEGATIVES_UNCHECKED, killsUnchecked: KILLS_UNCHECKED, clauses };
}

/** The reason a clause that needs `--to dev` / `--to qualify` evidence is unchecked while installs are not approved; null otherwise. */
export const installReason = (id, allowInstall) => (!allowInstall && INSTALL_CLAUSES.includes(id) ? NEEDS_INSTALL_APPROVAL : null);

export function formatPlan(plan) {
  const out = [];
  for (const s of plan.steps) out.push(`${s.id}  ${s.title}${s.runs.length ? `  [${s.runs.join(", ")}]` : ""}`, `      clauses: ${s.clauses.join(" ") || "-"}`, `      non-negotiables: ${s.nn?.join(" ") || "-"}${s.nn?.includes("NN19") ? " (evaluated in every run)" : ""}`, `      ${s.note}`);
  const impl = plan.clauses.filter((c) => c.implemented);
  const un = plan.clauses.filter((c) => !c.implemented);
  out.push("", `clauses: ${plan.clauses.length} total, ${impl.length} asserted, ${un.length} unchecked by design`);
  out.push(`asserted:  ${impl.map((c) => `${c.id}(${c.ev})`).join(" ")}`);
  out.push(`unchecked: ${un.map((c) => `${c.id}(${c.ev})`).join(" ")}`);
  if (plan.installStagesSkipped?.length) out.push(`install stages not run: --to ${plan.installStagesSkipped.join(", --to ")} (they install an app; pass --allow-install only with Adrian's approval); clauses needing them: ${INSTALL_CLAUSES.join(" ")}`);
  out.push(`negatives not built: ${Object.keys(plan.negativesUnchecked).join(" ")}`, `kills not built: ${Object.keys(plan.killsUnchecked).join(" ")}`);
  out.push("", `non-negotiables (${(plan.nonNegotiables ?? []).length}; never unchecked on a host that can run them: missing evidence fails the run):`);
  for (const n of plan.nonNegotiables ?? []) out.push(`  ${n.id}  steps ${n.steps.join("+")}${n.selected ? "" : " (not selected)"}  ${n.title}  [${n.evidence}]`);
  out.push("", `extended non-negotiables NN20-NN42 (${NN_EXT.length}):`);
  for (const n of NN_EXT) out.push(`  ${n.id}  steps ${n.steps.join("+")}${n.always ? " (every run)" : ""}  ${n.title}  [${n.evidence}]`);
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

// ---------------------------------------------------------------------------------------------------------------------------------------------
// Non-negotiable assertions (pure). Each returns { ok, problems[] } and fails closed: evidence that is missing is a problem, never a pass.
// ---------------------------------------------------------------------------------------------------------------------------------------------

/** One ledger line per child process (spawn-ledger.mjs): {pid, at, ms, sync, cmd, args[], cwd, status}. Malformed lines are problems. */
export function parseLedger(text) {
  const entries = []; const problems = [];
  String(text ?? "").split(/\r?\n/).forEach((line, i) => {
    if (!line.trim()) return;
    try {
      const e = JSON.parse(line);
      if (!isObject(e) || typeof e.cmd !== "string" || !Array.isArray(e.args) || typeof e.cwd !== "string") throw new Error("not a ledger entry {cmd, args[], cwd}");
      entries.push(e);
    } catch (err) { problems.push(`ledger line ${i + 1}: ${err.message}`); }
  });
  return { entries, problems };
}

/** The program name of a ledger command: its basename without a Windows launcher suffix. */
export const programOf = (cmd) => String(cmd).replace(/\\/g, "/").split("/").pop().replace(/\.(exe|cmd|bat)$/i, "");

/** `{ program, args, cwd }` of an entry. A shell-string start (execSync: cmd holds the whole line, args is empty) is split on whitespace. */
export function normalizeSpawn(e) {
  const args = e.args.map(String);
  if (args.length === 0 && /\s/.test(String(e.cmd).trim())) { const [first, ...rest] = String(e.cmd).trim().split(/\s+/); return { program: programOf(first), args: rest, cwd: e.cwd, shim: false }; }
  return { program: programOf(e.cmd), args, cwd: e.cwd, shim: /rk-cargo-shim/.test(String(e.cmd)) };
}

const CARGO_VALUE_FLAGS = new Set(["-C", "-Z", "--config", "--color", "--manifest-path"]);
/** The cargo subcommand a start carries and how it is routed: `{ via: "cargo"|"shim"|"rustright", sub }`, or null when the start is no cargo call. */
export function cargoCall(entry) {
  const n = entry.program ? entry : normalizeSpawn(entry);
  let rest;
  let via;
  if (n.program === "cargo-fmt") return { via: "cargo", sub: "fmt" };
  if (n.program === "rustright") { const i = n.args.indexOf("cargo"); if (i < 0) return null; rest = n.args.slice(i + 1); via = "rustright"; }
  else if (n.program === "cargo") { rest = n.args; via = n.shim ? "shim" : "cargo"; }
  else return null;
  for (let i = 0; i < rest.length; i += 1) {
    const a = rest[i];
    if (a.startsWith("+")) continue;
    if (CARGO_VALUE_FLAGS.has(a)) { i += 1; continue; }
    if (a.startsWith("-")) continue;
    return { via, sub: a };
  }
  return { via, sub: null };
}

/** The allowlist of NN07: probes that truly must repeat inside one gate invocation. Each entry names its program, its exact args and a reason. Empty by decision: nothing in the code needs a repeat. */
// EMPTY by decision: any entry needs Adrian's approval by name (GC-A27 NN07/NN25). Do not add one without it.
export const NN07_ALLOWLIST = Object.freeze([]);

/** NN07: per-run parts of an argument or directory (temp dirs of probes and shims, the temp root, pid values) are normalised before two starts are compared. */
export const normalizeRunPart = (s) => String(s).replace(/(rk-probe-|rk-gate-shim-|rk-cargo-shim-)[A-Za-z0-9_-]+/g, "$1*").replace(/\/var\/folders\/[^/]+\/[^/]+\/T\//g, "$TMP/").replace(/\b(pid|PID)([= :]?)\d+/g, "$1$2*");
const spawnKey = (n) => `${n.program}\u0000${n.args.map(normalizeRunPart).join("\u0000")}\u0000${normalizeRunPart(n.cwd)}`;
const shortArgs = (args) => args.join(" ").replace(/\s+/g, " ").slice(0, 150);

/** NN07: the same program basename with the same arguments in the same directory starts once per gate invocation (parent and children together). */
export function assertNoDuplicateSpawns(entries, { allow = NN07_ALLOWLIST } = {}) {
  const problems = [];
  for (const a of allow) if (!isObject(a) || typeof a.program !== "string" || !Array.isArray(a.args) || typeof a.reason !== "string" || !a.reason.trim()) problems.push(`NN07 allowlist entry without program, args and a reason: ${JSON.stringify(a)}`);
  const allowed = new Set(allow.filter((a) => isObject(a) && typeof a.reason === "string" && a.reason.trim()).map((a) => `${a.program}\u0000${(a.args ?? []).join("\u0000")}`));
  const seen = new Map();
  for (const e of entries) {
    const n = normalizeSpawn(e);
    if (allowed.has(`${n.program}\u0000${n.args.join("\u0000")}`)) continue;
    const k = spawnKey(n);
    const prev = seen.get(k);
    if (prev) prev.count += 1; else seen.set(k, { count: 1, n });
  }
  const repeats = [...seen.values()].filter((v) => v.count > 1).sort((a, b) => b.count - a.count);
  for (const v of repeats.slice(0, 25)) problems.push(`${v.count}x ${v.n.program} ${shortArgs(v.n.args)} (cwd ${v.n.cwd})`);
  if (repeats.length > 25) problems.push(`... and ${repeats.length - 25} more repeated start(s)`);
  if (repeats.length) problems.unshift(`${repeats.length} identical child process(es) started more than once (${repeats.reduce((s, v) => s + v.count - 1, 0)} redundant start(s) of ${entries.length})`);
  return { ok: problems.length === 0, problems, distinct: seen.size, starts: entries.length, repeats: repeats.length };
}

const RUSTFMT_PROBE = /^(--version|-V|-v|--help|-h)$/;
/** NN08: no `cargo fmt` (through cargo, the shim or rustright), and rustfmt formats once per bucket that ran (a `--version` probe is not a format). `rows` is the run's coverage map. */
export function assertRustfmtSpawns(entries, rows) {
  const problems = [];
  if (!rows) problems.push("no coverage rows: the rustfmt buckets that ran cannot be counted");
  const ns = entries.map(normalizeSpawn);
  const fmt = ns.filter((n) => cargoCall(n)?.sub === "fmt");
  for (const n of fmt.slice(0, 5)) problems.push(`cargo fmt started: ${n.program} ${shortArgs(n.args)} (cwd ${n.cwd})`);
  const buckets = rows ? [...rows.values()].filter((r) => /^rustfmt(:|$)/.test(r.id) && r.coverage === "ran").length : 0;
  const formats = ns.filter((n) => n.program === "rustfmt" && !(n.args.length === 1 && RUSTFMT_PROBE.test(n.args[0])));
  if (rows && formats.length > buckets) problems.push(`${formats.length} rustfmt process(es) formatted for ${buckets} rustfmt bucket(s) that ran: ${formats.slice(0, 3).map((n) => shortArgs(n.args)).join(" | ")}`);
  return { ok: problems.length === 0, problems, formats: formats.length, buckets };
}

export const BROKER_FORBIDDEN_CARGO = Object.freeze(["metadata", "fetch", "tree", "locate-project", "pkgid", "verify-project", "read-manifest"]);
/** NN09: `rustright cargo <non-compiling subcommand>` (or the gate's shim, which carries into the broker) never starts. */
export function assertNoBrokerNonCompiling(entries) {
  const problems = [];
  for (const e of entries) {
    const n = normalizeSpawn(e); const c = cargoCall(n);
    if (c && c.via !== "cargo" && c.sub && BROKER_FORBIDDEN_CARGO.includes(c.sub)) problems.push(`${c.via === "rustright" ? "rustright cargo" : "shim cargo"} ${c.sub} went through the broker: ${shortArgs(n.args)} (cwd ${n.cwd})`);
  }
  return { ok: problems.length === 0, problems };
}

/** NN10: a warm run starts no real `cargo metadata` (direct, shim or broker) for the unchanged snapshot: the resident gate answers it. */
export function assertNoRealCargoMetadata(entries) {
  const problems = [];
  const hits = entries.map(normalizeSpawn).filter((n) => cargoCall(n)?.sub === "metadata");
  if (hits.length) problems.push(`${hits.length} real cargo metadata start(s) in a warm run: ${[...new Set(hits.map((n) => n.cwd))].slice(0, 6).join(", ")}`);
  return { ok: problems.length === 0, problems, starts: hits.length };
}

/** `snapshot proof: tree-hashes=<n> verify-walks=<n> resident-proofs=<n>`: the line the gate must print once per run that touches a snapshot (the evidence NN11 needs). */
export const SNAPSHOT_PROOF = /snapshot proof: tree-hashes=(\d+) verify-walks=(\d+) resident-proofs=(\d+)/;
/** NN11: the snapshot tree is hashed at most once per run, and a warm run walks it never (the resident index proves it). */
export function assertSnapshotProof(log, { warm }) {
  const problems = [];
  const m = SNAPSHOT_PROOF.exec(String(log));
  if (!m) return { ok: false, problems: ["the log has no `snapshot proof: tree-hashes=<n> verify-walks=<n> resident-proofs=<n>` line: the gate does not report how often it hashed or walked the snapshot, so NN11 cannot be judged"] };
  const [hashes, walks, resident] = [Number(m[1]), Number(m[2]), Number(m[3])];
  if (hashes > 1) problems.push(`the snapshot tree was hashed ${hashes} times in one run, at most 1 is allowed`);
  if (warm && walks > 0) problems.push(`a warm run walked the snapshot ${walks} time(s) to verify it (${resident} resident proof(s)); the resident index must prove it`);
  if (warm && resident < 1) problems.push("a warm run reports no resident proof of the snapshot");
  return { ok: problems.length === 0, problems, hashes, walks, resident };
}

const DEP_LINE = /snapshot dependencies root=(\S+)(.*)$/gm;
/** The `env=` digest of the log's `class A bytes ...` line, and `env-at-start=`; null when the line is absent. */
export function envDigestOf(log) {
  const line = /class A bytes [^\n]*/.exec(String(log))?.[0];
  if (!line) return null;
  return { env: /\benv=([0-9a-f]{64})/.exec(line)?.[1] ?? null, start: /\benv-at-start=([0-9a-f]{64})/.exec(line)?.[1] ?? null };
}
/** NN12: every installed root's record matched (`reuse-match=hit`), no `pnpm install` started, and the dependency digest did not change inside the run. */
export function assertDependencyReuse(log, entries) {
  const problems = [];
  const roots = [...String(log).matchAll(DEP_LINE)].filter((m) => !/ not installed:/.test(m[2]));
  if (roots.length === 0) problems.push("the log has no `snapshot dependencies root=...` line for an installed root");
  for (const m of roots) { const v = /reuse-match=(\S+)/.exec(m[2])?.[1]; if (v !== "hit") problems.push(`root ${m[1]}: reuse-match=${v ?? "(absent)"}, expected hit`); }
  const installs = entries.map(normalizeSpawn).filter((n) => n.program === "pnpm" && n.args.includes("install"));
  for (const n of installs.slice(0, 3)) problems.push(`pnpm install started on a warm run (cwd ${n.cwd})`);
  const d = envDigestOf(log);
  if (!d || !d.env || !d.start) problems.push("the log has no `class A bytes ... env-at-start=<digest> env=<digest>` line");
  else if (d.env !== d.start) problems.push(`the dependency digest changed during the run (env-at-start ${d.start.slice(0, 12)} -> env ${d.env.slice(0, 12)})`);
  return { ok: problems.length === 0, problems };
}
/** NN12 (two identical runs): the dependency digest is the same. */
export function assertDigestStable(a, b) {
  const da = envDigestOf(a); const db = envDigestOf(b);
  if (!da?.env || !db?.env) return { ok: false, problems: ["a warm run's log has no `class A bytes ... env=<digest>` line"] };
  return da.env === db.env ? { ok: true, problems: [] } : { ok: false, problems: [`the dependency digest differs between identical runs: ${da.env.slice(0, 12)} vs ${db.env.slice(0, 12)}`] };
}

/** NN13: a `--to check` log never reports build ownership held or joined. */
export function assertNoOwnershipOnCheck(log) {
  const lines = String(log).split(/\r?\n/).filter((l) => /build ownership (held|joined) /.test(l));
  return lines.length === 0 ? { ok: true, problems: [] } : { ok: false, problems: [`--to check took the build-ownership lease: ${lines[0].replace(/^\S+ /, "").slice(0, 120)}`] };
}

/** NN14: a warm run reports the app rules cached, and any preflight line it prints says cached too. */
export function assertRulesCached(log) {
  const problems = [];
  const text = String(log);
  if (!/app rules: cached pass/.test(text)) problems.push("the log has no `app rules: cached pass` line: the app rules ran again on an unchanged tree");
  for (const l of text.split(/\r?\n/)) if (/\bpreflight: /.test(l) && !/cached pass/.test(l)) problems.push(`preflight ran again on an unchanged tree: ${l.replace(/^\S+ /, "").slice(0, 120)}`);
  return { ok: problems.length === 0, problems };
}

export const ADVICE_ROW = /^(dependency-freshness|ghsa-audit|pnpm-audit|cargo-deny|cargo-audit)(:|$)/;
const utcDay = (iso) => String(iso).slice(0, 10); const utcHour = (iso) => String(iso).slice(0, 13);
/**
 * NN15: no advice scan runs on a warm run of the same UTC day. The one exception is a row whose key moved while the two runs straddle a UTC day or hour
 * boundary (the key carries the day or hour). `startedAt`/`prevStartedAt` are the coverage reports' own timestamps.
 */
export function assertAdviceCached(prev, next, { startedAt, prevStartedAt } = {}) {
  const problems = [];
  const advice = [...next.values()].filter((r) => ADVICE_ROW.test(r.id));
  if (advice.length === 0) problems.push("no advice rows (dependency-freshness, ghsa-audit, pnpm-audit, cargo-deny, cargo-audit) in the warm run's coverage");
  const crossed = Boolean(startedAt && prevStartedAt && (utcDay(startedAt) !== utcDay(prevStartedAt) || utcHour(startedAt) !== utcHour(prevStartedAt)));
  for (const r of advice) {
    if (r.coverage === "skipped" && r.status !== "not-in-pass") problems.push(`${r.id}: skipped (${r.reason})`);
    if (r.coverage !== "ran") continue;
    const before = prev?.get(r.id);
    if (crossed && before && before.key !== r.key) continue;
    problems.push(`${r.id}: ran on a warm run of the same UTC day${r.key === null ? " (no cache key)" : before && before.key === r.key ? " with the key it already had" : ""}`);
  }
  return { ok: problems.length === 0, problems, advice: advice.length };
}

/** NN16: a unit list runs Class A once (one coverage report; the later units say it is not run again) and starts no `cargo fmt`. */
export function assertUnitListShare(log, entries, { units }) {
  const problems = [];
  if (!Number.isInteger(units) || units < 2) problems.push(`the unit list names ${units} unit(s), NN16 needs two`);
  const reports = coveragePaths(log).length;
  if (reports !== 1) problems.push(`Class A produced ${reports} coverage report(s) for the unit list, expected exactly 1`);
  const memo = String(log).split(/\r?\n/).filter((l) => /class A \(\w+ tier\) of this unit list already (passed|failed)[^\n]*not run again/.test(l)).length;
  if (units >= 2 && memo < units - 1) problems.push(`${memo} later unit(s) reused the shared Class A, expected ${units - 1}`);
  for (const n of entries.map(normalizeSpawn)) { const c = cargoCall(n); if (c?.sub === "fmt") problems.push(`cargo fmt started for a unit list: ${n.program} ${shortArgs(n.args)}`); }
  return { ok: problems.length === 0, problems, reports, memo };
}

/** NN17 lanes: every Class A plan line reports as many lanes as the host has cores. */
export function assertLanes(log, { cores, jobsOverride = undefined }) {
  const problems = [];
  if (jobsOverride !== undefined && jobsOverride !== "") problems.push(`RK_GATE_JOBS=${jobsOverride} is set in the runner's environment: the lane count is not the default`);
  const lanes = [...String(log).matchAll(/class A scans (\d+) at a time/g)].map((m) => Number(m[1]));
  if (lanes.length === 0) problems.push("the log has no `class A scans N at a time` line");
  for (const n of lanes) if (n !== cores) problems.push(`class A ran ${n} lane(s) on a ${cores}-core host`);
  return { ok: problems.length === 0, problems, lanes };
}
const COMPILING_CARGO = new Set(["check", "clippy", "build", "test", "doc", "rustc", "nextest", "run", "bench"]);
/** NN17 batching: the compile log reports shared groups, and the number of compiling cargo starts stays within the groups plus the named solo scans. */
export function assertBatching(log, entries, { exit }) {
  const problems = [];
  const text = String(log);
  const note = /class B batching: (\d+) compile scan\(s\) in (\d+) configuration group\(s\)[^\n]*?\((\d+) scan\(s\) in (\d+) shared group\(s\)\)/.exec(text);
  if (!note) return { ok: false, problems: ["the compile log has no `class B batching: N compile scan(s) in G configuration group(s) ... (M scan(s) in K shared group(s))` note"] };
  const [scans, groups, shared, sharedGroups] = note.slice(1).map(Number);
  const solo = [...text.matchAll(/class B batching: (\d+) compile scan\(s\) stay their own invocation/g)].reduce((s, m) => s + Number(m[1]), 0);
  if (scans >= 2 && (shared === 0 || sharedGroups === 0)) problems.push(`${scans} Class B scans planned and no compatible group was batched (${shared} scan(s) in ${sharedGroups} shared group(s))`);
  if (exit === 0) {
    const invocations = entries.map(normalizeSpawn).filter((n) => { const c = cargoCall(n); return c && c.via !== "cargo" && COMPILING_CARGO.has(c.sub); });
    if (invocations.length > groups + solo) problems.push(`${invocations.length} compiling cargo invocation(s) for ${groups} configuration group(s) and ${solo} solo scan(s): a compatible group ran as separate invocations`);
  }
  return { ok: problems.length === 0, problems, scans, groups, shared, sharedGroups, solo };
}

const EXECUTING_ROW = /^(tsc|eslint|biome|vitest|test|generated-docs|swift-typecheck|pnpm-lockfile|pnpm-audit|pnpm)(:|$)/;
/** Whether a Class A row executes repository code or installed dependencies and may therefore carry the environment digest (gate: `executes` or tool pnpm). */
export const isExecutingRow = (id) => EXECUTING_ROW.test(id);
/** Whether a coverage key carries one of the environment digests (verbatim, or as the value of a property named like env). */
export function keyCarriesEnvironment(key, digests) {
  if (typeof key !== "string") return false;
  for (const d of digests) if (key.includes(d) || key.includes(d.slice(0, 32))) return true;
  try {
    const walk = (v, name) => {
      if (typeof v === "string") return /env/i.test(name) && /^[0-9a-f]{32,64}$/.test(v);
      if (Array.isArray(v)) return v.some((x) => walk(x, name));
      if (isObject(v)) return Object.entries(v).some(([k, x]) => walk(x, k));
      return false;
    };
    return walk(JSON.parse(key), "");
  } catch { return false; }
}
/** NN06: no non-executing row's key carries the environment digest. `digests` are the env digests the runs' logs print. */
export function assertEnvDigestScope(rows, digests) {
  const problems = [];
  if (digests.length === 0) return { ok: false, problems: ["no run log prints a class A `env=<digest>`: the digest a non-executing key must not carry cannot be identified"] };
  for (const r of rows.values()) {
    if (r.class !== "A" || r.coverage === "skipped" || isExecutingRow(r.id)) continue;
    if (keyCarriesEnvironment(r.key, digests)) problems.push(`${r.id}: a non-executing row's key carries the environment digest`);
  }
  return { ok: problems.length === 0, problems };
}

/** NN05: two identical runs give every row the same key. A scan answered in only one run, or one without a key, cannot show stability and fails. */
export function assertKeysStable(a, b) {
  const problems = [];
  const answered = (r) => r && !(r.coverage === "skipped" && r.status === "not-in-pass");
  for (const id of new Set([...a.keys(), ...b.keys()])) {
    const x = a.get(id); const y = b.get(id);
    if (!answered(x) && !answered(y)) continue;
    if (!answered(x) || !answered(y)) { problems.push(`${id}: present in only one of the two runs`); continue; }
    if (x.coverage === "skipped" && y.coverage === "skipped") continue;
    if (x.key === null || y.key === null) { problems.push(`${id}: no cache key in ${x.key === null ? "the first" : "the second"} run, so its stability cannot be shown`); continue; }
    if (x.key !== y.key) problems.push(`${id}: key differs between identical runs`);
  }
  return { ok: problems.length === 0, problems };
}

/** NN01: no Class A row ran on an unchanged commit. */
export function assertNoClassARan(rows) {
  const ran = [...rows.values()].filter((r) => r.class === "A" && r.coverage === "ran");
  return { ok: ran.length === 0, problems: ran.map((r) => `${r.id}: Class A row ran on an unchanged commit (${r.reason})`), ran: ran.map((r) => r.id) };
}
/** NN02: no Class B row ran on an unchanged commit. A row that failed in `prev` and fails again is reported in `reported`, never counted as a pass of the run. */
export function assertNoClassBRan(rows, prev = null, { strict = false } = {}) {
  const problems = []; const reported = [];
  const b = [...rows.values()].filter((r) => r.class === "B");
  if (b.length === 0) problems.push("the compile coverage has no Class B row");
  for (const r of b) {
    if (r.coverage !== "ran") continue;
    const before = prev?.get(r.id);
    if (!strict && r.status === "fail" && before?.status === "fail") { reported.push(`${r.id}: failed before and still fails (reported, not re-run silently)`); continue; }
    problems.push(`${r.id}: Class B row ran on an unchanged commit (${r.status})`);
  }
  return { ok: problems.length === 0, problems, reported };
}

const WORKSPACE_FAMILY = /^(cargo-metadata|cargo-deny|cargo-audit|ghsa-audit|cargo-package-list|clippy|check)(:|$)/;
/** NN04: after a manifest edit no workspace-scoped row of another workspace ran. */
export function assertOtherWorkspacesIdle(next, { workspace, workspaces = [], crates = {} }) {
  const problems = [];
  if (typeof workspace !== "string") return { ok: false, problems: ["the edited manifest's workspace is not known"] };
  const wsOf = (label) => (workspaces.includes(label) ? label : workspaces.find((w) => w === `crates/${label}`) ?? Object.entries(crates).find(([, names]) => names.includes(label.includes(":") ? label.slice(label.lastIndexOf(":") + 1) : label))?.[0] ?? null);
  let seenOwn = false;
  for (const r of next.values()) {
    const m = WORKSPACE_FAMILY.exec(r.id);
    if (!m) continue;
    const label = r.id.slice(m[0].length);
    const ws = m[1] === "cargo-metadata" && r.id === "cargo-metadata" ? "." : wsOf(label);
    if (ws === workspace) seenOwn = true;
    if (r.coverage !== "ran") continue;
    if (ws === null) problems.push(`${r.id}: ran and belongs to no known workspace`);
    else if (ws !== workspace) problems.push(`${r.id}: a row of workspace ${JSON.stringify(ws)} ran for an edit in workspace ${JSON.stringify(workspace)}`);
  }
  if (!seenOwn) problems.push(`no workspace-scoped row of ${JSON.stringify(workspace)} exists in the run`);
  return { ok: problems.length === 0, problems };
}

// ---- NN18: budgets and stage timings -------------------------------------------------------------------------------------------------------

const stampOf = (line) => { const m = /^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z) /.exec(line); return m ? Date.parse(m[1]) : null; };
/**
 * Stage durations from the stamped log. The line pairs (also written into ci/gate-acceptance-budgets.json):
 *  ownership       run start (the runner's launch time) -> first `build ownership held|joined` line; 0 when the run takes no lease (a `--to check`, NN13)
 *  snapshotSync    ownership line (else run start) -> `snapshot <dir> is commit <sha> tree <sha> (written=...` line
 *  planning        `rightkit-gate: --to <stage> for <sha12>: ...` line -> `<n> scan(s) for <m> changed file(s) vs <sha12>; class A scans <j> at a time` line
 *  dependencyStep  that plan line -> the first `snapshot dependencies root=` line (a compile prints a second one later, after Class A)
 * A named line that is missing is a problem (the stage cannot be timed), except the ownership line, whose absence is the stage being 0.
 */
export function stageMarks(log, startedMs) {
  const lines = String(log).split(/\r?\n/);
  const first = (re) => lines.find((l) => re.test(l));
  const last = (re) => [...lines].reverse().find((l) => re.test(l));
  const problems = [];
  const t = (line, what) => { const ms = line ? stampOf(line) : null; if (ms === null) problems.push(`no stamped \`${what}\` line`); return ms; };
  const own = first(/build ownership (held|joined) /);
  const ownT = own ? stampOf(own) : null;
  const sync = t(first(/snapshot \S+ is commit [0-9a-f]{40} tree [0-9a-f]{40} \(written=/), "snapshot ... is commit ... (written=");
  const stageStart = t(first(/rightkit-gate: --to \w+ for [0-9a-f]{12}/), "--to <stage> for <sha>");
  const plan = t(first(/\d+ scan\(s\) for \d+ changed file\(s\) vs [0-9a-f]{12}; class A scans \d+ at a time/), "<n> scan(s) for <m> changed file(s) ...; class A scans <j> at a time");
  const deps = t(first(/snapshot dependencies root=/), "snapshot dependencies root=");
  const span = (a, b, what) => { if (a === null || b === null) return null; if (b < a) { problems.push(`${what}: the closing line is stamped before the opening line`); return null; } return b - a; };
  const marks = {
    ownership: own ? span(startedMs, ownT, "ownership") : 0,
    snapshotSync: span(ownT ?? startedMs, sync, "snapshotSync"),
    planning: span(stageStart, plan, "planning"),
    dependencyStep: span(plan, deps, "dependencyStep"),
  };
  return { marks, problems };
}

export const BUDGET_STAGES = Object.freeze(["ownership", "snapshotSync", "dependencyStep", "planning"]);
/** Validates the budgets file: schema, a hosts object with mac and dell, every number positive or null. */
export function validateBudgets(b) {
  const problems = [];
  if (!isObject(b)) return { ok: false, problems: ["budgets file is not an object"] };
  if (b.schema !== BUDGETS_SCHEMA) problems.push(`budgets.schema is ${JSON.stringify(b.schema)}, expected ${BUDGETS_SCHEMA}`);
  if (!isObject(b.hosts)) problems.push("budgets.hosts is not an object");
  else for (const host of ["mac", "dell"]) {
    const h = b.hosts[host];
    if (!isObject(h)) { problems.push(`budgets.hosts.${host} is missing`); continue; }
    const num = (v, where) => { if (v !== null && !(Number.isFinite(v) && v > 0)) problems.push(`${where} is neither null nor a positive number`); };
    num(h.warmCheckMs, `hosts.${host}.warmCheckMs`); num(h.warmCompileMs, `hosts.${host}.warmCompileMs`);
    if (!isObject(h.stages)) problems.push(`hosts.${host}.stages is missing`); else for (const s of BUDGET_STAGES) { if (!(s in h.stages)) problems.push(`hosts.${host}.stages.${s} is missing`); else num(h.stages[s], `hosts.${host}.stages.${s}`); }
  }
  return { ok: problems.length === 0, problems };
}
export function loadBudgets(file = BUDGETS_FILE) {
  let parsed; try { parsed = JSON.parse(fs.readFileSync(file, "utf8")); } catch (e) { return { budgets: null, problem: `cannot read ${file}: ${e.message}` }; }
  const v = validateBudgets(parsed);
  return v.ok ? { budgets: parsed, problem: null } : { budgets: null, problem: `${file}: ${v.problems.join("; ")}` };
}
/**
 * NN18: each warm run's wall time and fixed-stage durations stay inside the host's budget. `runs` is `[{ label, kind: "check"|"compile", ms, marks }]`.
 * A null budget FAILS ("no budget measured"): a host with no measurement is never passed.
 */
export function assertBudget({ host, budgets, runs }) {
  const problems = [];
  const h = budgets?.hosts?.[host];
  if (!h) return { ok: false, problems: [`no budget for host ${host} in ${BUDGETS_FILE}`] };
  const need = (v, what) => { if (v === null || v === undefined) { problems.push(`no budget measured for ${host}: ${what}`); return false; } return true; };
  for (const r of runs) {
    const total = r.kind === "compile" ? h.warmCompileMs : h.warmCheckMs;
    if (need(total, r.kind === "compile" ? "warmCompileMs" : "warmCheckMs") && r.ms > total) problems.push(`${r.label}: warm --to ${r.kind} took ${r.ms} ms, budget ${total} ms`);
    for (const s of BUDGET_STAGES) {
      const v = r.marks?.[s];
      if (v === null || v === undefined) { problems.push(`${r.label}: stage ${s} could not be timed`); continue; }
      if (need(h.stages?.[s], `stages.${s}`) && v > h.stages[s]) problems.push(`${r.label}: stage ${s} took ${v} ms, budget ${h.stages[s]} ms`);
    }
  }
  if (runs.length === 0) problems.push("no warm run to measure");
  return { ok: problems.length === 0, problems };
}

// ---- NN19: the gate under test ----------------------------------------------------------------------------------------------------------------

/** The newest sealed install under `<managed root>/trusted-gates` (a folder named by the sha256 of its manifest, as agent-bin/rightkit-host-gate trusts it): `{ version, entry }` or null. */
export function findInstalledGate(managedRoot, io = {}) { return listInstalledGates(managedRoot, io)[0] ?? null; }
/** Every sealed install, newest version first. */
export function listInstalledGates(managedRoot, { read = (f) => fs.readFileSync(f), list = (d) => fs.readdirSync(d) } = {}) {
  const root = path.join(managedRoot, "trusted-gates");
  let names; try { names = list(root); } catch { return []; }
  const found = [];
  for (const name of names) {
    if (!/^[a-f0-9]{64}$/.test(name)) continue;
    try {
      if (sha256(read(path.join(root, `${name}.manifest.txt`))) !== name) continue;
      const pkgDir = path.join(root, name, "node_modules", "@rightkit", "release");
      const pkg = JSON.parse(String(read(path.join(pkgDir, "package.json"))));
      if (pkg?.name === "@rightkit/release" && /^\d+\.\d+\.\d+$/.test(pkg.version)) found.push({ version: pkg.version, sealedSha256: name, entry: path.join(pkgDir, "bin", "rightkit-gate.mjs") });
    } catch { /* not a sealed install */ }
  }
  const key = (v) => v.split(".").map(Number);
  found.sort((a, b) => { const x = key(a.version); const y = key(b.version); for (let i = 0; i < 3; i += 1) if (x[i] !== y[i]) return y[i] - x[i]; return 0; });
  return found;
}
/**
 * NN19: the gate that runs is the installed sealed gate. Without `--gate` the default launcher must exist, report the newest sealed install's version
 * and not exec the checkout's gate; a checkout gate is accepted only when `--gate` names it (and the result then says so: `kind: "named"`).
 */
export function gateIdentityProblems({ gateArg, defaultGate, launcherExists, launcherVersion, launcherText = "", installed, checkoutRoot }) {
  const problems = [];
  const named = gateArg !== undefined && gateArg !== null && path.normalize(gateArg) !== path.normalize(defaultGate);
  if (!launcherExists) problems.push(`the launcher ${named ? gateArg : defaultGate} does not exist`);
  if (!launcherVersion) problems.push("`--version` of the launcher printed no version");
  if (!named) {
    const entry = path.join(checkoutRoot, "packages", "release", "bin", "rightkit-gate.mjs");
    if (String(launcherText).includes(entry)) problems.push(`the default launcher runs the checkout gate ${entry} and no --gate names it`);
    if (!installed) problems.push("no sealed installed gate under trusted-gates, so the installed gate's identity cannot be established");
    else if (launcherVersion && launcherVersion !== installed.version) problems.push(`the launcher reports ${launcherVersion}, the newest sealed install is ${installed.version}: not the installed gate`);
  }
  return { ok: problems.length === 0, problems, kind: named ? "named" : "installed" };
}

// ---- Evaluation over a run's evidence --------------------------------------------------------------------------------------------------------

// ---------------------------------------------------------------------------------------------------------------------------------------------
// GC-A27 extension: NN20-NN38 and the tightened NN01-NN19. Every assertion below is an e2e judgement over the run's OWN evidence (coverage, log,
// spawn ledger, receipt, git history); none reads a synthetic input in production. That each one CAN fail is shown by `--prove` (control runs
// of real conditions and mutant gates), never by a unit test. Evidence the gate does not emit makes its check FAIL, naming the evidence to add.
// ---------------------------------------------------------------------------------------------------------------------------------------------

/** NN33: the NN table rows of the contract: `| NNxx | property | fails when |`. */
export function parseContractNN(text) {
  const rows = [];
  for (const line of String(text ?? "").split(/\r?\n/)) {
    const m = /^\|\s*(NN\d\d)\s*\|(.*)\|\s*$/.exec(line);
    if (!m) continue;
    const cells = m[2].split("|").map((s) => s.trim());
    rows.push({ id: m[1], property: cells[0] ?? "", fails: cells.slice(1).join(" | ") });
  }
  return rows;
}

/** NN33: every contract id has a check and every check id is in the contract (both directions, and the registry equals both). */
export function assertContractMatch({ contractRows, checkIds, registryIds }) {
  const problems = [];
  const contract = contractRows.map((r) => r.id);
  if (contract.length === 0) problems.push("the contract has no NN table rows (`| NNxx | ... |`): nothing to match");
  const dup = contract.filter((id, i) => contract.indexOf(id) !== i);
  for (const id of new Set(dup)) problems.push(`${id} appears more than once in the contract table`);
  const checks = new Set(checkIds); const registry = new Set(registryIds); const inContract = new Set(contract);
  for (const id of contract) { if (!checks.has(id)) problems.push(`${id} is in the contract and has no check in the runner`); if (!registry.has(id)) problems.push(`${id} is in the contract and not in the runner's registry`); }
  for (const id of checks) if (!inContract.has(id)) problems.push(`${id} has a check in the runner and is not in the contract`);
  for (const id of registry) if (!inContract.has(id)) problems.push(`${id} is in the runner's registry and not in the contract`);
  return { ok: problems.length === 0, problems };
}

export const CONTRACT_FILE = path.resolve(here, "..", "..", "docs", "GATE-CONTRACT.md");
export const RUNNER_FILE = fileURLToPath(import.meta.url);
const fileSha = (f) => { try { return sha256(fs.readFileSync(f)); } catch { return null; } };

// ---- NN25 / NN07 allowlists: EMPTY by decision. An entry needs Adrian's approval (GC-A27 NN25: "a probe that must run names its reason in the runner's
// allowlist, which Adrian approves"; NN07 likewise). Do not add one without his approval by name in the commit message.
export const NN25_ALLOWLIST = Object.freeze([]);
const OWNERSHIP_PROBES = new Set(["ps", "lsof", "launchctl", "sysctl"]);
const VERSION_ARG = /^(--version|-V|-v|version)$/;
const TREE_WALKERS = new Set(["find", "rsync", "du"]);
const GIT_TREE_VERBS = new Set(["ls-files", "ls-tree", "archive", "status", "diff", "diff-files", "diff-index", "update-index"]);

/** NN25: no child of the forbidden families on a warm run (git after the resident proof, version probes, pnpm, ownership probes, real cargo). */
export function assertWarmNoFamilies(entries, { log = "", allow = NN25_ALLOWLIST } = {}) {
  const problems = [];
  const proofLine = String(log).split(/\r?\n/).find((l) => /snapshot proof:|resident proof|\(verified before/.test(l));
  const proofAt = proofLine ? stampOf(proofLine) : null;
  const allowed = new Set(allow.filter((a) => isObject(a) && a.reason && a.program).map((a) => `${a.program}\u0000${(a.args ?? []).join("\u0000")}`));
  const found = new Map();
  for (const e of entries) {
    const n = normalizeSpawn(e);
    if (allowed.has(`${n.program}\u0000${n.args.join("\u0000")}`)) continue;
    const at = Date.parse(e.at);
    const call = cargoCall(n);
    let fam = null;
    if (n.args.length <= 2 && n.args.some((a) => VERSION_ARG.test(a))) fam = "tool version probe";
    else if (n.program === "git" && (proofAt === null || at >= proofAt)) fam = proofAt === null ? "git (the log carries no resident-proof time, so every git start counts)" : "git after the resident proof";
    else if (n.program === "pnpm") fam = "pnpm";
    else if (OWNERSHIP_PROBES.has(n.program)) fam = `ownership probe (${n.program})`;
    else if (call && call.via !== "rustright") fam = "real cargo";
    if (fam) { const k = `${fam}\u0000${n.program} ${shortArgs(n.args)}`; found.set(k, (found.get(k) ?? 0) + 1); }
  }
  for (const [k, c] of [...found].slice(0, 20)) { const [fam, cmd] = k.split("\u0000"); problems.push(`${fam}: ${c}x ${cmd}`); }
  if (found.size > 20) problems.push(`... and ${found.size - 20} more`);
  return { ok: problems.length === 0, problems };
}

/** NN22: every sync line of a warm run is written=0 removed=0 and the ledger shows no tree walk. */
export function assertWarmSync(log, entries) {
  const problems = [];
  const syncs = [...String(log).matchAll(/snapshot \S+ is commit [0-9a-f]{40} tree [0-9a-f]{40} \(written=(\d+) removed=(\d+)/g)];
  if (!syncs.length) problems.push("the log has no `snapshot <dir> is commit <sha> tree <sha> (written=N removed=N` line");
  for (const m of syncs) if (m[1] !== "0" || m[2] !== "0") problems.push(`the snapshot sync wrote ${m[1]} and removed ${m[2]} on an unchanged commit`);
  for (const n of entries.map(normalizeSpawn)) if (TREE_WALKERS.has(n.program) || (n.program === "git" && n.args.some((a) => GIT_TREE_VERBS.has(a)))) problems.push(`a tree walk started on a warm run: ${n.program} ${shortArgs(n.args)} (cwd ${n.cwd})`);
  return { ok: problems.length === 0, problems };
}

/** NN26: no `node gate-serve.mjs ask` process (the gate asks the resident gate in process over its socket). */
export function assertNoServeAskSpawn(entries) {
  const problems = [];
  for (const n of entries.map(normalizeSpawn)) if (n.args.some((a) => /gate-serve\.mjs$/.test(a)) && n.args.includes("ask")) problems.push(`a process was started to ask the resident gate: ${n.program} ${shortArgs(n.args)}`);
  return { ok: problems.length === 0, problems };
}

const cargoBFamily = (id) => {
  const bare = id.replace(/@[^@:]+$/, ""); const target = /@([^@:]+)$/.exec(id)?.[1] ?? "host"; const p = bare.split(":");
  if (p[0] === "test" && p[1] === "rust") return { fam: "test", ws: p[2], crate: p[3], idx: p[4] ?? "0", target };
  if (p[0] === "clippy") return { fam: "clippy", ws: p[1], crate: p[2], idx: p[3] ?? "0", target };
  if (p[0] === "check-features") return { fam: "check", ws: p[1], crate: p[2], idx: p[3] ?? "0", target };
  if (p[0] === "doc") return { fam: "doc", ws: p[1], crate: p[2], idx: "0", target };
  return null;
};
/** NN27: no check row runs where a clippy row covers the same configuration, and no two rows run the same compiler pass over one crate, target and feature set. */
export function assertNoDoublePass(rows) {
  const problems = [];
  const seen = new Map();
  for (const r of rows.values()) {
    if (r.class !== "B" || r.coverage !== "ran") continue;
    const f = cargoBFamily(r.id); if (!f || (f.fam !== "check" && f.fam !== "clippy")) continue;
    const key = `${f.ws}|${f.crate}|${f.idx}|${f.target}`;
    (seen.get(key) ?? seen.set(key, []).get(key)).push({ id: r.id, fam: f.fam });
  }
  for (const [key, list] of seen) if (list.length > 1) problems.push(`${list.length} compiler passes ran over ${key.split("|").join(" ")}: ${list.map((x) => x.id).join(", ")}`);
  return { ok: problems.length === 0, problems };
}

/** NN28: the resident gate learns changes from RightKit's file index and watcher; the gate must say so, and no polling child starts. */
export function assertChangeSource(log, entries) {
  const problems = [];
  const m = /snapshot proof:[^\n]*\bchange-source=(\S+)/.exec(String(log));
  if (!m) problems.push("the log has no `snapshot proof: ... change-source=fsindex,file-watch` field: the gate must report which change source the resident gate used (rightkit-fsindex, rightkit-file-watch), so NN28 cannot be judged");
  else if (m[1] !== "fsindex,file-watch") problems.push(`the resident gate's change source is ${m[1]}, expected fsindex,file-watch`);
  for (const n of entries.map(normalizeSpawn)) if (TREE_WALKERS.has(n.program) || (n.program === "git" && n.args.some((a) => GIT_TREE_VERBS.has(a)))) problems.push(`a change was learned by polling: ${n.program} ${shortArgs(n.args)}`);
  return { ok: problems.length === 0, problems };
}

/** NN23: every row the previous gate answered stays a hit, with its key, under the upgraded gate. */
export function assertUpgradeKeepsHits(prev, next) {
  const problems = [];
  let kept = 0;
  for (const [id, p] of prev) {
    if (p.coverage === "skipped") continue;
    const n = next.get(id);
    if (!n) { problems.push(`${id}: answered by the previous gate and absent after the upgrade`); continue; }
    if (n.coverage !== "hit") problems.push(`${id}: ${p.coverage} under the previous gate, ${n.coverage} after the upgrade`);
    else if (n.key !== p.key) problems.push(`${id}: its key changed across the upgrade`);
    else kept += 1;
  }
  if (kept === 0 && problems.length === 0) problems.push("no row was answered by the previous gate: the upgrade cannot be judged");
  return { ok: problems.length === 0, problems, kept };
}

/** NN24: a warm run after the resident gate was killed ran no row. */
export function assertRestartWarm(rows, restart) {
  const problems = [];
  if (!restart?.killed) problems.push(restart?.why ?? "the resident gate was not restarted, so the case proves nothing");
  const ran = [...rows.values()].filter((r) => r.coverage === "ran");
  for (const r of ran.slice(0, 20)) problems.push(`${r.id}: ran after the resident gate restarted`);
  return { ok: problems.length === 0, problems };
}

/** NN29: a run whose per-run identities differ gets the same keys and no miss. */
export function assertIdentityIndependent(base, other) {
  const keys = assertKeysStable(base, other);
  const problems = [...keys.problems];
  for (const r of other.values()) if (r.class === "A" && r.coverage === "ran") problems.push(`${r.id}: a reuse miss on a run that differs only in per-run identities`);
  return { ok: problems.length === 0, problems };
}

// ---- NN20 / NN21 / NN04 (compile) ----------------------------------------------------------------------------------------------------------------
/** Crate dependency map of one workspace from its manifests: `{ name: [workspace crates it names as dependencies] }`. */
export function crateDeps(manifestTexts) {
  const names = Object.keys(manifestTexts); const deps = {};
  for (const name of names) deps[name] = names.filter((o) => o !== name && new RegExp(`^\\s*${o.replace(/[-]/g, "\\-")}\\s*(=|\\.)`, "m").test(manifestTexts[name]));
  return deps;
}
/** `crate` and every crate that (transitively) depends on it. */
export function affectedCrates(crate, deps) {
  const out = new Set([crate]); let grew = true;
  while (grew) { grew = false; for (const [c, ds] of Object.entries(deps)) if (!out.has(c) && ds.some((d) => out.has(d))) { out.add(c); grew = true; } }
  return out;
}
/** NN20: after a one-crate edit no Class B row of a crate the edit cannot affect ran. */
export function assertCrateEditScope(rows, { workspace, crate, deps }) {
  const problems = [];
  const affected = affectedCrates(crate, deps);
  let b = 0;
  for (const r of rows.values()) {
    if (r.class !== "B" || r.coverage === "skipped") continue;
    b += 1;
    if (r.coverage !== "ran") continue;
    const f = cargoBFamily(r.id);
    if (!f) problems.push(`${r.id}: a Class B row that is no crate's row ran for a one-crate edit`);
    else if (f.ws !== workspace || !affected.has(f.crate)) problems.push(`${r.id}: crate ${f.crate} (${f.ws}) cannot be affected by an edit of ${crate} (affected: ${[...affected].join(", ")})`);
  }
  if (b === 0) problems.push("the compile run answered no Class B row: nothing shows what the edit recompiled");
  return { ok: problems.length === 0, problems };
}
/** Whether a row reads the lockfile of Cargo workspace `lockDir`. */
export function rowReadsLock(id, lockDir) {
  const wsRe = lockDir.replace(/[.*+?^${}()|[\]\\/]/g, "\\const PREV_WARM = Object.freeze(");
  if (new RegExp(`^(cargo-metadata|cargo-deny|cargo-audit|ghsa-audit|cargo-package-list):${wsRe}(:|$)`).test(id)) return true;
  const f = cargoBFamily(id); return Boolean(f && f.ws === lockDir);
}
/** NN21: after a lockfile edit only rows that read that lockfile (and the per-file scans that read the edited file) ran. */
export function assertLockEditScope(rows, { lockDir }) {
  const problems = [];
  for (const r of rows.values()) {
    if (r.coverage !== "ran") continue;
    if (rowReadsLock(r.id, lockDir) || /^(secret-scan|licen[cs]e)/.test(r.id)) continue;
    problems.push(`${r.id}: ran after an edit of ${lockDir}/Cargo.lock and does not read it`);
  }
  if (![...rows.values()].some((r) => r.coverage === "ran" && rowReadsLock(r.id, lockDir))) problems.push("no row that reads the edited lockfile ran: the edit was not seen");
  return { ok: problems.length === 0, problems };
}
/** NN04 on any stage: no workspace-scoped row of another workspace ran (Class A families and Class B crate rows). */
export function assertOtherWorkspacesIdleAny(rows, { workspace }) {
  const problems = []; let own = false;
  for (const r of rows.values()) {
    const m = /^(cargo-metadata|cargo-deny|cargo-audit|ghsa-audit|cargo-package-list):(.+)$/.exec(r.id); const b = cargoBFamily(r.id);
    const ws = m ? m[2].split(":")[0] : b ? b.ws : null;
    if (r.id === "cargo-metadata") { own = own || workspace === "."; if (r.coverage === "ran" && workspace !== ".") problems.push(`${r.id}: ran for an edit in ${workspace}`); continue; }
    if (ws === null) continue;
    if (ws === workspace && r.coverage !== "skipped") own = true;
    if (r.coverage === "ran" && ws !== workspace) problems.push(`${r.id}: a row of workspace ${JSON.stringify(ws)} ran for a manifest edit in ${JSON.stringify(workspace)}`);
  }
  if (!own) problems.push(`no row of workspace ${JSON.stringify(workspace)} exists in the run`);
  return { ok: problems.length === 0, problems };
}

// ---- tightened NN01 / NN02 / NN03 / NN09 / NN11 / NN13 ---------------------------------------------------------------------------------------------
const ROW_TOOL = [[/^rustfmt/, "rustfmt"], [/^shellcheck/, "shellcheck"], [/^actionlint/, "actionlint"], [/^cargo-deny/, "cargo-deny"], [/^cargo-audit/, "cargo-audit"], [/^oxlint/, "oxlint"], [/^tsc:/, "tsc"]];
/** NN01 (ledger half): a row reported `hit` never started its tool. */
export function assertHitRowsStartedNoTool(rows, entries) {
  const problems = [];
  const started = new Set(entries.map(normalizeSpawn).map((n) => n.program));
  for (const r of rows.values()) {
    if (r.coverage !== "hit") continue;
    const tool = ROW_TOOL.find(([re]) => re.test(r.id))?.[1];
    if (tool && started.has(tool)) problems.push(`${r.id}: reported hit and ${tool} started in the same run (ledger)`);
  }
  return { ok: problems.length === 0, problems };
}
/** NN03 (files-read half): rustfmt formats exactly the edited files; secret scan and licence report exactly them. */
export function assertEditedFilesRead(rows, entries, { files }) {
  const problems = [];
  const fmt = entries.map(normalizeSpawn).filter((n) => n.program === "rustfmt" && !(n.args.length === 1 && RUSTFMT_PROBE.test(n.args[0])));
  const want = [...files].sort().join("\u0000");
  for (const n of fmt) {
    const got = n.args.filter((a) => !a.startsWith("-")).map((a) => files.find((f) => a.endsWith(f)) ?? a).sort().join("\u0000");
    if (got !== want) problems.push(`rustfmt read ${shortArgs(n.args)}, expected exactly ${files.join(", ")}`);
  }
  for (const id of ["secret-scan", "license"]) {
    const r = [...rows.values()].find((x) => x.id === id || (id === "license" && /^licen[cs]e/.test(x.id)));
    if (!r) { problems.push(`no ${id} row in the run`); continue; }
    if (!Array.isArray(r.filesRead)) problems.push(`${r.id}: the coverage row has no \`filesRead\`; the gate must list the files each scan read, so reading more than the edited files cannot be ruled out`);
    else if ([...r.filesRead].sort().join("\u0000") !== want) problems.push(`${r.id}: read ${r.filesRead.length} file(s), expected exactly ${files.join(", ")}`);
  }
  return { ok: problems.length === 0, problems };
}
/** NN09 (--version half): `rustright cargo --version` also never starts. */
export function assertNoBrokerVersion(entries) {
  const problems = [];
  for (const n of entries.map(normalizeSpawn)) if (n.program === "rustright" && n.args.includes("cargo") && n.args.some((a) => VERSION_ARG.test(a))) problems.push(`rustright cargo ${n.args.filter((a) => VERSION_ARG.test(a))[0]} started: ${shortArgs(n.args)}`);
  return { ok: problems.length === 0, problems };
}
/** NN11 (agreement half): a log that says the tree was not walked is contradicted by a ledger that shows a walk. */
export function assertLedgerAgreesWithProof(log, entries) {
  const m = SNAPSHOT_PROOF.exec(String(log)); const problems = [];
  if (!m) return { ok: false, problems: ["no `snapshot proof:` line to compare the ledger with"] };
  const walks = Number(m[2]);
  const seen = entries.map(normalizeSpawn).filter((n) => TREE_WALKERS.has(n.program) || (n.program === "git" && n.args.some((a) => ["ls-files", "ls-tree", "archive"].includes(a)))).length;
  if (walks === 0 && seen > 0) problems.push(`the log says verify-walks=0 and the ledger shows ${seen} tree-walking start(s)`);
  return { ok: problems.length === 0, problems };
}
/** NN13 (probe half): a `--to check` run starts no ownership probe. */
export function assertNoOwnershipProbe(entries) {
  const problems = [];
  for (const n of entries.map(normalizeSpawn)) if (OWNERSHIP_PROBES.has(n.program)) problems.push(`--to check started an ownership probe: ${n.program} ${shortArgs(n.args)}`);
  return { ok: problems.length === 0, problems };
}

// ---- NN18: windows from launch to exit, per workload and kind --------------------------------------------------------------------------------------
export const WINDOWS = Object.freeze(["startup", "ownership", "snapshotSync", "stageStart", "snapshotVerify", "appRules", "planning", "dependencyStep", "scans", "finish"]);
export const WORKLOADS = Object.freeze(["fixture", "rightkit", "heardright", "pulse"]);
export const PROFILES = Object.freeze(["warm-check", "warm-compile", "edited-check", "edited-compile", "cold-check", "cold-compile"]);
/**
 * Consecutive windows that tile the run from the runner's launch to the gate's exit, each named by the log line that closes it:
 * startup (launch -> first stamped line), ownership (-> `build ownership held|joined`; 0 when the run takes no lease), snapshotSync (-> the sync line),
 * stageStart (-> `--to <stage> for <sha>`), snapshotVerify (-> `(verified before`), appRules (-> `app rules:`), planning (-> the plan line),
 * dependencyStep (-> `snapshot dependencies root=`), scans (-> the last `[n/N]` result line) and finish (-> exit). Optional markers that are absent
 * fold into the next window; a required marker that is absent or stamped out of order makes the run untimeable.
 */
export function windowMarks(log, startedMs, endMs) {
  const lines = String(log).split(/\r?\n/);
  const find = (re, last = false) => (last ? [...lines].reverse() : lines).find((l) => re.test(l));
  const problems = [];
  const stamp = (re, what, required, last) => { const line = find(re, last); if (!line) { if (required) problems.push(`no \`${what}\` line`); return undefined; } const t = stampOf(line); if (t === null) { problems.push(`the \`${what}\` line carries no UTC stamp`); return undefined; } return t; };
  const firstLine = lines.map(stampOf).find((t) => t !== null);
  const seq = [
    ["startup", firstLine, true],
    ["ownership", stamp(/build ownership (held|joined) /, "build ownership", false), false],
    ["snapshotSync", stamp(/snapshot \S+ is commit [0-9a-f]{40} tree [0-9a-f]{40} \(written=/, "snapshot ... (written=", true), true],
    ["stageStart", stamp(/rightkit-gate: --to \w+ for [0-9a-f]{12}/, "--to <stage> for <sha>", true), true],
    ["snapshotVerify", stamp(/snapshot \S+ is commit [0-9a-f]{40} tree [0-9a-f]{40} \(verified/, "snapshot ... (verified", false), false],
    ["appRules", stamp(/app rules: /, "app rules:", false), false],
    ["planning", stamp(/\d+ scan\(s\) for \d+ changed file\(s\) vs [0-9a-f]{12}; class A scans \d+ at a time/, "plan line", true), true],
    ["dependencyStep", stamp(/snapshot dependencies root=/, "snapshot dependencies root=", true), true],
    ["scans", stamp(/\[(\d+)\/\1\] /, "[N/N] scan result", true, true), true],
    ["finish", endMs, true],
  ];
  if (firstLine === undefined) problems.push("the log has no stamped line");
  const windows = Object.fromEntries(WINDOWS.map((w) => [w, null]));
  let prev = startedMs;
  for (const [name, t, required] of seq) {
    if (t === undefined || t === null) { if (name === "ownership") windows.ownership = 0; else if (required) windows[name] = null; continue; }
    if (t < prev) { problems.push(`${name}: closing line stamped ${prev - t} ms before the previous marker; the run cannot be tiled`); continue; }
    windows[name] = t - prev; prev = t;
  }
  return { windows, problems };
}
export const budgetKeyOf = (label) => (/^S2:/.test(label) ? "warm" : /^S11:/.test(label) ? "cold" : "edited");
/**
 * NN18 over the windows: `runs` is `[{ label, workload, profile, ms, windows, rows }]`. A missing or null budget fails ("no budget measured"); an
 * ownership window is budgeted on compile profiles only (a lease on a check is NN13's failure and must be 0 here). A cargo-metadata row that hit must
 * report `scanElapsedMs` within `metadataHitMs`.
 */
export function assertWindowBudgets({ host, budgets, runs }) {
  const problems = [];
  if (!runs.length) return { ok: false, problems: ["no run to measure"] };
  for (const r of runs) {
    const prof = budgets?.hosts?.[host]?.workloads?.[r.workload]?.[r.profile];
    const where = `${r.label} (${r.workload} ${r.profile})`;
    if (!prof) { problems.push(`${where}: no budget entry for host ${host}`); continue; }
    if (prof.totalMs === null || prof.totalMs === undefined) problems.push(`${where}: no budget measured for the whole run`);
    else if (r.ms > prof.totalMs) problems.push(`${where}: took ${r.ms} ms, budget ${prof.totalMs} ms`);
    const unbudgeted = [];
    for (const w of WINDOWS) {
      const v = r.windows?.[w];
      if (w === "ownership" && r.profile.endsWith("-check")) { if (v !== 0 && v !== null && v !== undefined) problems.push(`${where}: a check took ${v} ms of build-ownership time`); continue; }
      if (v === null || v === undefined) { problems.push(`${where}: window ${w} could not be timed`); continue; }
      const b = prof.windows?.[w];
      if (b === null || b === undefined) unbudgeted.push(w); else if (v > b) problems.push(`${where}: window ${w} took ${v} ms, budget ${b} ms`);
    }
    if (unbudgeted.length) problems.push(`${where}: no budget measured for window(s) ${unbudgeted.join(", ")}`);
    if (r.profile.startsWith("warm") && r.rows) for (const row of r.rows.values()) if (/^cargo-metadata(:|$)/.test(row.id) && row.coverage === "hit") {
      if (prof.metadataHitMs === null || prof.metadataHitMs === undefined) { problems.push(`${where}: no budget measured for a cargo-metadata hit`); break; }
      if (typeof row.scanElapsedMs !== "number") { problems.push(`${row.id}: the coverage row has no scanElapsedMs, so the metadata hit time cannot be judged`); break; }
      if (row.scanElapsedMs > prof.metadataHitMs) problems.push(`${row.id}: a metadata hit took ${row.scanElapsedMs} ms, budget ${prof.metadataHitMs} ms`);
    }
  }
  return { ok: problems.length === 0, problems };
}
/** The required (workload, profile) cells of a full run: the fixture warm/edited/cold, RightKit and HeardRight warm check, the Pulse compile. */
export const REQUIRED_CELLS = Object.freeze([["fixture", "warm-check"], ["fixture", "warm-compile"], ["fixture", "edited-check"], ["fixture", "edited-compile"], ["fixture", "cold-check"], ["fixture", "cold-compile"], ["rightkit", "warm-check"], ["heardright", "warm-check"], ["pulse", "cold-compile"]]);

// ---- NN19 / NN30 / NN31: the receipt ---------------------------------------------------------------------------------------------------------------
export const CONTRACT_RECEIPT_SCHEMA = "rightkit.contract-test-receipt/1";
export function buildContractReceipt({ host, commit, gate, contractSha256, runnerSha256, budgetsSha256, fixtureCommit, result }) {
  return { schema: CONTRACT_RECEIPT_SCHEMA, host, commit, gate, contractSha256, runnerSha256, budgetsSha256, fixtureCommit, startedAt: result.startedAt, endedAt: result.endedAt, verdict: result.verdict, ok: result.ok, nn: Object.fromEntries(result.nonNegotiables.map((n) => [n.id, n.status])), uncheckedClauses: result.clauses.filter((c) => c.status === "unchecked").map((c) => c.id), waivers: result.waivers ?? [] };
}
export const validWaiver = (w) => isObject(w) && typeof w.clause === "string" && /^GC-/.test(w.clause) && w.by === "Adrian" && typeof w.words === "string" && w.words.trim().length > 0;
/** `--verify-release`: both receipts pass, for one gate commit and one sealed gate, with the contract and runner they were judged by. */
export function verifyReleaseReceipts(a, b, { commit = null, contractSha256 = null, runnerSha256 = null, waivers = [] } = {}) {
  const problems = [];
  const hosts = [a?.host, b?.host].sort().join("+");
  if (hosts !== "dell+mac") problems.push(`the two receipts are from ${hosts || "no host"}, expected one from the mac and one from the dell`);
  for (const [name, r] of [["first", a], ["second", b]]) {
    const w = `${name} receipt (${r?.host})`;
    if (!isObject(r) || r.schema !== CONTRACT_RECEIPT_SCHEMA) { problems.push(`${w}: not a ${CONTRACT_RECEIPT_SCHEMA}`); continue; }
    if (r.ok !== true || !/^PASS /.test(String(r.verdict))) problems.push(`${w}: verdict ${JSON.stringify(r.verdict)} is not a PASS`);
    if (!isCommit(r.commit)) problems.push(`${w}: commit is not a 40-hex sha`);
    if (r.gate?.kind !== "installed" || !isSha(r.gate?.sealedSha256)) problems.push(`${w}: not the installed sealed gate (kind ${JSON.stringify(r.gate?.kind)})`);
    for (const id of Object.keys(r.nn ?? {})) if (r.nn[id] !== "pass") problems.push(`${w}: ${id} is ${r.nn[id]}`);
    if (!r.nn || Object.keys(r.nn).length < 38) problems.push(`${w}: covers ${Object.keys(r.nn ?? {}).length} non-negotiables, expected 38`);
    const open = (r.uncheckedClauses ?? []).filter((id) => !waivers.some((x) => validWaiver(x) && x.clause === id));
    if (open.length) problems.push(`${w}: ${open.length} operative clause(s) unchecked and not waived by Adrian by name (${open.slice(0, 8).join(", ")}${open.length > 8 ? ", ..." : ""})`);
  }
  if (isObject(a) && isObject(b)) {
    for (const k of ["commit", "contractSha256", "runnerSha256", "fixtureCommit"]) if (a[k] !== b[k]) problems.push(`the receipts differ in ${k}: ${a[k]} vs ${b[k]}`);
    if (a.gate?.sealedSha256 !== b.gate?.sealedSha256) problems.push("the receipts name different sealed gates");
    if (commit && a.commit !== commit) problems.push(`the receipts are for commit ${a.commit}, the release is ${commit}`);
    if (contractSha256 && a.contractSha256 !== contractSha256) problems.push("the contract changed after the receipts were written");
    if (runnerSha256 && a.runnerSha256 !== runnerSha256) problems.push("the runner changed after the receipts were written");
  }
  return { ok: problems.length === 0, problems };
}
/** NN31: release, upload and trusted-gate install tooling must call `gate-acceptance.mjs --verify-release`. Reads the tooling's own files. */
export function releaseToolingCallsVerify(files) {
  const calling = Object.entries(files).filter(([, text]) => /gate-acceptance(\.mjs)?\b[^\n]*--verify-release|--verify-release[^\n]*gate-acceptance/.test(text)).map(([f]) => f);
  return { calling };
}

// ---- NN36 / NN37: the lock and the review ----------------------------------------------------------------------------------------------------------
const numbersOf = (v, p = "") => (typeof v === "number" ? [[p, v]] : Array.isArray(v) ? v.flatMap((x, i) => numbersOf(x, `${p}[${i}]`)) : isObject(v) ? Object.entries(v).flatMap(([k, x]) => numbersOf(x, `${p}.${k}`)) : []);
/** Budgets that rose between two versions of the file (a null becoming a number is a new measurement, not a rise). A number becoming null is a change too (it removes a measured ceiling), so it needs Adrian's approval like a rise. */
export function risenBudgets(before, after) {
  const was = new Map(numbersOf(before?.hosts ?? {}));
  const is = new Map(numbersOf(after?.hosts ?? {}));
  const nullAt = (obj, p) => { try { return p.split(/\.|(?=\[)/).filter(Boolean).reduce((o, k) => o?.[k.replace(/^\[(\d+)\]$/, "$1")], obj) === null; } catch { return false; } };
  const out = [...is].filter(([p, v]) => was.has(p) && v > was.get(p)).map(([p, v]) => `${p}: ${was.get(p)} -> ${v}`);
  for (const [p, v] of was) if (!is.has(p) && nullAt(after?.hosts ?? {}, p.replace(/^\./, ""))) out.push(`${p}: ${v} -> null (a measured budget removed)`);
  return out;
}
export function lockProblems({ rootDir, paths, budgetsRel, contractRel, fixtureCommit, pin, run = (args) => git(rootDir, args) }) {
  const problems = [];
  const tableOf = (t) => parseContractNN(t).map((r) => `${r.id}|${r.property}|${r.fails}`).join("\n");
  const safe = (args) => { try { return run(args); } catch { return null; } };
  const dirty = (safe(["status", "--porcelain", "--", ...paths]) ?? "").split("\n").filter(Boolean).filter((l) => !l.includes(contractRel));
  for (const l of dirty) problems.push(`a locked file has uncommitted changes (${l.trim()}): the commit must cite Adrian's approval`);
  const headContract = safe(["show", `HEAD:${contractRel}`]); let nowContract = null; try { nowContract = fs.readFileSync(path.join(rootDir, contractRel), "utf8"); } catch { /* reported below */ }
  if (headContract !== null && nowContract !== null && tableOf(headContract) !== tableOf(nowContract)) problems.push("the contract's NN table has uncommitted changes: the commit must cite Adrian's approval");
  const base = (safe(["log", "--grep=Gate-Acceptance-Locked:", "--format=%H", "-n", "1"]) ?? "").trim();
  if (!base) problems.push("the test is not locked: no commit carries a `Gate-Acceptance-Locked: <Adrian's words>` line, so there is no baseline to judge changes against");
  else {
    const commits = (safe(["log", `${base}..HEAD`, "--format=%H", "--", ...paths]) ?? "").split("\n").filter(Boolean);
    for (const c of commits) {
      const msg = safe(["show", "-s", "--format=%B", c]) ?? "";
      const touchedContract = (safe(["diff-tree", "--no-commit-id", "--name-only", "-r", c]) ?? "").split("\n").includes(contractRel);
      const onlyContractNoTable = touchedContract && !(paths.filter((p) => p !== contractRel).some((p) => (safe(["diff-tree", "--no-commit-id", "--name-only", "-r", c]) ?? "").split("\n").includes(p))) && tableOf(safe(["show", `${c}:${contractRel}`]) ?? "") === tableOf(safe(["show", `${c}~1:${contractRel}`]) ?? "");
      if (onlyContractNoTable) continue;
      if (!/^Adrian-approved: .+/m.test(msg)) problems.push(`commit ${c.slice(0, 12)} changed a locked file without \`Adrian-approved: <his words>\` in its message`);
      let was = null; let is = null; try { was = JSON.parse(safe(["show", `${c}~1:${budgetsRel}`]) ?? "null"); is = JSON.parse(safe(["show", `${c}:${budgetsRel}`]) ?? "null"); } catch { /* not json at that commit */ }
      for (const rise of risenBudgets(was, is)) problems.push(`commit ${c.slice(0, 12)} raised a budget: ${rise}`);
    }
  }
  if (pin && fixtureCommit && pin !== fixtureCommit) problems.push(`the fixture is at ${fixtureCommit}, the pin is ${pin}`);
  if (!pin) problems.push("the budgets file has no fixture pin (`fixture.commit`)");
  return { ok: problems.length === 0, problems };
}
export function reviewProblems({ budgets, runnerSha256, contractSha256 }) {
  const reviews = Array.isArray(budgets?.reviews) ? budgets.reviews : [];
  const good = reviews.find((r) => isObject(r) && r.runnerSha256 === runnerSha256 && r.contractSha256 === contractSha256 && typeof r.reviewer === "string" && r.reviewer && typeof r.author === "string" && r.author && r.reviewer !== r.author && typeof r.adrianWords === "string" && r.adrianWords.trim() && r.verdict === "pass");
  return good ? { ok: true, problems: [] } : { ok: false, problems: [`no independent review is recorded for this runner (sha256 ${String(runnerSha256).slice(0, 12)}) and contract (${String(contractSha256).slice(0, 12)}): budgets.reviews needs { runnerSha256, contractSha256, reviewer, author (different), adrianWords, verdict: "pass" }`] };
}
/** NN38: every cross-target row built with stand-ins says `evidence: "type-check-only"` in coverage and in the receipt. */
export function assertLimitedEvidence(runs) {
  const problems = []; let cross = 0;
  for (const [label, run] of Object.entries(runs)) {
    if (!run.rows) continue;
    const rows = [...run.rows.values()].filter((r) => /@[^@:]+$/.test(r.id) && r.coverage !== "skipped");
    cross += rows.length;
    const bad = rows.filter((r) => r.evidence !== "type-check-only");
    if (bad.length) problems.push(`${label}: ${bad.length} cross-target row(s) lack evidence "type-check-only" in coverage (${bad.slice(0, 3).map((r) => r.id).join(", ")})`);
    if (rows.length && run.receipt) {
      const listed = new Set((run.receipt.stages?.compile?.limitedEvidence ?? []).filter((x) => x?.evidence === "type-check-only").map((x) => x.id));
      const missing = rows.filter((r) => !listed.has(r.id));
      if (missing.length) problems.push(`${label}: the receipt's stages.compile.limitedEvidence does not list ${missing.length} cross-target row(s) as type-check-only (the gate must write that list)`);
    }
  }
  if (cross === 0) problems.push("no cross-target row was answered in any run: nothing shows the limited-evidence marking");
  return { ok: problems.length === 0, problems };
}


/**
 * NN39 tolerance: an idle-lane-while-queued window shorter than this is start-up jitter (one lane finishing and the next queued row being picked up,
 * process spawn and the ledger's own write). Measured scheduling gaps between consecutive starts are tens of milliseconds; 250 ms is five times that.
 * A longer window is a lane that sat idle while work waited.
 */
export const NN39_TOLERANCE_MS = 250;
/** Intervals `[{ s, e }]` (ms) where fewer than `lanes` run while a later-starting interval is still queued: contiguous windows longer than `toleranceMs`. */
export function idleLaneWindows(intervals, lanes, toleranceMs = NN39_TOLERANCE_MS) {
  const iv = intervals.filter((x) => Number.isFinite(x.s) && Number.isFinite(x.e)).sort((a, b) => a.s - b.s);
  const points = [...new Set(iv.flatMap((x) => [x.s, x.e]))].sort((a, b) => a - b);
  const idle = [];
  for (let i = 0; i + 1 < points.length; i += 1) {
    const [a, b] = [points[i], points[i + 1]];
    const running = iv.filter((x) => x.s <= a && x.e > a).length;
    const queued = iv.filter((x) => x.s >= b).length;
    if (queued > 0 && running < lanes) {
      const last = idle[idle.length - 1];
      if (last && last.end === a) { last.end = b; last.minRunning = Math.min(last.minRunning, running); last.queued = Math.max(last.queued, queued); } else idle.push({ start: a, end: b, minRunning: running, queued });
    }
  }
  return idle.filter((w) => w.end - w.start > toleranceMs).map((w) => ({ ...w, ms: w.end - w.start }));
}
/** NN39: the ledger's Class A lane processes (async children that are not probes or compiles) and Class B carried compiles keep `lanes` busy while work is queued. */
export function assertRealParallelism(entries, { lanes, ranA = 0, label = "" }) {
  const problems = [];
  const ns = entries.map((e) => ({ e, n: normalizeSpawn(e) }));
  const at = ({ e }) => { const s = Date.parse(e.at); return { s, e: s + (Number(e.ms) || 0) }; };
  const A = ns.filter(({ e, n }) => e.sync === false && typeof e.ms === "number" && !["git", "ps", "lsof", "launchctl", "sysctl"].includes(n.program) && !(cargoCall(n)?.via === "rustright") && !/rustright/.test(n.program)).map(at);
  const B = ns.filter(({ n }) => { const c = cargoCall(n); return c && c.via !== "cargo" && COMPILING_CARGO.has(c.sub); }).map(at);
  if (ranA >= 2 && A.length < 2) problems.push(`${ranA} Class A rows ran and the ledger shows ${A.length} lane process(es): the lanes cannot be judged`);
  for (const [name, iv] of [["Class A lane", A], ["Class B carried compile", B]]) {
    if (iv.length < 2) continue;
    const idle = idleLaneWindows(iv, lanes);
    for (const w of idle.slice(0, 3)) problems.push(`${label}${name}: idle lane while work waited: only ${w.minRunning} of ${lanes} lanes busy for ${w.ms} ms with ${w.queued} start(s) still queued (tolerance ${NN39_TOLERANCE_MS} ms)`);
  }
  return { ok: problems.length === 0, problems };
}


/** Cargo argv without the unit selectors (`-p x`, `--package x`, `-px`, `--package=x`): the feature round and configuration of an invocation. */
const withoutUnits = (args) => { const out = []; for (let i = 0; i < args.length; i += 1) { const a = args[i]; if (a === "-p" || a === "--package") { i += 1; continue; } if (/^(-p.+|--package=.*)$/.test(a)) continue; out.push(a); } return out; };
const cargoArgsOf = (n) => { const i = n.args.indexOf("cargo"); return n.program === "rustright" ? n.args.slice(i + 1) : n.args; };
/** The rows of a coverage map that belong to the doc stage (`doc:<workspace>:<crate>`). */
const docRowsOf = (rows) => [...rows.values()].filter((r) => cargoBFamily(r.id)?.fam === "doc");
/**
 * NN40: docs are one command per workspace and feature round, on every compile. Fails on a `cargo rustdoc` start, on doc invocations of one
 * workspace and configuration (the argv without its `-p` units) running more than once, on doc rows that ran with no doc invocation in the
 * ledger, and on a compile run that answers no doc row at all (the doc stage was skipped).
 */
export function assertDocOneCommand(rows, entries, { label = "" } = {}) {
  const problems = []; const p = label ? `${label}: ` : "";
  const calls = entries.map(normalizeSpawn).map((n) => ({ n, c: cargoCall(n) })).filter(({ c }) => c && (c.sub === "doc" || c.sub === "rustdoc"));
  for (const { n, c } of calls) if (c.sub === "rustdoc") problems.push(`${p}a cargo rustdoc started (${n.program} ${shortArgs(n.args)}, cwd ${n.cwd}): docs must be one \`cargo doc\` per workspace and feature round`);
  const groups = new Map();
  for (const { n, c } of calls) { if (c.sub !== "doc") continue; const key = `${normalizeRunPart(n.cwd)}\u0000${withoutUnits(cargoArgsOf(n)).map(normalizeRunPart).join(" ")}`; (groups.get(key) ?? groups.set(key, []).get(key)).push(n); }
  for (const [key, list] of groups) if (list.length > 1) problems.push(`${p}${list.length} doc invocations for one workspace and configuration (${key.replace("\u0000", " | ")}): ${list.slice(0, 3).map((n) => shortArgs(n.args)).join(" ;; ")}`);
  const docRows = docRowsOf(rows).filter((r) => r.coverage !== "skipped");
  if (docRows.length === 0) problems.push(`${p}the compile answered no doc row (every doc row is skipped or absent): the doc stage was skipped`);
  const ranDoc = docRows.filter((r) => r.coverage === "ran");
  if (ranDoc.length && !calls.some(({ c }) => c.sub === "doc")) problems.push(`${p}${ranDoc.length} doc row(s) ran (${ranDoc.slice(0, 3).map((r) => r.id).join(", ")}) and the ledger has no \`cargo doc\` start: the doc stage is not one command`);
  return { ok: problems.length === 0, problems, docRows: docRows.length, ranDoc: ranDoc.length, invocations: calls.length };
}

/** The `--target` values of a cargo argv. */
const targetsOf = (args) => { const t = []; for (let i = 0; i < args.length; i += 1) { if (args[i] === "--target" && args[i + 1]) { t.push(args[i + 1]); i += 1; } else if (/^--target=./.test(args[i])) t.push(args[i].slice(9)); } return t; };
const withoutTargets = (args) => { const out = []; for (let i = 0; i < args.length; i += 1) { if (args[i] === "--target") { i += 1; continue; } if (/^--target=/.test(args[i])) continue; out.push(args[i]); } return out; };
export const NN42_MISSING = "installed broker refuses several --target per invocation (RK_TARGET_INVALID)";
/**
 * NN42: the cross targets of one stage go in one broker command naming every `--target`. Fails when the log carries RK_TARGET_INVALID (the broker
 * refused several), or when one stage sends one command per cross target (same unit selection and configuration, a different single target each).
 * Both name the missing broker capability. A run that built no cross-target row is no evidence either way (`crossRan` 0).
 */
export function assertCrossTargetsOneCommand(log, entries, rows, { label = "" } = {}) {
  const problems = []; const p = label ? `${label}: ` : "";
  const crossRan = rows ? [...rows.values()].filter((r) => /@[^@:]+$/.test(r.id) && r.coverage === "ran").length : 0;
  if (/RK_TARGET_INVALID/.test(String(log))) problems.push(`${p}${NN42_MISSING}: the log carries RK_TARGET_INVALID; the broker needs a capability to accept several --target in one command`);
  const groups = new Map();
  for (const n of entries.map(normalizeSpawn)) {
    const c = cargoCall(n); if (!c || c.via === "cargo" || (!COMPILING_CARGO.has(c.sub) && c.sub !== "rustdoc")) continue;
    const a = cargoArgsOf(n); const t = targetsOf(a); if (!t.length) continue;
    const key = `${normalizeRunPart(n.cwd)}\u0000${c.sub}\u0000${withoutTargets(withoutUnits(a)).map(normalizeRunPart).join(" ")}`;
    (groups.get(key) ?? groups.set(key, []).get(key)).push(t);
  }
  for (const [key, list] of groups) {
    const all = [...new Set(list.flat())];
    if (list.length > 1 && all.length > 1 && list.every((t) => t.length === 1)) problems.push(`${p}${NN42_MISSING}: the stage sent ${list.length} commands for the cross targets ${all.join(", ")} (${key.split("\u0000")[1]} in ${key.split("\u0000")[0]}), one per target; one broker command naming every --target is missing`);
  }
  return { ok: problems.length === 0, problems, crossRan, groups: groups.size };
}

/**
 * NN41: run 1 and run 2 are the same commit; every row that really failed in run 1 must be a cache hit that still says fail in run 2. Run 3 changes the
 * toolchain identity; every such row must run again (a cached failure is never reused after the identity changed, an environment fix must rerun it).
 */
export function assertCachedFailure(first, second, third, { identity = "" } = {}) {
  const problems = [];
  const failed = first ? [...first.values()].filter((r) => r.class === "B" && r.coverage === "ran" && r.status === "fail") : [];
  if (failed.length === 0) problems.push("the failing commit's first run has no Class B row that ran and failed: the fixture did not fail deterministically, so no cached failure can be judged");
  for (const f of failed) {
    const r = second?.get(f.id);
    if (!r) problems.push(`${f.id}: absent from the second run of the same commit`);
    else if (r.coverage !== "hit") problems.push(`${f.id}: failed, and with unchanged inputs, toolchain and broker identity it ${r.coverage === "ran" ? "ran again" : `is ${r.coverage}`} (a failure must be a cache hit)`);
    else if (r.status !== "fail") problems.push(`${f.id}: a cached failure came back with status ${r.status}`);
    else if (f.key && r.key && f.key !== r.key) problems.push(`${f.id}: the key changed between two identical runs`);
  }
  if (!third) problems.push(`no third run with a changed toolchain identity (${identity || "none chosen"}): the reuse after an identity change cannot be judged; the evidence needed is a second installed toolchain the gate keys on (RUSTUP_TOOLCHAIN)`);
  else for (const f of failed) {
    const r = third.get(f.id);
    if (!r) problems.push(`${f.id}: absent from the run with a changed toolchain identity`);
    else if (r.coverage === "hit") problems.push(`${f.id}: a cached failure was reused after the toolchain identity changed (${identity}): an environment fix must rerun it`);
  }
  return { ok: problems.length === 0, problems, failed: failed.map((f) => f.id) };
}

/** The NN a step-less (release-level or structural) check cannot judge from a run: names the evidence the gate or the tooling must add. */
const NN_EVAL_EXT = {
  NN20: (ctx) => { const r = usableRun(ctx, "S12:crate", { red: true }); if (r.fail) return [r.fail]; if (!ctx.crateEdit) return [bad("S12:crate", ["the edited crate's workspace and dependencies are not known"])]; return [checkFor("S12:crate", assertCrateEditScope(r.run.rows, ctx.crateEdit), r.run.coveragePaths)]; },
  NN21: (ctx) => { const r = usableRun(ctx, "S12:lock", { red: true }); if (r.fail) return [r.fail]; return [checkFor("S12:lock", assertLockEditScope(r.run.rows, { lockDir: ctx.lockDir ?? "rk" }), r.run.coveragePaths)]; },
  NN22: (ctx) => warmRuns(ctx, RED).map((w) => { if (w.fail) return w.fail; const l = ledgerOf(w.run, w.label); return l.fail ?? checkFor(w.label, assertWarmSync(w.run.log, l.entries), [w.run.logPath, w.run.ledgerPath]); }),
  NN23: (ctx) => {
    if (ctx.upgradeWhy) return [bad("S13", [ctx.upgradeWhy])];
    const a = usableRun(ctx, "S13:prev", RED); const b = usableRun(ctx, "S13:next", RED); if (a.fail || b.fail) return [a.fail ?? b.fail];
    return [checkFor("S13", assertUpgradeKeepsHits(a.run.rows, b.run.rows), [...a.run.coveragePaths, ...b.run.coveragePaths])];
  },
  NN24: (ctx) => { const r = usableRun(ctx, "S14:check", RED); if (r.fail) return [r.fail]; return [checkFor("S14:check", assertRestartWarm(r.run.rows, ctx.restart), r.run.coveragePaths)]; },
  NN25: (ctx) => warmRuns(ctx, RED).map((w) => { if (w.fail) return w.fail; const l = ledgerOf(w.run, w.label); return l.fail ?? checkFor(w.label, assertWarmNoFamilies(l.entries, { log: w.run.log }), [w.run.ledgerPath]); }),
  NN26: (ctx) => ledgerRuns(ctx).map(([label, run]) => { const l = ledgerOf(run, label); return l.fail ?? checkFor(label, assertNoServeAskSpawn(l.entries), [run.ledgerPath]); }),
  NN27: (ctx) => { const out = []; for (const label of ["S1:compile", "S11:compile"].filter((l) => ctx.runs[l])) { const r = usableRun(ctx, label, RED); out.push(r.fail ?? checkFor(label, assertNoDoublePass(r.run.rows), r.run.coveragePaths)); } return out.length ? out : [bad("S1:compile", ["no compile run"])]; },
  NN28: (ctx) => warmRuns(ctx, RED).map((w) => { if (w.fail) return w.fail; const l = ledgerOf(w.run, w.label); return l.fail ?? checkFor(w.label, assertChangeSource(w.run.log, l.entries), [w.run.logPath, w.run.ledgerPath]); }),
  NN29: (ctx) => { const a = usableRun(ctx, "S2:check2", RED); const b = usableRun(ctx, "S16:ident", RED); if (a.fail || b.fail) return [a.fail ?? b.fail]; return [checkFor("S16:ident", assertIdentityIndependent(a.run.rows, b.run.rows), [...a.run.coveragePaths, ...b.run.coveragePaths])]; },
  NN30: (ctx) => {
    const other = ctx.otherReceipt;
    if (!other) return [bad("other host", [`no passing receipt from the ${ctx.host === "mac" ? "dell" : "mac"} for this commit was given (--other-receipt <file>); a release needs both hosts`])];
    if (other.error) return [bad("other host", [other.error])];
    const own = { ...other }; const problems = [];
    if (other.host === ctx.host) problems.push(`the other receipt is from this host (${ctx.host})`);
    if (other.ok !== true) problems.push(`the ${other.host} receipt is not a pass (${other.verdict})`);
    if (other.commit !== ctx.gateCommit) problems.push(`the ${other.host} receipt is for ${other.commit}, this run is for ${ctx.gateCommit}`);
    return [{ label: `${other.host} receipt`, ok: problems.length === 0, problems, evidence: [ctx.otherReceiptPath ?? "--other-receipt"] , own }];
  },
  NN31: (ctx) => {
    const calling = ctx.tooling?.calling ?? [];
    if (calling.length) return [{ label: "tooling", ok: true, problems: [], evidence: calling }];
    return [bad("tooling", [`no release, upload or trusted-gate install tooling calls \`node packages/release/gate-acceptance.mjs --verify-release <receipt-mac> <receipt-dell>\` (searched: ${(ctx.tooling?.searched ?? []).length} file(s)). Release/upload tooling (right-release upload, build-release, the publish path) and the trusted-gate promotion (installing a candidate beside the trusted gate, testing it, then promoting) must call it and refuse on a non-zero exit`], ctx.tooling?.searched?.slice(0, 3) ?? [])];
  },
  NN32: (ctx) => {
    const kills = Object.entries(ctx.runs).filter(([, r]) => r.kill);
    if (!kills.length) return [bad("S8", ["no kill case ran"])];
    const out = kills.map(([label, r]) => checkFor(label, r.recovery ?? { ok: false, problems: ["no recovery verdict"] }, [r.logPath]));
    const have = new Set(kills.map(([, r]) => r.killSignal ?? r.signalName));
    const need = ctx.host === "dell" ? ["terminate"] : ["SIGKILL", "SIGINT"];
    const lacking = need.filter((s) => !have.has(s) && !(s === "terminate" && [...have].some((x) => x === "SIGKILL" || x === "terminate")));
    if (lacking.length) out.push(bad("signals", [`the host's kill signal(s) not exercised: ${lacking.join(", ")}`]));
    return out;
  },
  NN33: (ctx) => {
    const registryIds = (ctx.nnSet ?? NN_ALL).map((n) => n.id);
    const checkIds = [...Object.keys(NN_EVAL), ...Object.keys(NN_EVAL_EXT), ...(ctx.postIds ?? [])];
    return [{ label: "contract", ...assertContractMatch({ contractRows: parseContractNN(ctx.contractText), checkIds, registryIds }), evidence: [ctx.contractFile ?? CONTRACT_FILE] }];
  },
  NN34: () => [],
  NN35: () => [],
  NN36: (ctx) => [{ label: "lock", ...lockProblems({ rootDir: ctx.rootDir, paths: ["packages/release/gate-acceptance.mjs", "ci/gate-acceptance-budgets.json", "docs/GATE-CONTRACT.md"], budgetsRel: "ci/gate-acceptance-budgets.json", contractRel: "docs/GATE-CONTRACT.md", fixtureCommit: ctx.fixtureCommit, pin: ctx.budgets?.fixture?.commit }), evidence: ["git history of the runner, the contract table, the budgets file"] }],
  NN37: (ctx) => [{ label: "review", ...reviewProblems({ budgets: ctx.budgets, runnerSha256: ctx.runnerSha256, contractSha256: ctx.contractSha256 }), evidence: [BUDGETS_FILE] }],
  NN39: (ctx) => {
    const out = [];
    for (const label of ["S1:check", "S11:check", "S1:compile", "S11:compile"].filter((l) => ctx.runs[l])) {
      const w = usableRun(ctx, label, RED); if (w.fail) { out.push(w.fail); continue; }
      const l = ledgerOf(w.run, label); if (l.fail) { out.push(l.fail); continue; }
      const ranA = [...w.run.rows.values()].filter((r) => r.class === "A" && r.coverage === "ran").length;
      out.push(checkFor(label, assertRealParallelism(l.entries, { lanes: ctx.cores, ranA }), [w.run.ledgerPath]));
    }
    return out;
  },
  NN40: (ctx) => {
    const labels = Object.keys(ctx.runs).filter((l) => ctx.runs[l].stage === "compile" && ctx.runs[l].ladder && !ctx.runs[l].kill && /^(S1|S2|S11):compile\d?$/.test(l));
    if (!labels.length) return [bad("S2:compile", ["no compile run exists, so no doc evidence exists (a run with --checks-only has none)"])];
    return labels.map((label) => { const w = usableRun(ctx, label, RED); if (w.fail) return w.fail; const l = ledgerOf(w.run, label); return l.fail ?? checkFor(label, assertDocOneCommand(w.run.rows, l.entries, { label }), [w.run.ledgerPath, ...w.run.coveragePaths]); });
  },
  NN41: (ctx) => {
    const a = usableRun(ctx, "S17:fail1", RED); const b = usableRun(ctx, "S17:fail2", RED); if (a.fail || b.fail) return [a.fail ?? b.fail];
    const c = ctx.runs["S17:fail3"] ? usableRun(ctx, "S17:fail3", RED) : null; if (c?.fail) return [c.fail];
    return [checkFor("S17", assertCachedFailure(a.run.rows, b.run.rows, c?.run.rows ?? null, { identity: ctx.identityChange ?? "" }), [...a.run.coveragePaths, ...b.run.coveragePaths, ...(c?.run.coveragePaths ?? [])])];
  },
  NN42: (ctx) => {
    const labels = Object.keys(ctx.runs).filter((l) => ctx.runs[l].stage === "compile" && ctx.runs[l].ladder && !ctx.runs[l].kill && /^(S1|S2|S11):compile\d?$/.test(l));
    if (!labels.length) return [bad("S2:compile", ["no compile run exists, so no cross-target evidence exists"])];
    return labels.map((label) => { const w = usableRun(ctx, label, RED); if (w.fail) return w.fail; const l = ledgerOf(w.run, label); return l.fail ?? checkFor(label, assertCrossTargetsOneCommand(w.run.log, l.entries, w.run.rows, { label }), [w.run.ledgerPath, w.run.logPath]); });
  },
  NN38: (ctx) => [{ label: "limited evidence", ...assertLimitedEvidence(ctx.runs), evidence: Object.values(ctx.runs).flatMap((r) => r.coveragePaths ?? []).slice(0, 3) }],
};

const PREV_WARM = Object.freeze({ "S2:check": "S1:check", "S2:compile": "S1:compile", "S2:check2": "S2:check", "S2:compile2": "S2:compile" });
const kindOf = (label) => (/compile/.test(label) ? "compile" : "check");
const bad = (label, problems, evidence = []) => ({ label, ok: false, problems, evidence });
const checkFor = (label, res, evidence) => ({ label, ok: res.ok, problems: res.problems, evidence });

/** A run usable as warm or edit evidence: it executed, exited 0 and wrote coverage rows; otherwise a failing check names why. */
function usableRun(ctx, label, { rows = true, red = false } = {}) {
  const run = ctx.runs[label];
  if (!run) return { fail: bad(label, [`run ${label} did not execute, so its evidence does not exist`]) };
  // `red`: Class A coverage, keys, ledger and log lines of a compile that failed in Class B are still evidence of the Class A properties (NN01, 05, 06, 10, 12, 14, 15).
  if (run.exit !== 0 && !(red && run.rows)) return { fail: bad(label, [`run ${label} exited ${run.exit}${run.timedOut ? " (timed out)" : ""}: its evidence cannot stand for a passing run`], [run.logPath]) };
  if (rows && !run.rows) return { fail: bad(label, [`run ${label} wrote no readable coverage report`], [run.logPath]) };
  return { run };
}
/** The run's spawn ledger entries, or a failing check: the gate under test must emit it (RK_GATE_SPAWN_LEDGER). */
function ledgerOf(run, label) {
  if (!run.ledgerPath) return { fail: bad(label, [`run ${label} was not started with RK_GATE_SPAWN_LEDGER`]) };
  if (!run.ledger) return { fail: bad(label, [`run ${label} wrote no spawn ledger at ${run.ledgerPath}: the gate under test does not emit RK_GATE_SPAWN_LEDGER (spawn-ledger.mjs installed by the gate entry)`], [run.ledgerPath]) };
  if (run.ledger.problems.length) return { fail: bad(label, run.ledger.problems.slice(0, 5), [run.ledgerPath]) };
  if (run.ledger.entries.length === 0) return { fail: bad(label, [`the spawn ledger ${run.ledgerPath} is empty: a gate run starts child processes, so it was not recorded`], [run.ledgerPath]) };
  return { entries: run.ledger.entries };
}
const warmRuns = (ctx, opts = {}) => (ctx.warmLabels ?? WARM_LABELS).map((label) => ({ label, ...usableRun(ctx, label, opts) }));
const RED = { red: true };
const ledgerRuns = (ctx) => Object.entries(ctx.runs).filter(([, r]) => r.ledgerPath !== undefined && !r.kill);

const NN_EVAL = {
  NN01: (ctx) => warmRuns(ctx, RED).flatMap((w) => {
    if (w.fail) return [w.fail];
    const out = [checkFor(w.label, assertNoClassARan(w.run.rows), w.run.coveragePaths)];
    if (ctx.extended) { const l = ledgerOf(w.run, w.label); out.push(l.fail ?? checkFor(`${w.label} ledger`, assertHitRowsStartedNoTool(w.run.rows, l.entries), [w.run.ledgerPath])); }
    return out;
  }),
  NN02: (ctx) => ["S2:compile", "S2:compile2"].map((label) => {
    const w = usableRun(ctx, label); if (w.fail) return w.fail;
    const prev = ctx.runs["S1:compile"]?.rows ?? null;
    const r = assertNoClassBRan(w.run.rows, prev, { strict: Boolean(ctx.extended) });
    return { ...checkFor(label, r, w.run.coveragePaths), reported: r.reported };
  }),
  NN03: (ctx) => {
    const s3 = usableRun(ctx, "S3:check"); if (s3.fail) return [{ ...s3.fail, problems: [...s3.fail.problems, ctx.s3Why ?? ""].filter(Boolean) }];
    const prev = ctx.runs["S2:check2"]?.rows ?? ctx.runs["S2:check"]?.rows ?? ctx.runs["S1:check"]?.rows;
    if (!prev) return [bad("S3:check", ["no earlier check run to compare the edit against"])];
    const out = [checkFor("S3:check", assertOneFileEdit(prev, s3.run.rows), [...s3.run.coveragePaths])];
    if (ctx.extended) { const l = ledgerOf(s3.run, "S3:check"); out.push(l.fail ?? checkFor("S3:check files read", assertEditedFilesRead(s3.run.rows, l.entries, { files: [ctx.s3File] }), [s3.run.ledgerPath, ...s3.run.coveragePaths])); }
    return out;
  },
  NN04: (ctx) => {
    const s4 = usableRun(ctx, "S4:check"); if (s4.fail) return [{ ...s4.fail, problems: [...s4.fail.problems, ctx.s4Why ?? ""].filter(Boolean) }];
    if (!s4.run.manifestInfo) return [bad("S4:check", ["the edited manifest's workspace is not known"])];
    const out = [checkFor("S4:check", assertOtherWorkspacesIdle(s4.run.rows, s4.run.manifestInfo), s4.run.coveragePaths)];
    if (ctx.extended) { const c = usableRun(ctx, "S12:manifest", RED); out.push(c.fail ?? checkFor("S12:manifest (--to compile)", assertOtherWorkspacesIdleAny(c.run.rows, { workspace: s4.run.manifestInfo.workspace }), c.run.coveragePaths)); }
    return out;
  },
  NN05: (ctx) => [["S2:check", "S2:check2"], ["S2:compile", "S2:compile2"]].map(([a, b]) => {
    const x = usableRun(ctx, a, RED); const y = usableRun(ctx, b, RED);
    if (x.fail || y.fail) return x.fail ?? y.fail;
    return checkFor(`${a} vs ${b}`, assertKeysStable(x.run.rows, y.run.rows), [...x.run.coveragePaths, ...y.run.coveragePaths]);
  }),
  NN06: (ctx) => {
    const labels = ["S1:check", "S1:compile", ...WARM_LABELS, "S3:check", "S4:check"].filter((l) => ctx.runs[l]);
    const digests = new Set();
    for (const l of labels) { const d = envDigestOf(ctx.runs[l].log); if (d?.env) digests.add(d.env); if (d?.start) digests.add(d.start); }
    const out = [];
    for (const l of labels) { const w = usableRun(ctx, l, RED); if (w.fail) { out.push(w.fail); continue; } out.push(checkFor(l, assertEnvDigestScope(w.run.rows, [...digests]), w.run.coveragePaths)); }
    return out.length ? out : [bad("S1/S2", ["no check or compile run produced coverage keys"])];
  },
  NN07: (ctx) => ledgerRuns(ctx).map(([label, run]) => { const l = ledgerOf(run, label); return l.fail ?? checkFor(label, assertNoDuplicateSpawns(l.entries), [run.ledgerPath]); }),
  NN08: (ctx) => ledgerRuns(ctx).filter(([, r]) => r.exit === 0).map(([label, run]) => { const l = ledgerOf(run, label); return l.fail ?? checkFor(label, assertRustfmtSpawns(l.entries, run.rows), [run.ledgerPath, ...run.coveragePaths]); }),
  NN09: (ctx) => ledgerRuns(ctx).map(([label, run]) => { const l = ledgerOf(run, label); if (l.fail) return l.fail; const a = assertNoBrokerNonCompiling(l.entries); const v = ctx.extended ? assertNoBrokerVersion(l.entries) : { problems: [] }; return checkFor(label, { ok: a.ok && !v.problems.length, problems: [...a.problems, ...v.problems] }, [run.ledgerPath]); }),
  NN10: (ctx) => warmRuns(ctx, RED).map((w) => { if (w.fail) return w.fail; const l = ledgerOf(w.run, w.label); return l.fail ?? checkFor(w.label, assertNoRealCargoMetadata(l.entries), [w.run.ledgerPath]); }),
  NN11: (ctx) => {
    const cold = ["S1:check", "S1:compile"].filter((l) => ctx.runs[l]);
    const out = cold.map((label) => checkFor(label, assertSnapshotProof(ctx.runs[label].log, { warm: false }), [ctx.runs[label].logPath]));
    for (const w of warmRuns(ctx, RED)) {
      if (w.fail) { out.push(w.fail); continue; }
      out.push(checkFor(w.label, assertSnapshotProof(w.run.log, { warm: true }), [w.run.logPath]));
      if (ctx.extended) { const l = ledgerOf(w.run, w.label); out.push(l.fail ?? checkFor(`${w.label} ledger`, assertLedgerAgreesWithProof(w.run.log, l.entries), [w.run.logPath, w.run.ledgerPath])); }
    }
    return out;
  },
  NN12: (ctx) => {
    const out = warmRuns(ctx, RED).map((w) => { if (w.fail) return w.fail; const l = ledgerOf(w.run, w.label); return l.fail ?? checkFor(w.label, assertDependencyReuse(w.run.log, l.entries), [w.run.logPath, w.run.ledgerPath]); });
    for (const [a, b] of [["S2:check", "S2:check2"], ["S2:compile", "S2:compile2"]]) {
      const x = ctx.runs[a]; const y = ctx.runs[b];
      if (x && y) out.push(checkFor(`${a} vs ${b}`, assertDigestStable(x.log, y.log), [x.logPath, y.logPath]));
    }
    return out;
  },
  NN13: (ctx) => {
    const out = Object.entries(ctx.runs).filter(([, r]) => r.stage === "check" && r.ledgerPath !== undefined && !r.kill).flatMap(([label, r]) => {
      const a = checkFor(label, assertNoOwnershipOnCheck(r.log), [r.logPath]);
      if (!ctx.extended) return [a];
      const l = ledgerOf(r, label);
      return [a, l.fail ?? checkFor(`${label} ledger`, assertNoOwnershipProbe(l.entries), [r.ledgerPath])];
    });
    return out;
  },
  NN14: (ctx) => warmRuns(ctx, RED).map((w) => w.fail ?? checkFor(w.label, assertRulesCached(w.run.log), [w.run.logPath])),
  NN15: (ctx) => warmRuns(ctx, RED).map((w) => {
    if (w.fail) return w.fail;
    const prevRun = ctx.runs[PREV_WARM[w.label]];
    return checkFor(w.label, assertAdviceCached(prevRun?.rows ?? null, w.run.rows, { startedAt: w.run.coverageStarts?.[0], prevStartedAt: prevRun?.coverageStarts?.[0] }), w.run.coveragePaths);
  }),
  NN16: (ctx) => {
    const run = ctx.runs["S10:units"];
    if (!run) return [bad("S10:units", [ctx.unitsWhy ?? "the unit-list run did not execute"])];
    const l = ledgerOf(run, "S10:units"); if (l.fail) return [run.exit !== 0 ? bad("S10:units", [`the unit-list run exited ${run.exit}`, ...l.fail.problems], [run.logPath]) : l.fail];
    const a = assertUnitListShare(run.log, l.entries, { units: run.units?.length ?? 0 });
    return [checkFor("S10:units", { ok: a.ok && run.exit === 0, problems: [...a.problems, ...(run.exit === 0 ? [] : [`the unit-list run exited ${run.exit}`])] }, [run.logPath, run.ledgerPath])];
  },
  NN17: (ctx) => {
    const labels = Object.keys(ctx.runs).filter((l) => /^S[12]:/.test(l) && ctx.runs[l].ledgerPath !== undefined && /class A scans \d+ at a time/.test(ctx.runs[l].log));
    const out = labels.map((l) => checkFor(`lanes ${l}`, assertLanes(ctx.runs[l].log, { cores: ctx.cores, jobsOverride: ctx.jobsEnv }), [ctx.runs[l].logPath]));
    if (!out.length) out.push(bad("lanes", ["no run's log has a `class A scans N at a time` line"]));
    const compiles = ["S1:compile", "S2:compile", "S2:compile2"].filter((l) => ctx.runs[l]);
    if (!compiles.length) out.push(bad("batching", ["no compile run to read the class B batching note from"]));
    for (const l of compiles) { const run = ctx.runs[l]; const le = ledgerOf(run, l); out.push(le.fail ?? checkFor(`batching ${l}`, assertBatching(run.log, le.entries, { exit: run.exit }), [run.logPath, run.ledgerPath])); }
    return out;
  },
  NN18: (ctx) => {
    const runs = []; const out = [];
    if (ctx.budgetProblem) out.push(bad("budgets", [ctx.budgetProblem], [BUDGETS_FILE]));
    for (const w of warmRuns(ctx)) {
      if (w.fail) { out.push(w.fail); continue; }
      const m = stageMarks(w.run.log, w.run.startedMs);
      if (m.problems.length) out.push(bad(w.label, m.problems, [w.run.logPath]));
      runs.push({ label: w.label, kind: kindOf(w.label), ms: w.run.ms, marks: m.marks });
    }
    if (!ctx.budgetProblem) out.push(checkFor("budget", assertBudget({ host: ctx.host, budgets: ctx.budgets, runs }), [BUDGETS_FILE]));
    out.measured = runs;
    if (ctx.extended) out.push(...windowChecks(ctx));
    return out;
  },
  NN19: (ctx) => {
    const id = ctx.gateIdentity ?? { ok: false, problems: ["the gate's identity was not established"] };
    const dev = id.kind === "named" ? { ok: false, problems: ["development run: the gate under test is a named gate, not the newest sealed trusted gate; its results are development evidence and never count"] } : { ok: true, problems: [] };
    return [checkFor("gate", { ok: id.ok && dev.ok, problems: [...id.problems, ...dev.problems] }, [ctx.gateIdentityEvidence ?? "--version, launcher"])];
  },
};

/** NN18 over every run that has a workload profile: windows tile launch to exit; every required (workload, profile) cell must have a run. */
function windowChecks(ctx) {
  const out = []; const runs = [];
  for (const [label, run] of Object.entries(ctx.runs)) {
    if (!run.profile || run.kill) continue;
    const m = windowMarks(run.log, run.startedMs, run.startedMs + run.ms);
    if (m.problems.length) { out.push(bad(label, m.problems, [run.logPath])); continue; }
    runs.push({ label, workload: run.workload, profile: run.profile, ms: run.ms, windows: m.windows, rows: run.rows });
  }
  for (const [wl, pr] of REQUIRED_CELLS) if (!runs.some((r) => r.workload === wl && r.profile === pr) && !out.some((o) => o.label?.startsWith?.(`${wl}:${pr}`))) out.push(bad(`${wl}:${pr}`, [ctx.cellWhy?.[`${wl}:${pr}`] ?? "no run measured this workload and profile"]));
  if (runs.length) out.push(checkFor("windows", assertWindowBudgets({ host: ctx.host, budgets: ctx.budgets, runs }), [BUDGETS_FILE]));
  return out;
}

/** One result per non-negotiable: `{ id, title, steps, status: "pass"|"fail"|"not-evaluated", problems[], checks[], evidence[] }`. A selected NN with no evidence fails. */
export function evaluateNonNegotiables(ctx) {
  const wanted = new Set(ctx.wanted ?? []);
  const set = ctx.nnSet ?? NN;
  const done = []; ctx.nnSoFar = done;
  return set.map((nn) => { const res = evalOne(nn); done.push(res); return res; });
  function evalOne(nn) {
    const base = { id: nn.id, title: nn.title, steps: nn.steps, evidenceSource: nn.evidence };
    // Evaluated only when every step that produces its evidence was selected: a subset run cannot judge it, and says so (the verdict marks the run partial).
    if (ctx.forceNN ? !ctx.forceNN.includes(nn.id) : (!nn.always && !nn.steps.every((s) => wanted.has(s)))) return { ...base, status: "not-evaluated", problems: [`steps ${nn.steps.filter((x) => !wanted.has(x)).join(", ")} not selected`], checks: [], evidence: [] };
    let checks;
    if (nn.post) return { ...base, status: "pass", problems: [], checks: [], evidence: [], pending: true };
    try { checks = (NN_EVAL[nn.id] ?? NN_EVAL_EXT[nn.id])(ctx); } catch (e) { checks = [bad("assertion", [`assertion threw: ${e.stack ?? e.message}`])]; }
    const measured = checks.measured;
    if (!checks.length) checks = [bad("evidence", [`no evidence for ${nn.id}: ${nn.evidence}`])];
    const failed = checks.filter((c) => !c.ok);
    return { ...base, status: failed.length ? "fail" : "pass", problems: failed.flatMap((c) => c.problems.map((p) => (c.label ? `${c.label}: ${p}` : p))), checks: checks.map((c) => ({ label: c.label, ok: c.ok, ...(c.reported?.length ? { reported: c.reported } : {}) })), evidence: [...new Set(checks.flatMap((c) => c.evidence ?? []))], ...(measured ? { measured } : {}) };
  }
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

export function buildResult({ host, gate, broker, commit, steps, clauseChecks, startedAt, endedAt, notes = [], allowInstall = true, stepChecks = [], nonNegotiables = [], nnIds = NN_IDS, development = false, waivers = [] }) {
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
  const partial = stepsOut.length < STEPS.length;
  // Non-negotiables (GC-A27): any failure blocks; in a full run every one must be present and passing (never unchecked, never absent).
  const nnById = new Map(nonNegotiables.map((n) => [n.id, n]));
  const nnCounts = { pass: 0, fail: 0, notEvaluated: 0 };
  for (const id of nnIds) {
    const n = nnById.get(id);
    if (n?.status === "pass") nnCounts.pass += 1;
    else if (n?.status === "not-evaluated" && partial) nnCounts.notEvaluated += 1;
    else { nnCounts.fail += 1; blockers.push(n ? (n.status === "fail" ? `non-negotiable ${id} failed` : `non-negotiable ${id} was not evaluated in a full run`) : `non-negotiable ${id} has no result`); }
  }
  if (counts.pass === 0) blockers.push("no clause passed");
  // NN34: a run of some steps never prints PASS. NN19: a run of a gate that is not the newest sealed trusted gate is a development run.
  if (partial) blockers.push(`partial run (steps ${stepsOut.map((s) => s.id).join(",")} only): a partial run never passes`);
  if (development) blockers.push("development run: the gate under test is not the newest sealed trusted gate; the run prints results and never passes");
  const ok = blockers.length === 0;
  const asserted = counts.pass + counts.fail;
  const nnText = `non-negotiables ${nnCounts.pass} of ${nnIds.length} passed${nnCounts.notEvaluated ? `, ${nnCounts.notEvaluated} not evaluated (steps not selected)` : ""}${nnCounts.fail ? `, ${nnCounts.fail} failed` : ""}`;
  const tail = `${asserted} of ${clauses.length} asserted, ${counts.unchecked} unchecked; ${nnText}`;
  const verdictText = ok ? `PASS (${tail})` : `NOT PASSED (${development ? "development run; " : ""}${tail}${partial && stepsOut.length ? `; partial run: steps ${stepsOut.map((s) => s.id).join(",")} only` : ""}; ${counts.fail} failed clause(s); ${blockers.length} blocker(s))`;
  return { schema: RESULT_SCHEMA, host, gate, broker, fixtureCommit: commit, startedAt, endedAt, steps: stepsOut, stepChecks, clauses, nonNegotiables, totals: { clauses: clauses.length, ...counts, nonNegotiables: nnCounts }, ok, verdict: verdictText, blockers, partial, development, waivers, notes };
}

export function formatSummary(result) {
  const lines = [`gate acceptance ${result.verdict}  host ${result.host}  commit ${String(result.fixtureCommit).slice(0, 12)}`, `clauses: ${result.totals.pass} pass, ${result.totals.fail} fail, ${result.totals.unchecked} unchecked of ${result.totals.clauses}`];
  for (const b of result.blockers) lines.push(`  BLOCKER ${b}`);
  for (const c of result.clauses.filter((x) => x.status === "fail")) lines.push(`  FAIL ${c.id}: ${c.problems.slice(0, 2).join(" | ").slice(0, 200)}  [${c.evidence.slice(0, 2).join(", ")}]`);
  for (const n of (result.nonNegotiables ?? [])) lines.push(`  ${n.id} ${n.status}${n.status === "fail" ? `: ${n.problems.slice(0, 2).join(" | ").slice(0, 220)}` : ""}`);
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
/** Up to `n` unit selectors of the gate's grammar (crate and npm units named by a commit's tracked files), in sorted order. */
export function unitSelectorsOf(tracked, n = 2) {
  const out = [];
  for (const f of [...tracked].sort()) {
    const c = /^crates\/(rightkit-[a-z0-9-]+)\/Cargo\.toml$/.exec(f); const p = /^packages\/([a-z0-9-]+)\/package\.json$/.exec(f);
    if (c) out.push(`cargo:${c[1]}`); else if (p) out.push(`npm:@rightkit/${p[1]}`);
  }
  return out.slice(0, n);
}

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
    try { child = launch({ gate, args, cwd, env }); } catch (e) { resolve({ exit: null, signal: null, log: `spawn error: ${e.message}`, ms: 0, startedMs: t0, timedOut: false }); return; }
    const timer = setTimeout(() => { timedOut = true; signalGroup(child, "SIGKILL"); }, timeoutMs);
    child.stdout.on("data", (d) => { log += d; }); child.stderr.on("data", (d) => { log += d; });
    child.on("close", (exit, signal) => { clearTimeout(timer); resolve({ exit, signal, log: timedOut ? `${log}\ngate-acceptance: timed out after ${timeoutMs} ms; the process group was killed` : log, ms: Date.now() - t0, startedMs: t0, timedOut }); });
    child.on("error", (e) => { clearTimeout(timer); resolve({ exit: null, signal: null, log: `${log}\nspawn error: ${e.message}`, ms: Date.now() - t0, startedMs: t0, timedOut }); });
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

/** The resident gate's script for the gate under test. */
function gateServeScriptFor({ gateArg, kind, installed }) {
  if (gateArg && /\.mjs$/.test(gateArg)) return path.join(path.dirname(path.dirname(path.resolve(gateArg))), "gate-serve.mjs");
  if (kind === "installed" && installed) return path.join(path.dirname(path.dirname(installed.entry)), "gate-serve.mjs");
  return path.join(here, "gate-serve.mjs");
}

/** The crate a one-crate edit changes in workspace `ws`: one with dependencies and no dependents (a leaf), its file and the workspace's dependency map. */
export function crateEditInfo({ layout, tracked, ws = "rk" }) {
  const texts = {}; const fileOf = {};
  for (const f of layout.manifests) {
    const name = /\[package\][^[]*?\bname\s*=\s*"([^"]+)"/s.exec(layout.texts.get(f))?.[1];
    if (!name || cargoWorkspaceOf(f, layout.wsDirs) !== ws) continue;
    texts[name] = layout.texts.get(f); fileOf[name] = f;
  }
  const deps = crateDeps(texts);
  const dependents = (c) => Object.entries(deps).filter(([, ds]) => ds.includes(c)).map(([n]) => n);
  const leaf = Object.keys(deps).sort().find((c) => deps[c].length > 0 && dependents(c).length === 0);
  if (!leaf) return { why: `workspace ${ws} has no crate with a dependency and no dependents (a leaf to edit)` };
  const dir = path.posix.dirname(fileOf[leaf]);
  const file = [`${dir}/src/lib.rs`, `${dir}/src/main.rs`].find((f) => tracked.includes(f));
  return file ? { workspace: ws, crate: leaf, deps, file } : { why: `crate ${leaf} has no tracked src/lib.rs or src/main.rs` };
}

/** NN31: the text of every file that could release, upload or promote a gate. */
export function collectTooling(rootDir, managedRoot) {
  const dirs = [path.join(rootDir, "packages", "release"), path.join(rootDir, "packages", "release", "bin"), path.join(rootDir, "scripts"), path.join(rootDir, ".github", "workflows"), path.join(managedRoot, "agent-bin")];
  const files = {}; const searched = [];
  for (const d of dirs) {
    let names; try { names = fs.readdirSync(d, { withFileTypes: true }); } catch { continue; }
    for (const e of names) {
      if (!e.isFile() || /^gate-acceptance/.test(e.name) || /\.(png|jpg|zip|gz|dmg)$/.test(e.name)) continue;
      const f = path.join(d, e.name); searched.push(f);
      try { const st = fs.statSync(f); if (st.size < 2_000_000) files[f] = fs.readFileSync(f, "utf8"); } catch { /* unreadable: not searched */ }
    }
  }
  return { ...releaseToolingCallsVerify(files), searched };
}

/** Real-condition controls (--prove): assertions applied to the cold run and the real edits, where the violation really happened. Each returns true when the assertion CAUGHT it. */
export function coldControls({ cold, warmPrev, warmRows, crate, ws, coldCompileRows }) {
  const caught = {};
  if (cold?.rows) {
    caught.NN01 = !assertNoClassARan(cold.rows).ok;
    if (warmPrev) caught.NN03 = !assertOneFileEdit(warmPrev, cold.rows).ok;
    caught.NN04 = !assertOtherWorkspacesIdleAny(cold.rows, { workspace: ws ?? "rk" }).ok;
    caught.NN21 = !assertLockEditScope(cold.rows, { lockDir: ws ?? "rk" }).ok;
    caught.NN14 = !assertRulesCached(cold.log).ok;
    caught.NN15 = !assertAdviceCached(warmPrev ?? new Map(), cold.rows, {}).ok;
    caught.NN24 = !assertRestartWarm(cold.rows, { killed: true }).ok;
    if (warmRows) caught.NN29 = !assertIdentityIndependent(warmRows, cold.rows).ok;
  }
  if (coldCompileRows) {
    caught.NN02 = !assertNoClassBRan(coldCompileRows, null, { strict: true }).ok;
    if (crate) caught.NN20 = !assertCrateEditScope(coldCompileRows, crate).ok;
  }
  return caught;
}

async function main(argv) {
  const opt = (name) => { const i = argv.indexOf(name); return i < 0 ? undefined : argv[i + 1]; };
  const known = new Set(["--fixture", "--host", "--steps", "--out", "--plan", "--gate", "--commit", "--leaf-file", "--manifest-file", "--kill-after-ms", "--allow-install", "--budgets", "--help", "--prove", "--verify-release", "--other-receipt", "--contract-file", "--pulse-compile", "--prove-out", "--nn", "--checks-only", "--s11-checks-only", "--kills"]);
  for (const a of argv) if (a.startsWith("--") && !known.has(a)) { console.error(`gate-acceptance: unknown option ${a} (no cache bypass or other gate flag is ever passed)`); return 64; }
  if (argv.includes("--verify-release")) return verifyReleaseCli(argv);
  const host = opt("--host") ?? (process.platform === "win32" ? "dell" : "mac");
  if (!["mac", "dell"].includes(host)) { console.error("gate-acceptance: --host must be mac or dell"); return 64; }
  const wanted = (opt("--steps") ?? STEPS.map((s) => s.id).join(",")).split(",").map((s) => s.trim()).filter(Boolean);
  for (const s of wanted) if (!STEPS.some((x) => x.id === s)) { console.error(`gate-acceptance: unknown step ${s}`); return 64; }
  const allowInstall = argv.includes("--allow-install");
  if (allowInstall) console.error("gate-acceptance: --allow-install: --to dev and --to qualify INSTALL an app on this host. Installs need Adrian's approval; this flag is not that approval.");
  if (argv.includes("--plan")) { console.log(formatPlan(buildPlan({ steps: wanted, allowInstall }))); return 0; }
  const fixture = opt("--fixture");
  if (argv.includes("--prove")) return proveMain({ fixture, host, out: opt("--prove-out"), argv });
  if (!fixture) { console.error("usage: node gate-acceptance.mjs --fixture <dir> [--host mac|dell] [--steps S0,S1,...] [--out <dir>] [--plan] [--gate <path>] [--commit <sha>] [--allow-install]"); return 64; }
  return execute({ fixture, host, wanted, gate: opt("--gate") ?? defaultGate(host), gateArg: opt("--gate"), budgetsFile: opt("--budgets"), out: opt("--out"), commitArg: opt("--commit"), leafFile: opt("--leaf-file"), manifestFile: opt("--manifest-file"), killAfterMs: Number(opt("--kill-after-ms") ?? 3000), allowInstall, otherReceiptFile: opt("--other-receipt"), contractFile: opt("--contract-file"), pulseCompile: argv.includes("--pulse-compile"), forceNN: opt("--nn")?.split(",").filter(Boolean), checksOnly: argv.includes("--checks-only"), s11ChecksOnly: argv.includes("--s11-checks-only"), killsFilter: opt("--kills")?.split(",").filter(Boolean) });
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

async function executeRun({ fixture, host, wanted, gate, gateArg, budgetsFile, out, commitArg, leafFile, manifestFile, killAfterMs, allowInstall, otherReceiptFile, contractFile, pulseCompile, forceNN, checksOnly, s11ChecksOnly, killsFilter, g, scratch }) {
  const startedAt = new Date().toISOString();
  const platform = host === "dell" ? "win32" : "darwin";
  const managedRoot = managedRootOf(host);
  const outDir = out ?? path.join(scratch, "runs", `${host}-${startedAt.replace(/[:.]/g, "-")}`);
  fs.mkdirSync(outDir, { recursive: true });
  const budgetLoad = loadBudgets(budgetsFile ?? BUDGETS_FILE);
  const commit = commitArg ?? budgetLoad.budgets?.fixture?.commit ?? g.head();
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

  // NN19: the gate under test. `--version` of the launcher is its only output and carries no stamp.
  const launcherProbe = () => {
    const exists = fs.existsSync(gate); let version = null; let text = "";
    if (exists) {
      try { text = fs.readFileSync(gate, "utf8").slice(0, 65536); } catch { /* a binary launcher: no text to read */ }
      try { const spec = launchSpec(gate, ["--version"]); const r = spawnSync(spec.command, spec.args, { encoding: "utf8", timeout: 60_000, windowsHide: true, ...spec.options }); if (r.status === 0) version = String(r.stdout).trim().split(/\r?\n/).pop() || null; } catch { /* reported as no version */ }
    }
    return { exists, version, text };
  };
  const probe = launcherProbe();
  const installedGate = findInstalledGate(managedRoot);
  ctx.gateIdentity = gateIdentityProblems({ gateArg, defaultGate: defaultGate(host), launcherExists: probe.exists, launcherVersion: probe.version, launcherText: probe.text, installed: installedGate, checkoutRoot: workspaceRootOf() });
  ctx.gateIdentityEvidence = `${gate} --version -> ${probe.version ?? "(none)"}; newest sealed install ${installedGate ? installedGate.version : "(none)"}`;
  gateIdentity.kind = ctx.gateIdentity.kind; gateIdentity.version = probe.version; gateIdentity.installedVersion = installedGate?.version ?? null;
  Object.assign(ctx, { wanted, host, budgets: budgetLoad.budgets, budgetProblem: budgetLoad.problem, cores: typeof os.availableParallelism === "function" ? os.availableParallelism() : os.cpus().length, jobsEnv: process.env.RK_GATE_JOBS });

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

  async function doRun({ label, stage, commit: c = commit, args = [], ladder = false, step, extra = {}, receipt = true, cwd = g.canonical, env = {}, gateCmd = gate, gateArgs = [], profile, workload = "fixture" }) {
    const receiptPath = receipt && RECEIPT_STAGES.has(stage) ? path.join(outDir, `${label.replace(/[^\w.-]/g, "_")}.receipt.json`) : null;
    const full = ["--to", stage, "--commit", c, ...(receiptPath ? ["--receipt-out", receiptPath] : []), ...args];
    // Every gate invocation writes its own spawn ledger (GC-A27 NN07-NN12, NN16). RK_GATE_* never enters a cache key, so this changes no result.
    const ledgerPath = path.join(outDir, `${label.replace(/[^\w.-]/g, "_")}.ledger.jsonl`);
    fs.rmSync(ledgerPath, { force: true });
    const res = await runGate({ gate: gateCmd, cwd, args: [...gateArgs, ...full], env: { ...process.env, ...env, RK_GATE_SPAWN_LEDGER: ledgerPath } });
    return finishRun({ label, stage, commit: c, full, res, receiptPath, ladder, step, extra: { ledgerPath, workload, ...(profile ? { profile } : {}), ...(cwd !== g.canonical ? { noTree: true } : {}), ...extra } });
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
    let ledger = null;
    if (extra.ledgerPath && fs.existsSync(extra.ledgerPath)) ledger = parseLedger(fs.readFileSync(extra.ledgerPath, "utf8"));
    const run = { label, stage, commit: c, tree: extra.noTree ? null : g.tree(c), args: full, exit: res.exit, signal: res.signal, timedOut: res.timedOut === true, ms: res.ms, startedMs: res.startedMs, ledger, coverageStarts: reports.map((r) => r.startedAt), log: res.log, logPath, receipt, receiptPath, coveragePaths: cov, coverageProblems, rows: reports.length ? mergeCoverage(reports) : null, ladder, stageIndex: LADDER.indexOf(stage), timings: stageTimings(res.log), sourceIdentity, ...extra };
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
  // Two warm pairs on the same commit: NN01/NN02 judge every one of them, NN05 compares the keys of the first pair with the second.
  if (wanted.includes("S2")) for (const [label, stage] of [["S2:check", "check"], ["S2:compile", "compile"], ["S2:check2", "check"], ["S2:compile2", "compile"]].filter(([, s]) => !checksOnly || s === "check")) { const r = await doRun({ label, stage, ladder: true, step: "S2", profile: `warm-${stage}` }); check("S2", `${label}: --to ${stage} exits 0`, exitOk(r)); }

  // S3, S4 (ephemeral edits) ----------------------------------------------------------------------------------------------------------------
  const tracked = g.tracked(commit);
  const layout = (wanted.includes("S4") || wanted.includes("S7") || wanted.includes("S11") || wanted.includes("S12") || wanted.includes("S17")) ? g.cargoLayout(commit, tracked) : null;
  const editStep = async (id, name, file, edit, label, extraOf = () => ({})) => {
    if (!wanted.includes(id)) return;
    if (!file) { stepOf(id).notes.push(`unchecked: ${NEEDS_FIXTURE}`); ctx[`${id.toLowerCase()}Why`] = `${id} made no run: the fixture has no tracked ${id === "S3" ? "rk/**/src/lib.rs (or pass --leaf-file)" : "rk/**/Cargo.toml of a non-workspace-root crate (or pass --manifest-file)"}`; return; }
    const c = g.witnessCommit(commit, name, [{ path: file, content: edit(g.show(commit, file)) }]);
    const r = await doRun({ label, stage: "check", commit: c, ladder: true, step: id, extra: extraOf(file), profile: "edited-check" });
    check(id, `--to check on the edited commit exits 0`, exitOk(r));
  };
  // A fresh nonce per run: the same edit bytes as an earlier acceptance run would hit that run's cache entries, so "the edited row ran" could not be judged.
  const editNonce = `${Date.now()}-${process.pid}`;
  ctx.s3File = leafFile ?? tracked.find((f) => /^rk\/.*\/src\/lib\.rs$/.test(f));
  await editStep("S3", "s3-one-file", leafFile ?? tracked.find((f) => /^rk\/.*\/src\/lib\.rs$/.test(f)), (t) => `${t}${t.endsWith("\n") ? "" : "\n"}// gate-acceptance one-file edit ${editNonce}\n`, "S3:check");
  const manifest = manifestFile ?? (layout ? layout.manifests.find((f) => /^rk\/.*\/Cargo\.toml$/.test(f) && !layout.wsDirs.includes(path.posix.dirname(f))) : undefined);
  await editStep("S4", "s4-manifest", manifest, (t) => `${t}${t.endsWith("\n") ? "" : "\n"}\n[package.metadata.gate-acceptance]\nedit = "manifest ${editNonce}"\n`, "S4:check", (file) => {
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
  if (wanted.includes("S8")) for (const k of KILLS.filter((x) => !killsFilter || killsFilter.includes(x.id))) {
    const label = `S8:${k.id}`; const logPath = path.join(outDir, `${label.replace(/[^\w.-]/g, "_")}.log`);
    const stage = k.id.startsWith("K02") ? "check" : "compile"; const receiptPath = path.join(outDir, `${label.replace(/[^\w.-]/g, "_")}.receipt.json`);
    const killed = await killDuring({ gate, cwd: g.canonical, args: ["--to", stage, "--commit", commit, "--receipt-out", receiptPath], signal: k.signal, waitFor: readOwner, afterMs: killAfterMs });
    fs.writeFileSync(logPath, killed.log);
    const before = readOwner();
    const dry = before ? runDry(ownerFile) : { exit: null, text: "" };
    const next = await doRun({ label: `${label}:next`, stage, ladder: true, step: "S8" });
    const after = readOwner();
    // A fixture compile exits 1 for reasons that are not ownership (its Class B rows fail); a compile that reclaimed the dead record and reached planning passed the recovery.
    const reached = stage === "compile" && /class A scans \d+ at a time/.test(next.log) && !/build ownership [^\n]*(busy|refused)|RK_CHECK_ERROR/.test(next.log);
    const recovery = assertRecovery({ signal: k.signal, before, dry, next: { exit: reached ? 0 : next.exit, text: next.log }, after, exitedBySignal: exitedBySignal({ signal: killed.signal, exit: killed.exit, killSignal: k.signal, platform: process.platform }) || (k.signal === "SIGINT" && !killed.signal && killed.exit !== null && killed.exit !== 0) });
    runs[label] = { label, kill: k.id, killSignal: process.platform === "win32" ? "terminate" : k.signal, stage, recovery, logPath, ownerPath: ownerFile, exit: killed.exit, signal: killed.signal, log: killed.log, ladder: false, args: [] };
    check("S8", `${k.id} ${k.signal}: dead owner reclaimed by the next run, which passes`, recovery);
  }

  // S10 unit list (NN16) ----------------------------------------------------------------------------------------------------------------------
  // `--units-file` names units in the gate's own grammar (crates/rightkit-<x>, packages/<x>). The run shares one Class A and starts no per-unit format check.
  if (wanted.includes("S10")) {
    // The gate accepts --units-file only for the RightKit repository itself ("--units-file names RightKit units ... every other repository is built whole"), so the unit-list run is a read-only --to check of the RightKit checkout.
    const rkDir = path.join(workspaceRootOf(), "rightkit");
    let rkTracked = []; try { rkTracked = git(rkDir, ["ls-tree", "-r", "--name-only", "HEAD"]).split("\n").filter(Boolean); } catch { /* reported below */ }
    const units = unitSelectorsOf(rkTracked, 2);
    if (units.length < 2) {
      ctx.unitsWhy = `the RightKit checkout has ${units.length} unit(s) in the gate's unit grammar (crates/rightkit-<x>/Cargo.toml, packages/<x>/package.json); --units-file needs two`;
      check("S10", "the fixture names two units for --units-file", { ok: false, problems: [ctx.unitsWhy] });
    } else {
      const unitsFile = path.join(outDir, "units.json");
      fs.writeFileSync(unitsFile, `${JSON.stringify({ units })}\n`);
      const r = await doRun({ label: "S10:units", stage: "check", commit: "HEAD", cwd: rkDir, args: ["--units-file", unitsFile], ladder: false, step: "S10", extra: { units }, receipt: false });
      check("S10", "--to check --units-file exits 0", exitOk(r));
    }
  }

  // S11 cold run ----------------------------------------------------------------------------------------------------------------------------
  // The gate's scan-result cache (<git common dir>/rightkit-gate-cache, rule verdicts inside) is MOVED ASIDE under scratch (never deleted) and put back afterwards.
  ctx.cellWhy = {}; ctx.proofs = {};
  const crateInfo = layout && (wanted.includes("S11") || wanted.includes("S12") || wanted.includes("S17")) ? crateEditInfo({ layout, tracked, ws: "rk" }) : null;
  if (crateInfo && !crateInfo.why) ctx.crateEdit = { workspace: crateInfo.workspace, crate: crateInfo.crate, deps: crateInfo.deps };
  if (wanted.includes("S11")) {
    const common = path.resolve(g.canonical, g.run(["rev-parse", "--git-common-dir"]).trim());
    const cacheDir = path.join(common, "rightkit-gate-cache");
    const asideRoot = path.join(scratch, `cold-aside-${Date.now()}`);
    fs.mkdirSync(asideRoot, { recursive: true });
    const aside = path.join(asideRoot, "rightkit-gate-cache");
    let moved = false;
    try {
      if (fs.existsSync(cacheDir)) { fs.renameSync(cacheDir, aside); moved = true; }
      check("S11", `the gate cache dir ${cacheDir} was moved aside to ${aside}`, { ok: moved, problems: moved ? [] : [`${cacheDir} does not exist: the run is not proven cold`] });
      for (const stage of (s11ChecksOnly ? ["check"] : ["check", "compile"])) { const r = await doRun({ label: `S11:${stage}`, stage, ladder: true, step: "S11", profile: `cold-${stage}` }); check("S11", `cold --to ${stage} wrote coverage`, { ok: r.rows !== null, problems: [`exit ${r.exit}, no readable coverage`] }); }
    } finally {
      let restored = !moved;
      if (moved) { try { if (fs.existsSync(cacheDir)) fs.renameSync(cacheDir, path.join(asideRoot, "created-during-the-cold-run")); fs.renameSync(aside, cacheDir); restored = true; } catch (e) { check("S11", "the gate cache dir is restored", { ok: false, problems: [e.message] }); } }
      check("S11", "the original gate cache dir is back in place", { ok: restored && (!moved || fs.existsSync(cacheDir)), problems: ["the cache dir was not restored"] });
    }
    Object.assign(ctx.proofs, Object.fromEntries(Object.entries(coldControls({ cold: runs["S11:check"], warmPrev: runs["S2:check2"]?.rows ?? runs["S1:check"]?.rows ?? null, warmRows: runs["S2:check2"]?.rows ?? null, crate: ctx.crateEdit, ws: "rk", coldCompileRows: runs["S11:compile"]?.rows ?? null })).map(([k, v]) => [k, { kind: "real-condition control: the cold run (every row really ran)", caught: v }])));
  }

  // S12 compile-side edits ------------------------------------------------------------------------------------------------------------------
  if (wanted.includes("S12")) {
    const nonce2 = `${Date.now()}-${process.pid}`;
    if (!crateInfo || crateInfo.why) { ctx.cellWhy["fixture:edited-compile"] = crateInfo?.why ?? "no layout"; check("S12", "a crate to edit exists", { ok: false, problems: [crateInfo?.why ?? "no cargo layout"] }); }
    else {
      const c1 = g.witnessCommit(commit, "s12-crate", [{ path: crateInfo.file, content: `${g.show(commit, crateInfo.file)}\n// gate-acceptance crate edit ${nonce2}\n` }]);
      const r1 = await doRun({ label: "S12:crate", stage: "compile", commit: c1, ladder: true, step: "S12", profile: "edited-compile" });
      check("S12", "the crate-edit compile wrote coverage", { ok: r1.rows !== null, problems: [`exit ${r1.exit}, no readable coverage`] });
    }
    const lockFile = tracked.find((f) => f === "rk/Cargo.lock") ?? tracked.find((f) => /(^|\/)Cargo\.lock$/.test(f) && !/^app\//.test(f));
    if (!lockFile) check("S12", "a lockfile to edit exists", { ok: false, problems: ["the fixture tracks no Cargo.lock outside app/"] });
    else {
      ctx.lockDir = path.posix.dirname(lockFile);
      const c2 = g.witnessCommit(commit, "s12-lock", [{ path: lockFile, content: `${g.show(commit, lockFile)}\n# gate-acceptance lock edit ${nonce2}\n` }]);
      const r2 = await doRun({ label: "S12:lock", stage: "check", commit: c2, ladder: true, step: "S12", profile: "edited-check" });
      check("S12", "the lock-edit check wrote coverage", { ok: r2.rows !== null, problems: [`exit ${r2.exit}, no readable coverage`] });
    }
    const mf = manifestFile ?? (layout ? layout.manifests.find((f) => /^rk\/.*\/Cargo\.toml$/.test(f) && !layout.wsDirs.includes(path.posix.dirname(f))) : undefined);
    if (!mf) check("S12", "a manifest to edit exists", { ok: false, problems: [NEEDS_FIXTURE] });
    else {
      const c3 = g.witnessCommit(commit, "s12-manifest", [{ path: mf, content: `${g.show(commit, mf)}\n[package.metadata.gate-acceptance-compile]\nedit = "manifest ${nonce2}"\n` }]);
      const r3 = await doRun({ label: "S12:manifest", stage: "compile", commit: c3, ladder: true, step: "S12", profile: "edited-compile", extra: { manifestInfo: { workspace: cargoWorkspaceOf(mf, layout.wsDirs), workspaces: [...new Set([...layout.wsDirs, cargoWorkspaceOf(mf, layout.wsDirs)])], crates: layout.crates } } });
      check("S12", "the manifest-edit compile wrote coverage", { ok: r3.rows !== null, problems: [`exit ${r3.exit}, no readable coverage`] });
    }
  }

  // S13 gate upgrade ------------------------------------------------------------------------------------------------------------------------
  if (wanted.includes("S13")) {
    const gates = listInstalledGates(managedRoot);
    const older = gates.find((x) => x.version !== gates[0]?.version);
    if (!older) {
      ctx.upgradeWhy = `needs two installed versions: ${gates.length} sealed gate(s) under ${path.join(managedRoot, "trusted-gates")} (${gates.map((x) => x.version).join(", ") || "none"})`;
      check("S13", "two sealed gate versions are installed", { ok: false, problems: [ctx.upgradeWhy] });
    } else {
      for (const [label, gt] of [["S13:prev", older], ["S13:next", gates[0]]]) { const r = await doRun({ label, stage: "check", ladder: true, step: "S13", gateCmd: process.execPath, gateArgs: [gt.entry], extra: { gateVersion: gt.version } }); check("S13", `${label} (${gt.version}) wrote coverage`, { ok: r.rows !== null, problems: [`exit ${r.exit}, no readable coverage`] }); }
    }
  }

  // S14 resident restart --------------------------------------------------------------------------------------------------------------------
  if (wanted.includes("S14")) {
    const script = gateServeScriptFor({ gateArg, kind: ctx.gateIdentity.kind, installed: installedGate });
    const st = spawnSync(process.execPath, [script, "status"], { encoding: "utf8", timeout: 30_000 });
    const text = `${st.stdout ?? ""}${st.stderr ?? ""}`;
    const pid = Number(/"?\bpid"?\s*[:=]\s*(\d+)/.exec(text)?.[1]);
    ctx.restart = { killed: false, why: `the resident gate's \`status\` printed no pid (${script}): ${text.trim().slice(0, 160) || "no output"}` };
    if (Number.isInteger(pid) && pid > 1) {
      try {
        process.kill(pid, "SIGTERM");
        const until = Date.now() + 10_000;
        while (Date.now() < until) { try { process.kill(pid, 0); } catch { break; } await new Promise((r) => setTimeout(r, 100)); }
        let alive = true; try { process.kill(pid, 0); } catch { alive = false; }
        ctx.restart = alive ? { killed: false, why: `the resident gate (pid ${pid}) did not exit within 10 s of SIGTERM` } : { killed: true, pid };
      } catch (e) { ctx.restart = { killed: false, why: `cannot stop the resident gate: ${e.message}` }; }
    }
    check("S14", "the resident gate was stopped before the run", { ok: ctx.restart.killed, problems: [ctx.restart.why ?? ""] });
    const r = await doRun({ label: "S14:check", stage: "check", ladder: true, step: "S14" });
    check("S14", "the run after the restart wrote coverage", { ok: r.rows !== null, problems: [`exit ${r.exit}, no readable coverage`] });
  }

  // S15 workloads ---------------------------------------------------------------------------------------------------------------------------
  if (wanted.includes("S15")) {
    for (const [wl, dir] of [["rightkit", workspaceRootOf() + "/rightkit"], ["heardright", workspaceRootOf() + "/heardright"]]) {
      if (!fs.existsSync(dir)) { ctx.cellWhy[`${wl}:warm-check`] = `${dir} does not exist`; check("S15", `${wl} checkout exists`, { ok: false, problems: [`${dir} does not exist`] }); continue; }
      const r = await doRun({ label: `S15:${wl}`, stage: "check", commit: "HEAD", cwd: dir, receipt: false, ladder: false, step: "S15", profile: "warm-check", workload: wl });
      check("S15", `${wl} --to check exits 0`, exitOk(r));
    }
    if (pulseCompile) {
      const dir = workspaceRootOf() + "/pulse";
      const r = await doRun({ label: "S15:pulse", stage: "compile", commit: "HEAD", cwd: dir, receipt: false, ladder: false, step: "S15", profile: "cold-compile", workload: "pulse" });
      check("S15", "pulse --to compile exits 0", exitOk(r));
    } else ctx.cellWhy["pulse:cold-compile"] = "the Pulse compile did not run: it is a real compile of another repository and runs only with --pulse-compile (Adrian's approval to compile)";
  }

  // S16 per-run identities ------------------------------------------------------------------------------------------------------------------
  if (wanted.includes("S16")) {
    const tmp = fs.mkdtempSync(path.join(scratch, "ident-tmp-"));
    const r = await doRun({ label: "S16:ident", stage: "check", ladder: true, step: "S16", env: { CODEX_TOOL_CALL_ID: `fake-call-${Date.now()}`, TERM_SESSION_ID: `fake-session-${process.pid}`, TMPDIR: tmp, RK_GATE_RUN_NOTE: `identity-${Date.now()}` } });
    check("S16", "the run with different per-run identities wrote coverage", { ok: r.rows !== null, problems: [`exit ${r.exit}, no readable coverage`] });
  }

  // S17 cached failure ----------------------------------------------------------------------------------------------------------------------
  // One witness commit that adds a real compile error to one fixture crate; the same commit runs twice (the failing rows must be hits the second
  // time), then once more with RUSTUP_TOOLCHAIN naming another installed toolchain (a keyed environment input: the failing rows must run again).
  if (wanted.includes("S17")) {
    if (!crateInfo || crateInfo.why) check("S17", "a crate to break exists", { ok: false, problems: [crateInfo?.why ?? "no cargo layout"] });
    else {
      const cf = g.witnessCommit(commit, "s17-fail", [{ path: crateInfo.file, content: `${g.show(commit, crateInfo.file)}\ncompile_error!("gate-acceptance NN41: a deterministic compile failure");\n` }]);
      const f1 = await doRun({ label: "S17:fail1", stage: "compile", commit: cf, ladder: true, step: "S17", profile: "edited-compile" });
      check("S17", "the failing commit's first compile wrote coverage", { ok: f1.rows !== null, problems: [`exit ${f1.exit}, no readable coverage`] });
      const f2 = await doRun({ label: "S17:fail2", stage: "compile", commit: cf, ladder: true, step: "S17", profile: "edited-compile" });
      check("S17", "the failing commit's second compile wrote coverage", { ok: f2.rows !== null, problems: [`exit ${f2.exit}, no readable coverage`] });
      const other = otherToolchain(f1.receipt?.binding?.toolchain?.rustc);
      if (!other) { ctx.identityChange = "no second installed toolchain found under RUSTUP_HOME/toolchains"; check("S17", "a second installed toolchain exists to change the identity", { ok: false, problems: [ctx.identityChange] }); }
      else {
        ctx.identityChange = `RUSTUP_TOOLCHAIN=${other}`;
        await doRun({ label: "S17:fail3", stage: "compile", commit: cf, ladder: true, step: "S17", profile: "edited-compile", env: { RUSTUP_TOOLCHAIN: other } });
      }
    }
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
  const contractPath = contractFile ?? CONTRACT_FILE;
  let contractText = ""; try { contractText = fs.readFileSync(contractPath, "utf8"); } catch { /* NN33 reports an empty contract */ }
  const rootDir = path.resolve(here, "..", "..");
  let gateCommit = null; try { gateCommit = git(rootDir, ["rev-parse", "HEAD"]).trim(); } catch { /* NN30 reports it */ }
  let otherReceipt = null; if (otherReceiptFile) { const j = readJson(otherReceiptFile); otherReceipt = j.value ?? { error: j.error }; }
  Object.assign(ctx, { forceNN, warmLabels: checksOnly ? ["S2:check", "S2:check2"] : undefined, extended: true, nnSet: NN_ALL, contractText, contractFile: contractPath, rootDir, runnerSha256: fileSha(RUNNER_FILE), contractSha256: fileSha(contractPath), fixtureCommit: commit, gateCommit, otherReceipt, otherReceiptPath: otherReceiptFile, tooling: collectTooling(rootDir, managedRoot) });
  let nonNegotiables = evaluateNonNegotiables(ctx);
  const notes = [`out ${outDir}`, "wall time is judged only by the NN18 budgets in ci/gate-acceptance-budgets.json"];
  if (ctx.gateIdentity.kind === "named") notes.push(`the gate under test is ${gate}, named by --gate: NOT the installed sealed gate; acceptance of the installed gate is still owed`);
  if (!allowInstall) notes.push(`--to ${INSTALL_STAGES.join(" and --to ")} not run: ${NEEDS_INSTALL_APPROVAL}`);
  const development = ctx.gateIdentity.kind === "named";
  const waivers = (Array.isArray(budgetLoad.budgets?.waivers) ? budgetLoad.budgets.waivers : []).filter(validWaiver);
  const build = (nn) => buildResult({ host, gate: gateIdentity, broker: { runtimeManifest: ctx.broker.manifest ? { path: ctx.broker.manifestPath, brokerDigest: ctx.broker.manifest.brokerDigest, version: ctx.broker.manifest.version } : null, pinsBrokerDigest: ctx.broker.pins?.brokerDigest ?? null, processAtStart: ctx.s0?.broker ?? null }, commit, steps: stepRecords, clauseChecks, startedAt, endedAt: new Date().toISOString(), notes, allowInstall, stepChecks, nonNegotiables: nn, nnIds: NN_ALL_IDS, development, waivers });
  let result = build(nonNegotiables);
  // NN34 and NN35 are judged from the finished result (a first build with their placeholders, then the real verdict).
  const post = (id, problems, evidence) => { const i = nonNegotiables.findIndex((n) => n.id === id); const base = nonNegotiables[i]; nonNegotiables[i] = { ...base, pending: undefined, status: problems.length ? "fail" : "pass", problems, checks: [{ label: "result", ok: problems.length === 0 }], evidence }; };
  const noEvidence = nonNegotiables.filter((n) => n.status === "pass" && !n.pending && (n.checks.length === 0 || n.evidence.length === 0)).map((n) => `${n.id} passed with no evidence`);
  const bareClause = result.clauses.filter((c) => c.status === "pass" && (c.assertions === 0 || c.evidence.length === 0)).map((c) => `clause ${c.id} passed with no evidence`);
  post("NN34", [...noEvidence, ...bareClause, ...(result.partial && /^PASS/.test(result.verdict) ? ["a partial run printed PASS"] : [])], ["the finished result"]);
  const unwaived = result.clauses.filter((c) => c.status === "unchecked" && !waivers.some((w) => w.clause === c.id)).map((c) => c.id);
  const badWaivers = (Array.isArray(budgetLoad.budgets?.waivers) ? budgetLoad.budgets.waivers : []).filter((w) => !validWaiver(w)).map((w) => `waiver ${JSON.stringify(w).slice(0, 80)} is not by Adrian by name with his words`);
  post("NN35", [...(/\d+ of \d+ asserted/.test(result.verdict) && /non-negotiables \d+ of \d+/.test(result.verdict) ? [] : ["the verdict omits `N of M` coverage"]), ...badWaivers], [`verdict: ${result.verdict.slice(0, 120)}`, `${unwaived.length} operative clause(s) unchecked and not waived: a release needs 0`]);
  result = build(nonNegotiables);
  result.releaseEligible = result.ok && !development && unwaived.length === 0;
  result.unwaivedUnchecked = unwaived;
  result.proofs = { ...ctx.proofs, ...(development ? { NN19: { kind: "real-condition control: a checkout gate run is a development run", caught: nonNegotiables.find((n) => n.id === "NN19")?.status === "fail" && !result.ok } } : {}), ...(result.partial ? { NN34: { kind: "real-condition control: a partial run", caught: !result.ok } } : {}) };
  let receiptPath = null;
  if (result.ok && !development) {
    const entry = findInstalledGate(managedRoot);
    const receipt = buildContractReceipt({ host, commit: gateCommit, gate: { kind: "installed", version: entry?.version ?? probe.version, sealedPath: entry?.entry ?? null, sealedSha256: entry?.sealedSha256 ?? null }, contractSha256: ctx.contractSha256, runnerSha256: ctx.runnerSha256, budgetsSha256: fileSha(budgetsFile ?? BUDGETS_FILE), fixtureCommit: commit, result });
    receiptPath = path.join(outDir, `contract-test-receipt.${host}.json`); fs.writeFileSync(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`);
  }
  result.receipt = receiptPath ?? (development ? "none: a development run writes no receipt" : "none: the run did not pass");
  result.timings = Object.fromEntries(Object.entries(runs).map(([k, v]) => [k, { ms: v.ms, stages: v.timings }]));
  fs.writeFileSync(path.join(outDir, "result.json"), `${JSON.stringify(result, null, 2)}\n`);
  const summary = formatSummary(result); fs.writeFileSync(path.join(outDir, "summary.txt"), `${summary}\n`);
  console.log(summary); console.log(`result: ${path.join(outDir, "result.json")}`);
  return result.ok ? 0 : 1;
}

// ---------------------------------------------------------------------------------------------------------------------------------------------
// --verify-release and --prove
// ---------------------------------------------------------------------------------------------------------------------------------------------
/** `node gate-acceptance.mjs --verify-release <receipt-mac> <receipt-dell> [--commit <gate commit>]`: exit 0 only when both hosts' receipts pass for one gate commit (NN30, NN31, NN35). Release, upload and trusted-gate promotion tooling must call this. */
function verifyReleaseCli(argv) {
  const i = argv.indexOf("--verify-release");
  const files = [argv[i + 1], argv[i + 2]];
  if (!files[0] || !files[1] || files.some((f) => f.startsWith("--"))) { console.error("usage: node gate-acceptance.mjs --verify-release <receipt-mac> <receipt-dell> [--commit <sha>]"); return 64; }
  const read = files.map((f) => readJson(f));
  const problems = read.filter((r) => r.error).map((r) => r.error);
  const j = argv.indexOf("--commit");
  if (!problems.length) {
    const budgets = loadBudgets().budgets;
    const waivers = (Array.isArray(budgets?.waivers) ? budgets.waivers : []).filter(validWaiver);
    problems.push(...verifyReleaseReceipts(read[0].value, read[1].value, { commit: j < 0 ? null : argv[j + 1], contractSha256: fileSha(CONTRACT_FILE), runnerSha256: fileSha(RUNNER_FILE), waivers }).problems);
  }
  console.log(problems.length ? `verify-release REFUSED (${problems.length} problem(s))` : "verify-release OK: both hosts pass for one gate commit");
  for (const p of problems) console.log(`  ${p}`);
  return problems.length ? 1 : 0;
}

const GATE_INJECT_ANCHOR = "  installSpawnLedger();\n";
const spawnLine = (prog, args, opt = "{ cwd: process.cwd() }") => `__cp.spawnSync(${JSON.stringify(prog)}, ${JSON.stringify(args)}, ${opt});`;
const CARGO = ["car", "go"].join("");
/**
 * Mutant gates (--prove): a copy of the checkout's packages/release with ONE regression that reintroduces the violation of one non-negotiable. The
 * runner is run with --gate at the mutant; the NN must fail with a problem matching `expect` that the unmutated gate's run does not show.
 */
export const MUTANTS = Object.freeze([
  { nn: "NN07", steps: "S2", what: "the gate starts the same `git rev-parse --show-toplevel` twice", inject: [spawnLine("git", ["rev-parse", "--show-toplevel"]), spawnLine("git", ["rev-parse", "--show-toplevel"])], expect: /identical child process/ },
  { nn: "NN08", steps: "S2", what: "the gate runs the whole-workspace format check through cargo", inject: [spawnLine(CARGO, ["fmt", "--all", "--check"])], expect: /cargo fmt started/ },
  { nn: "NN09", steps: "S2", what: "the gate asks the broker for a metadata read (`rustright cargo metadata`)", inject: [spawnLine("rustright", [CARGO, "metadata", "--no-deps"])], expect: /went through the broker/ },
  { nn: "NN10", steps: "S2", what: "the gate starts a real metadata read on a warm run", inject: [spawnLine(CARGO, ["metadata", "--no-deps", "--format-version", "1"])], expect: /real cargo metadata/ },
  { nn: "NN13", steps: "S2", what: "a --to check starts an ownership probe (`ps`)", inject: [spawnLine("ps", ["-p", "1"])], expect: /ownership probe/ },
  { nn: "NN16", steps: "S10", what: "a unit-list run starts the per-unit format check through cargo", inject: [spawnLine(CARGO, ["fmt", "-p", "rk-core", "--check"])], expect: /cargo fmt started for a unit list/ },
  { nn: "NN22", steps: "S2", what: "a warm run walks the tree with `find`", inject: [spawnLine("find", [".", "-maxdepth", "1"])], expect: /tree walk/ },
  { nn: "NN25", steps: "S2", what: "a warm run starts `pnpm --version`", inject: [spawnLine("pnpm", ["--version"])], expect: /pnpm|version probe/ },
  { nn: "NN26", steps: "S2", what: "the gate starts `node gate-serve.mjs ask` to ask the resident gate", inject: ['__cp.spawnSync(process.execPath, [process.getBuiltinModule("node:url").fileURLToPath(new URL("../gate-serve.mjs", import.meta.url)), "ask"], { timeout: 2000 });'], expect: /ask the resident gate/ },
  { nn: "NN39", steps: "S11", extra: ["--s11-checks-only"], what: "the Class A lane count is forced to 1 in the code path (not an environment override)", patch: { find: "const jobs = Math.max(1, Number(o.env.RK_GATE_JOBS ?? defaultJobs) || defaultJobs);", replace: "const jobs = 1;" }, expect: /idle lane while work waited/ },
  { nn: "NN17", steps: "S2", what: "the Class A lane count is forced to 1 in the code path", patch: { find: "const jobs = Math.max(1, Number(o.env.RK_GATE_JOBS ?? defaultJobs) || defaultJobs);", replace: "const jobs = 1;" }, expect: /lane\(s\) on a/ },
  { nn: "NN05", steps: "S2", what: "a per-run value (a high-resolution clock reading) enters every Class A row key", patch: { find: "${rowShape(scan)}`; scanKeys.set(scan.id, full);", replace: "${rowShape(scan)}:${process.hrtime.bigint()}`; scanKeys.set(scan.id, full);" }, expect: /key differs between identical runs/ },
  { nn: "NN06", steps: "S2", what: "every Class A row is environment-bound, so non-executing keys carry the environment digest", patch: { find: "function classAEnvironmentBound(scan) { return scan.executes === true || scan.tool === \"pnpm\"; }", replace: "function classAEnvironmentBound(scan) { return true; }" }, expect: /carries the environment digest/ },
  { nn: "NN12", steps: "S2", what: "a warm run starts `pnpm install`", inject: [spawnLine("pnpm", ["install", "--offline"])], expect: /pnpm install started/ },
  { nn: "NN29", steps: "S2,S16", what: "the tool-call id of the environment enters every Class A row key", patch: { find: "${rowShape(scan)}`; scanKeys.set(scan.id, full);", replace: "${rowShape(scan)}:${process.env.CODEX_TOOL_CALL_ID ?? \"\"}`; scanKeys.set(scan.id, full);" }, expect: /key differs between identical runs|reuse miss/ },
  { nn: "NN32", steps: "S8", extra: ["--kills", "K02,K02-int"], what: "a dead owner's record is never judged reclaimable (judgeOwner always refuses)", patch: { file: "build-ownership.mjs", find: "if (owner.host !== host) throw busy(`held by pid ${owner.pid} on ${owner.host}`);", replace: "if (true) throw busy(`held by pid ${owner.pid} (MUTANT: a dead owner is never judged reclaimable)`);" }, expect: /reclaimed from|exited/ },
  { nn: "NN40", steps: "S2", compile: true, control: true, what: "the gate starts per-crate `cargo rustdoc` again (two crates of the fixture workspace, two starts)", inject: [spawnLine("rustright", ["cargo", "rustdoc", "-p", "rk-core", "--lib", "--help"]), spawnLine("rustright", ["cargo", "rustdoc", "-p", "rk-util", "--lib", "--help"])], expect: /a cargo rustdoc started/ },
  { nn: "NN41", steps: "S17", control: true, what: "the gate refuses to reuse a cached failure (every failing cache entry is dropped before the run)", inject: ['try { const __fs = process.getBuiltinModule("node:fs"); const __p = process.getBuiltinModule("node:path"); const __d = __p.join(process.cwd(), ".git", "rightkit-gate-cache"); for (const __f of __fs.readdirSync(__d)) { const __x = __p.join(__d, __f); try { if (JSON.parse(__fs.readFileSync(__x, "utf8"))?.row?.status === "fail") __fs.rmSync(__x); } catch { /* not an entry */ } } } catch { /* no cache dir */ }'], expect: /ran again \(a failure must be a cache hit\)/ },
  { nn: "NN42", steps: "S2", compile: true, control: true, what: "the stage sends one broker command per cross target (two single-target commands of one configuration)", inject: [spawnLine("rustright", ["cargo", "check", "--help", "--target", "aarch64-unknown-linux-gnu"]), spawnLine("rustright", ["cargo", "check", "--help", "--target", "x86_64-pc-windows-msvc"])], expect: /one per target/ },
  { nn: "NN28", steps: "S2", what: "the gate polls for changes with `git status`", inject: [spawnLine("git", ["status", "--short"])], expect: /polling/ },
]);

function copyRelease(dest) {
  const src = here;
  fs.mkdirSync(dest, { recursive: true });
  const a = spawnSync("rsync", ["-a", "--exclude", "node_modules", `${src}/`, `${dest}/`], { encoding: "utf8" });
  if (a.status !== 0) throw new Error(`rsync of ${src}: ${a.stderr}`);
  const nm = path.join(src, "node_modules");
  if (fs.existsSync(nm) && !fs.existsSync(path.join(dest, "node_modules"))) { const c = spawnSync("cp", ["-cRL", nm, path.join(dest, "node_modules")], { encoding: "utf8" }); if (c.status !== 0) throw new Error(`cp node_modules: ${c.stderr}`); }
}
/** Writes the mutant copy and returns `{ dir, entry, diff }`; the checkout is never touched. */
export function makeMutant(root, m) {
  const dir = path.join(root, m.nn ?? "baseline"); const rel = path.join(dir, "release");
  copyRelease(rel);
  const entry = path.join(rel, "bin", "rightkit-gate.mjs");
  let diff = "(unmutated copy)";
  if (m.patch) {
    const target = path.join(rel, m.patch.file ?? path.join("bin", "rightkit-gate.mjs"));
    const text = fs.readFileSync(target, "utf8");
    if (text.split(m.patch.find).length !== 2) throw new Error(`mutant ${m.nn}: the patch anchor is not unique in ${path.basename(target)}`);
    fs.writeFileSync(target, text.replace(m.patch.find, () => m.patch.replace));
    diff = `--- ${m.patch.find}\n+++ ${m.patch.replace}`;
  }
  if (m.inject) {
    const text = fs.readFileSync(entry, "utf8");
    if (text.split(GATE_INJECT_ANCHOR).length !== 2) throw new Error(`mutant ${m.nn}: the anchor \`installSpawnLedger();\` is not unique in the gate`);
    const add = `  const __cp = process.getBuiltinModule("node:child_process"); // MUTANT ${m.nn}: ${m.what}\n${m.inject.map((l) => `  ${l}`).join("\n")}\n`;
    fs.writeFileSync(entry, text.replace(GATE_INJECT_ANCHOR, () => GATE_INJECT_ANCHOR + add));
    diff = `+++ bin/rightkit-gate.mjs (after installSpawnLedger();)\n${add.replace(/^/gm, "+")}`;
  }
  fs.chmodSync(entry, 0o755);
  fs.writeFileSync(path.join(dir, "mutation.diff"), `${diff}\n`);
  return { dir, entry, diff };
}

/** An installed rustup toolchain other than the one the gate recorded (`rustc 1.96.1 (...)`): the newest remaining one, or null. */
function otherToolchain(recordedRustc) {
  const home = process.env.RUSTUP_HOME || path.join(os.homedir(), ".rustup");
  let names = []; try { names = fs.readdirSync(path.join(home, "toolchains")); } catch { return null; }
  const rec = /rustc (\d+\.\d+\.\d+)/.exec(String(recordedRustc ?? ""))?.[1] ?? null;
  const ver = (n) => (/^(\d+)\.(\d+)\.(\d+)/.exec(n) ?? []).slice(1).map(Number);
  const cmp = (a, b) => { for (let i = 0; i < 3; i += 1) if ((a[i] ?? 0) !== (b[i] ?? 0)) return (b[i] ?? 0) - (a[i] ?? 0); return 0; };
  const cands = names.filter((n) => /^\d+\.\d+\.\d+/.test(n) && (!rec || !n.startsWith(`${rec}-`))).sort((a, b) => cmp(ver(a), ver(b)));
  return cands[0] ?? null;
}
const spawnRunner = (file, args) => runRunner(args, { cwd: path.dirname(file), file });
function runRunner(args, { cwd, file = RUNNER_FILE }) {
  return new Promise((resolve) => {
    let out = "";
    const child = spawn(process.execPath, [file, ...args], { cwd, stdio: ["ignore", "pipe", "pipe"] });
    child.stdout.on("data", (d) => { out += d; }); child.stderr.on("data", (d) => { out += d; });
    child.on("close", (code) => resolve({ code, out }));
    child.on("error", (e) => resolve({ code: null, out: `${out}\n${e.message}` }));
  });
}

/** `--prove`: every NN is shown to be able to fail, by a real-condition control, a mutant gate, or a real altered contract. Fails unless every NN is caught. */
async function proveMain({ fixture, host, out }) {
  if (!fixture) { console.error("usage: node gate-acceptance.mjs --prove --fixture <dir> [--host mac|dell] [--prove-out <dir>]"); return 64; }
  const scratch = path.join(scratchRootOf(host), "gate-acceptance");
  const root = out ?? path.join(scratch, "prove", new Date().toISOString().replace(/[:.]/g, "-"));
  const mutRoot = path.join(scratch, "mutants");
  fs.mkdirSync(root, { recursive: true }); fs.mkdirSync(mutRoot, { recursive: true });
  const proofs = {}; const log = []; const controls = [];
  const note = (id, how, caught, detail) => { (proofs[id] ??= []).push({ how, caught, detail }); log.push(`${id} ${caught ? "CAUGHT" : "not caught"} by ${how}${detail ? `: ${detail}` : ""}`); };
  const resultOf = (dir) => { try { return JSON.parse(fs.readFileSync(path.join(dir, "result.json"), "utf8")); } catch { return null; } };
  const common = ["--fixture", fixture, "--host", host];

  // NN33: the real runner against a real altered contract (a row removed; a row added).
  const real = fs.readFileSync(CONTRACT_FILE, "utf8");
  const variants = { "a row removed": real.split("\n").filter((l) => !/^\| NN20 \|/.test(l)).join("\n"), "a row added": real.replace(/^(\| NN38 \|.*)$/m, "$1\n| NN99 | An invented property | never |") };
  for (const [name, text] of Object.entries(variants)) {
    const f = path.join(root, `contract-${name.replace(/\s/g, "-")}.md`); fs.writeFileSync(f, text);
    const o = path.join(root, `nn33-${name.replace(/\s/g, "-")}`);
    await runRunner([...common, "--steps", "S0", "--nn", "NN33", "--contract-file", f, "--out", o], { cwd: here });
    const r = resultOf(o)?.nonNegotiables?.find((n) => n.id === "NN33");
    note("NN33", `the runner against a contract with ${name}`, r?.status === "fail" && /NN(20|99)/.test(r.problems.join()), r?.problems?.[0]);
  }
  // Controls: the real runner against an unmutated copy of the checkout gate, with the cache moved aside (S11) after warm runs (S2).
  const base = makeMutant(mutRoot, { nn: "baseline" });
  const co = path.join(root, "controls");
  const cr = await runRunner([...common, "--steps", "S2,S11", "--checks-only", "--nn", "NN19,NN11,NN27,NN38", "--gate", base.entry, "--out", co], { cwd: here });
  const cres = resultOf(co);
  if (!cres) log.push(`controls run produced no result (exit ${cr.code})`);
  for (const [id, p] of Object.entries(cres?.proofs ?? {})) note(id, p.kind, p.caught === true, p.caught ? "the assertion flagged the real violation" : "the assertion did NOT flag it");
  // Mutants.
  for (const m of MUTANTS) {
    let mm; try { mm = makeMutant(mutRoot, m); } catch (e) { note(m.nn, `mutant gate (${m.what})`, false, e.message); continue; }
    const o = path.join(root, `mutant-${m.nn}`);
    const args = [...common, "--steps", m.steps, "--nn", m.nn, "--gate", mm.entry, "--out", o]; if (m.steps === "S2" && !m.compile) args.push("--checks-only");
    if (m.extra) args.push(...m.extra);
    await runRunner(args, { cwd: here });
    const r = resultOf(o)?.nonNegotiables?.find((n) => n.id === m.nn);
    const hit = r?.status === "fail" && r.problems.some((p) => m.expect.test(p));
    if (m.control) {
      // The unmutated control: the same run against the unmutated copy of the gate must NOT show the mutant's failure.
      const oc = path.join(root, `control-${m.nn}`);
      await runRunner([...args.slice(0, args.indexOf("--gate")), "--gate", base.entry, "--out", oc, ...(m.extra ?? [])], { cwd: here });
      const cr2 = resultOf(oc)?.nonNegotiables?.find((n) => n.id === m.nn);
      const shown = cr2?.problems?.find((x) => m.expect.test(x));
      controls.push({ id: m.nn, ok: Boolean(cr2) && !shown, detail: !cr2 ? "no result" : shown ? `the unmutated gate SHOWS the failure: ${shown.slice(0, 200)}` : `the unmutated gate does not show it (verdict ${cr2.status}${cr2.status === "fail" ? `: ${cr2.problems[0]?.slice(0, 160)}` : ""})`, out: oc });
      log.push(`${m.nn} control (unmutated gate, same run): ${controls[controls.length - 1].ok ? "does not show the failure" : "SHOWS the failure"}: ${controls[controls.length - 1].detail}`);
    }
    note(m.nn, `mutant gate: ${m.what} (${mm.dir}/mutation.diff)`, hit, hit ? r.problems.find((p) => m.expect.test(p)).slice(0, 140) : r ? `verdict ${r.status}; ${r.problems.slice(0, 2).join(" | ").slice(0, 200)}` : "no result");
  }
  // Real gate evidence from the controls run (the unmutated checkout gate): a check that fails there on a violation or on missing evidence is shown to be able to fail.
  for (const id of ["NN11", "NN27", "NN38"]) {
    const r = cres?.nonNegotiables?.find((n) => n.id === id);
    const missing = id === "NN11";
    note(id, missing ? "the real gate lacks the evidence the check needs (missing evidence fails; weaker than a violation)" : "a real violation by the unmutated checkout gate", r?.status === "fail", r?.problems?.[0]?.slice(0, 200));
  }
  // NN18: the real runner against a real budgets file with a 1 ms budget.
  {
    const tight = JSON.parse(fs.readFileSync(BUDGETS_FILE, "utf8"));
    tight.hosts.mac.warmCheckMs = 1; tight.hosts.mac.stages.dependencyStep = 1;
    const f = path.join(root, "budgets-1ms.json"); fs.writeFileSync(f, `${JSON.stringify(tight, null, 2)}\n`);
    const o = path.join(root, "nn18-tight");
    await runRunner([...common, "--steps", "S2", "--checks-only", "--nn", "NN18", "--budgets", f, "--gate", base.entry, "--out", o], { cwd: here });
    const r = resultOf(o)?.nonNegotiables?.find((n) => n.id === "NN18");
    const hit = r?.status === "fail" && r.problems.find((p) => /took \d+ ms, budget 1 ms/.test(p));
    note("NN18", "the real runner against a budgets file with a 1 ms budget", Boolean(hit), hit ? hit.slice(0, 200) : r?.problems?.[0]);
  }
  // NN23: needs two distinct sealed versions; without them the check's own failure message is the proof.
  {
    const gates = listInstalledGates(managedRootOf(host));
    const distinct = new Set(gates.map((x) => x.version)).size;
    const o = path.join(root, "nn23");
    await runRunner([...common, "--steps", "S13", "--nn", "NN23", "--out", o], { cwd: here });
    const r = resultOf(o)?.nonNegotiables?.find((n) => n.id === "NN23");
    if (distinct < 2) note("NN23", `the failure message itself: ${distinct} distinct sealed version(s) installed, so a real upgrade cannot be run (weaker than a violation)`, r?.status === "fail" && /needs two installed versions/.test(r.problems.join()), r?.problems?.[0]?.slice(0, 200));
    else note("NN23", "a real upgrade between the two newest sealed gates", r?.status === "fail", r?.problems?.[0]?.slice(0, 200) ?? `the check passed on a real upgrade: it can fail only when keys change, so it is unproven`);
  }
  // NN30, NN31, NN35: real --verify-release refusals on receipts built from the real run's result.
  if (cres) {
    const inst = listInstalledGates(managedRootOf(host))[0] ?? null;
    const budgets = loadBudgets().budgets;
    const mk = (h, over = {}) => ({ ...buildContractReceipt({ host: h, commit: "a".repeat(40), gate: { kind: "installed", version: inst?.version ?? "0", sealedPath: inst?.entry ?? null, sealedSha256: inst?.sealedSha256 ?? "b".repeat(64) }, contractSha256: fileSha(CONTRACT_FILE), runnerSha256: fileSha(RUNNER_FILE), budgetsSha256: fileSha(BUDGETS_FILE), fixtureCommit: budgets?.fixture?.commit ?? "c".repeat(40), result: cres }), ...over });
    const w = (name, r) => { const f = path.join(root, `receipt-${name}.json`); fs.writeFileSync(f, `${JSON.stringify(r, null, 2)}\n`); return f; };
    const verify = async (a, b) => runRunner(["--verify-release", a, b], { cwd: here });
    const real1 = await verify(w("mac-real", mk("mac")), path.join(root, "receipt-dell-missing.json"));
    note("NN30", "--verify-release with the Dell receipt missing", real1.code === 1 && /ENOENT|cannot read/.test(real1.out), real1.out.split("\n").slice(0, 2).join(" | ").slice(0, 200));
    const real2 = await verify(w("mac-fail", mk("mac")), w("dell-fail", mk("dell")));
    note("NN30", "--verify-release with a failing receipt from each host (the real run's NOT PASSED result)", real2.code === 1 && /not a PASS/.test(real2.out), (real2.out.split("\n")[1] ?? "").trim().slice(0, 200));
    note("NN31", "--verify-release refuses a release whose receipts are not passes (what release, upload and promotion must call)", real2.code === 1 && /REFUSED/.test(real2.out), (real2.out.split("\n")[0] ?? "").slice(0, 200));
    const allPass = Object.fromEntries(NN_ALL_IDS.map((id) => [id, "pass"]));
    const unw = await verify(w("mac-unchecked", mk("mac", { ok: true, verdict: "PASS (forced for the proof)", nn: allPass })), w("dell-unchecked", mk("dell", { ok: true, verdict: "PASS (forced for the proof)", nn: allPass })));
    note("NN35", "--verify-release with passing receipts that leave operative clauses unchecked and unwaived", unw.code === 1 && /unchecked and not waived by Adrian/.test(unw.out), (unw.out.split("\n")[1] ?? "").trim().slice(0, 200));
  }
  // NN36 and NN37: real commits in a scratch clone of rightkit.
  {
    const lp = path.join(scratch, "lockproof"); const clone = path.join(lp, `clone-${Date.now()}`);
    fs.mkdirSync(lp, { recursive: true });
    const sh = (args, cwd = clone) => { const r = spawnSync("git", args, { cwd, encoding: "utf8" }); if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr || r.stdout}`); return r.stdout; };
    try {
      sh(["clone", "--quiet", path.resolve(here, "..", ".."), clone], lp);
      const cp = spawnSync("cp", ["-cRL", path.join(here, "node_modules"), path.join(clone, "packages", "release", "node_modules")], { encoding: "utf8" });
      if (cp.status !== 0) throw new Error(`cp node_modules: ${cp.stderr}`);
      const cRunner = path.join(clone, "packages", "release", "gate-acceptance.mjs");
      const cBudgets = path.join(clone, "ci", "gate-acceptance-budgets.json");
      const judge = async (name) => { const o = path.join(root, `lock-${name}`); await spawnRunner(cRunner, [...common, "--steps", "S0", "--nn", "NN36,NN37", "--out", o]); return resultOf(o)?.nonNegotiables ?? []; };
      sh(["commit", "--allow-empty", "-q", "-m", "lock the contract test\n\nGate-Acceptance-Locked: lockproof baseline"]);
      const base36 = (await judge("1-locked")).find((n) => n.id === "NN36");
      log.push(`NN36 control (not a proof): locked baseline, nothing changed since: ${base36?.status} ${base36?.problems?.[0]?.slice(0, 160) ?? ""}`);
      fs.appendFileSync(cRunner, "\n// lockproof: a change to the runner with no approval line\n");
      sh(["commit", "-q", "-am", "runner: an unapproved change"]);
      const u36 = (await judge("2-unapproved")).find((n) => n.id === "NN36");
      note("NN36", "scratch clone: a real commit changes the runner without the approval line", u36?.status === "fail" && /without `Adrian-approved:/.test(u36.problems.join()), u36?.problems?.find((p) => /Adrian-approved/.test(p))?.slice(0, 200) ?? u36?.problems?.[0]);
      const b = JSON.parse(fs.readFileSync(cBudgets, "utf8")); b.hosts.mac.warmCheckMs = (b.hosts.mac.warmCheckMs ?? 1000) * 2;
      fs.writeFileSync(cBudgets, `${JSON.stringify(b, null, 2)}\n`);
      sh(["commit", "-q", "-am", "budgets: raise warmCheckMs\n\nAdrian-approved: lockproof (the approval line is present; the rise is the violation)"]);
      const r36 = (await judge("3-raised")).find((n) => n.id === "NN36");
      note("NN36", "scratch clone: a commit that cites approval but raises a budget", r36?.status === "fail" && /raised a budget/.test(r36.problems.join()), r36?.problems?.find((p) => /raised a budget/.test(p))?.slice(0, 200) ?? r36?.problems?.[0]);
      const n37a = (await judge("4-noreview")).find((n) => n.id === "NN37");
      note("NN37", "scratch clone: no independent review recorded", n37a?.status === "fail" && /no independent review/.test(n37a.problems.join()), n37a?.problems?.[0]?.slice(0, 200));
      const rs = fileSha(cRunner); const cs = fileSha(path.join(clone, "docs", "GATE-CONTRACT.md"));
      const b2 = JSON.parse(fs.readFileSync(cBudgets, "utf8")); b2.reviews = [{ runnerSha256: rs, contractSha256: cs, reviewer: "same", author: "same", adrianWords: "lockproof", verdict: "pass" }];
      fs.writeFileSync(cBudgets, `${JSON.stringify(b2, null, 2)}\n`); sh(["commit", "-q", "-am", "budgets: self review\n\nAdrian-approved: lockproof"]);
      const n37b = (await judge("5-selfreview")).find((n) => n.id === "NN37");
      note("NN37", "scratch clone: the reviewer is the author", n37b?.status === "fail" && /no independent review/.test(n37b.problems.join()), n37b?.problems?.[0]?.slice(0, 200));
      b2.reviews = [{ runnerSha256: rs, contractSha256: cs, reviewer: "other", author: "same", adrianWords: "lockproof", verdict: "pass" }];
      fs.writeFileSync(cBudgets, `${JSON.stringify(b2, null, 2)}\n`); sh(["commit", "-q", "-am", "budgets: independent review\n\nAdrian-approved: lockproof"]);
      const n37c = (await judge("6-review")).find((n) => n.id === "NN37");
      log.push(`NN37 control: with an independent review recorded the check is ${n37c?.status}`);
    } catch (e) { log.push(`lockproof failed: ${e.message}`); note("NN36", "scratch clone", false, e.message.slice(0, 200)); note("NN37", "scratch clone", false, e.message.slice(0, 200)); }
  }
  // NN32: the unmutated gate recovers real kills (information), after the mutant above left a stale record: reclaimed by this run.
  {
    const o = path.join(root, "kills-real");
    await runRunner([...common, "--steps", "S8", "--nn", "NN32", "--gate", base.entry, "--kills", "K01,K01-int,K02,K02-int", "--out", o], { cwd: here });
    const r = resultOf(o)?.nonNegotiables?.find((n) => n.id === "NN32");
    log.push(`NN32 real kills on the unmutated gate (SIGKILL compile, SIGINT compile, SIGKILL check, SIGINT check): ${r?.status} ${r?.problems?.[0]?.slice(0, 200) ?? ""}`);
  }
  const rows = NN_ALL_IDS.map((id) => ({ id, proven: (proofs[id] ?? []).some((p) => p.caught), proofs: proofs[id] ?? [] }));
  const unproven = rows.filter((r) => !r.proven).map((r) => r.id);
  const badControls = controls.filter((c) => !c.ok);
  const summary = { schema: "rightkit.gate-acceptance-prove/1", host, ok: unproven.length === 0 && badControls.length === 0, unproven, controls, rows, log };
  fs.writeFileSync(path.join(root, "prove-result.json"), `${JSON.stringify(summary, null, 2)}\n`);
  console.log(`prove ${summary.ok ? "OK" : "FAILED"}: ${rows.length - unproven.length} of ${rows.length} non-negotiables have a proof that they can fail; ${controls.length - badControls.length} of ${controls.length} unmutated controls are clean`);
  for (const c of controls) console.log(`  control ${c.id} ${c.ok ? "clean" : "SHOWS THE FAILURE"}: ${c.detail}`);
  for (const r of rows) console.log(`  ${r.id} ${r.proven ? "proven" : "UNPROVEN"}${r.proofs.filter((p) => p.caught).map((p) => ` [${p.how.slice(0, 70)}]`).join("")}`);
  console.log(`result: ${path.join(root, "prove-result.json")}`);
  return summary.ok ? 0 : 1;
}

function runDry(ownerFile) {
  const r = spawnSync(process.execPath, [path.join(here, "ownership-recover.mjs"), ownerFile], { encoding: "utf8" });
  return { exit: r.status, text: `${r.stdout ?? ""}${r.stderr ?? ""}` };
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).then((code) => process.exit(code), (e) => { console.error(`gate-acceptance: ${e.stack ?? e}`); process.exit(2); });
}
