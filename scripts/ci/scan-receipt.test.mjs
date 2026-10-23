import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { digestScanSet } from "../../packages/release/deterministic-scans.mjs";
import { artifactMatches } from "./reviewed-candidate.mjs";
import { assertPublishDoc, SCANS_MUTATIONS, SCANS_TOOLS, SCANS_ACTIONLINT_ENV, SCANS_SHELLCHECK_ENV } from "./workflow-contract.mjs";
import { assertReviewedDoc, REVIEWED_MUTATIONS } from "./reviewed-workflow-contract.mjs";
import { parseYaml } from "./workflow-yaml.mjs";
import {
  ACTIONLINT_SHA256, buildScanReceipt, REQUIRED_SCAN_IDS, resultProblems, SCAN_RECEIPT_FILE, SCAN_TOOL_PINS, scanGatePassed, scanReceiptArtifactName,
  sha256Hex, toolVersionProblems, verifyOriginalScanReceipt, verifyScanReceipt,
} from "./scan-receipt.mjs";

const SOURCE = "a".repeat(40);
const LOCK = Buffer.from("lock bytes");
const LOCK_SHA = sha256Hex(LOCK);
const RUN = { repository: "bogusyogi/rightkit", runId: "77", runAttempt: "1" };
const OBSERVED = { "cargo-deny": "cargo-deny 0.20.2", "cargo-audit": "cargo-audit-audit 0.22.2", actionlint: "1.7.12" };
// The scans job proves class A only: Ubuntu cannot compile the native crates, so class B runs in the native verify jobs.
const rows = () => ["cargo-metadata", "cargo-deny", "cargo-audit", "actionlint", "yaml-parse"].map((id) => ({ id, class: "A", command: `cmd ${id}`, status: "pass" }));
const B_ROW = { id: "clippy:rightkit-fs", class: "B", command: "cargo clippy", status: "pass" };
const resultOf = (scans = rows(), classes = ["A"]) => ({ ok: scans.every((r) => r.status === "pass"), complete: scans.every((r) => r.status === "pass"), classes, deferred: scans.filter((r) => r.status === "deferred").map((r) => r.id), scans, toolVersions: {}, inputDigest: "d".repeat(64) });
const receipt = (over = {}) => ({ ...buildScanReceipt({ result: resultOf(), plannedClassA: rows(), sourceCommit: SOURCE, headCommit: SOURCE, cargoLockSha256: LOCK_SHA, observedTools: OBSERVED, run: RUN }), ...over });
const expectation = { sourceSha: SOURCE, cargoLockSha256: LOCK_SHA, runId: "77", runAttempt: "1", repository: "bogusyogi/rightkit" };
const clone = (v) => structuredClone(v);

test("workflow pins agree with the receipt module's trusted pins", () => {
  assert.equal(SCANS_TOOLS, `cargo-deny@${SCAN_TOOL_PINS["cargo-deny"]},cargo-audit@${SCAN_TOOL_PINS["cargo-audit"]}`);
  assert.deepEqual(SCANS_ACTIONLINT_ENV, { ACTIONLINT_VERSION: SCAN_TOOL_PINS.actionlint, ACTIONLINT_SHA256: ACTIONLINT_SHA256 });
  assert.deepEqual(SCANS_SHELLCHECK_ENV, { SHELLCHECK_VERSION: "0.10.0", SHELLCHECK_SHA256: "6c881ab0698e4e6ea235245f22832860544f17ba386442fe7e9d629f8cbedf87" });
});

test("a complete, passed, bound receipt verifies", () => {
  assert.equal(verifyScanReceipt(receipt(), expectation), true);
});

test("scoped receipts bind registry and cover candidates, including a subset after already-live units are skipped", () => {
  const scope = { registry: "crates", units: ["rightkit-a", "rightkit-b"] };
  const expected = { ...expectation, registry: "crates", units: ["rightkit-b"] };
  assert.equal(verifyScanReceipt(receipt({ scope }), expected), true);
  assert.equal(verifyScanReceipt(receipt(), expected), true, "legacy full-tree receipts remain valid");
  for (const bad of [null, {}, { registry: "cargo", units: scope.units }, { registry: "crates", units: [] }, { registry: "crates", units: ["rightkit-b", "rightkit-a"] }, { registry: "crates", units: ["rightkit-a", "rightkit-a"] }, { registry: "crates", units: ["../escape"] }]) {
    assert.throws(() => verifyScanReceipt(receipt({ scope: bad }), expected), /malformed scan scope/);
  }
  for (const mismatch of [{ registry: "npm", units: ["rightkit-b"] }, { registry: "crates", units: ["rightkit-c"] }, { registry: "crates", units: [] }]) {
    assert.throws(() => verifyScanReceipt(receipt({ scope }), { ...expectation, ...mismatch }), /scan scope does not cover/);
  }
  const built = buildScanReceipt({ result: resultOf(), plannedClassA: rows(), sourceCommit: SOURCE, headCommit: SOURCE, cargoLockSha256: LOCK_SHA, observedTools: OBSERVED, run: RUN, scope });
  assert.deepEqual(built.scope, scope);
  assert.notEqual(built.scope.units, scope.units);
});

test("a receipt is refused unless passed and bound to the candidate source, lockfile and run", () => {
  const bad = {
    "not passed": (r) => { r.passed = false; },
    "problems recorded": (r) => { r.problems = ["x"]; },
    "other source commit": (r) => { r.sourceCommit = "b".repeat(40); },
    "other head commit": (r) => { r.headCommit = "b".repeat(40); },
    "other lockfile": (r) => { r.cargoLockSha256 = "0".repeat(64); },
    "other run": (r) => { r.runId = "78"; },
    "other attempt": (r) => { r.runAttempt = "2"; },
    "other repository": (r) => { r.repository = "evil/rightkit"; },
    "foreign schema": (r) => { r.schema = "other/1"; },
    "deferred scan": (r) => { r.deferred = ["generated-docs"]; },
    "failed row": (r) => { r.scans[0].status = "fail"; },
    "deferred row": (r) => { r.scans[0].status = "deferred"; },
    "not-run row": (r) => { r.scans.at(-1).status = "not-run"; },
    "dropped row (digest)": (r) => { r.scans.pop(); },
    "edited command (digest)": (r) => { r.scans[0].command = "true"; },
    "forged digest": (r) => { r.scanSetDigest = "0".repeat(64); },
    "forged class digest": (r) => { r.classDigests.A = "0".repeat(64); },
    "class B digest present": (r) => { r.classDigests.B = digestScanSet([]); },
    "class B row smuggled in": (r) => { r.scans.push({ ...B_ROW }); r.scanSetDigest = digestScanSet(r.scans); r.classDigests = { A: digestScanSet(r.scans.filter((s) => s.class === "A")), B: digestScanSet([B_ROW]) }; },
    "class B row smuggled in (digests recomputed, no B digest)": (r) => { r.scans.push({ ...B_ROW }); r.scanSetDigest = digestScanSet(r.scans); r.classDigests = { A: digestScanSet(r.scans.filter((s) => s.class === "A")) }; },
    "classes record B": (r) => { r.classes = ["A", "B"]; },
    "classes record only B": (r) => { r.classes = ["B"]; },
    "classes missing": (r) => { delete r.classes; },
    "class digests missing": (r) => { delete r.classDigests; },
    "tool pin drift": (r) => { r.tools.pinned["cargo-deny"] = "0.19.0"; },
    "tool version drift": (r) => { r.tools.observed["cargo-audit"] = "cargo-audit-audit 0.21.0"; },
    "tool missing": (r) => { r.tools.observed.actionlint = null; },
  };
  for (const [name, mutate] of Object.entries(bad)) { const r = clone(receipt()); mutate(r); assert.throws(() => verifyScanReceipt(r, expectation), /scan receipt refused/, name); }
  for (const id of REQUIRED_SCAN_IDS) {
    const scans = rows().filter((r) => r.id !== id);
    const r = receipt({ scans, scanSetDigest: digestScanSet(scans), classDigests: { A: digestScanSet(scans.filter((s) => s.class === "A")) } });
    assert.throws(() => verifyScanReceipt(r, expectation), new RegExp(`lacks required scan ${id}`), id);
  }
  for (const [name, expected] of Object.entries({ "other source": { ...expectation, sourceSha: "c".repeat(40) }, "other lock": { ...expectation, cargoLockSha256: "1".repeat(64) } })) assert.throws(() => verifyScanReceipt(receipt(), expected), /scan receipt refused/, name);
  assert.throws(() => verifyScanReceipt(null, expectation), /scan receipt refused/);
});

test("a scan result is evidence only when complete, passed and undeferred", () => {
  const planned = rows();
  assert.deepEqual(resultProblems(resultOf(), planned), []);
  assert.match(resultProblems(resultOf([]), planned)[0], /no scans/);
  assert.ok(resultProblems(resultOf(), undefined).some((p) => /no planned class A scans/.test(p)), "without a plan nothing is proven");
  assert.ok(resultProblems(resultOf(), []).some((p) => /no planned class A scans/.test(p)));
  assert.ok(resultProblems(resultOf(rows().map((r, i) => (i === 0 ? { ...r, status: "deferred" } : r))), planned).some((p) => /deferred/.test(p)));
  assert.ok(resultProblems(resultOf(rows().map((r, i) => (i === 1 ? { ...r, status: "fail" } : r))), planned).some((p) => /cargo-deny: fail/.test(p)));
  assert.ok(resultProblems(resultOf(rows().filter((r) => r.id !== "cargo-audit")), planned).some((p) => /cargo-audit is missing/.test(p)));
  assert.ok(resultProblems(resultOf(rows().filter((r) => r.id !== "cargo-audit")), planned).some((p) => /not every planned class A scan/.test(p)));
  assert.ok(resultProblems(resultOf(), [...planned, { id: "licence", class: "A" }]).some((p) => /not every planned class A scan/.test(p)), "a planned class A scan with no passing row is refused");
  assert.ok(resultProblems(resultOf([...rows(), B_ROW]), planned).some((p) => /class B is not proven/.test(p)), "a class B row is not job-0 evidence");
  assert.ok(resultProblems(resultOf(rows(), ["A", "B"]), planned).some((p) => /exactly class A/.test(p)));
  assert.ok(resultProblems({ ...resultOf(), classes: undefined }, planned).some((p) => /exactly class A/.test(p)));
  assert.deepEqual(toolVersionProblems(OBSERVED), []);
  assert.equal(toolVersionProblems({ ...OBSERVED, "cargo-deny": "cargo-deny 0.20.20" }).length, 1);
  assert.equal(toolVersionProblems({ ...OBSERVED, actionlint: null }).length, 1);
  assert.equal(buildScanReceipt({ result: resultOf(rows().slice(1)), plannedClassA: rows(), sourceCommit: SOURCE, headCommit: SOURCE, cargoLockSha256: LOCK_SHA, observedTools: OBSERVED, run: RUN }).passed, false);
  assert.equal(buildScanReceipt({ result: resultOf(), sourceCommit: SOURCE, headCommit: SOURCE, cargoLockSha256: LOCK_SHA, observedTools: OBSERVED, run: RUN }).passed, false, "no plan, no receipt");
  assert.deepEqual(receipt().classes, ["A"]);
  assert.equal(receipt().passed, true);
});

test("the scans job runs class A only and the native verify jobs keep class B", () => {
  const runner = fs.readFileSync(new URL("./run-deterministic-scans.mjs", import.meta.url), "utf8");
  assert.match(runner, /runScans\(plan, \{ classes: \["A"\] \}\)/);
  assert.doesNotMatch(runner, /classes: \["A", "B"\]/);
  assert.match(runner, /resultProblems\(result, plannedClassA\)/);
  assert.match(runner, /buildScanReceipt\(\{\s*result, plannedClassA,/);
});

test("the CI gate passes only when every planned scan passed", () => {
  const planned = rows().filter((r) => r.class === "A");
  const full = resultOf(planned);
  assert.equal(scanGatePassed(planned, full), true);
  assert.equal(scanGatePassed([], resultOf([])), false, "an empty plan is not a pass");
  assert.equal(scanGatePassed(planned, null), false);
  assert.equal(scanGatePassed(planned, resultOf(planned.slice(1))), false, "a missing scan is not a pass");
  for (const status of ["fail", "deferred", "not-run"]) assert.equal(scanGatePassed(planned, resultOf(planned.map((r, i) => (i === 0 ? { ...r, status } : r)))), false, status);
  assert.equal(scanGatePassed(planned, { ...full, ok: false }), false);
  assert.equal(scanGatePassed(planned, { ...full, complete: false }), false);
  assert.equal(scanGatePassed(planned, { ...full, deferred: ["x"] }), false);
});

test("gate.mjs runs the shared scans in CI with no off switch and counts only a full pass", () => {
  const gate = fs.readFileSync(new URL("./gate.mjs", import.meta.url), "utf8");
  assert.doesNotMatch(gate, /deterministicScansEnabled/);
  assert.doesNotMatch(gate, /deterministic scans: disabled/);
  assert.match(gate, /let scansPassed = false;/);
  assert.match(gate, /if \(STEPS\.has\("scans"\)\) scansPassed = await scansGate\(selection\);/);
  assert.match(gate, /else if \(process\.env\.RK_SCANS_JOB_RESULT === "success"\) scansPassed = true;/);
  assert.doesNotMatch(gate, /let scansPassed = true;/);
  assert.match(gate, /scanGatePassed\(plan\.scans\.filter\(\(x\) => x\.class === "A"\), result\)/);
});

// ---- original-run verification with a fake GitHub ---------------------------------------------------------

function world(over = {}) {
  const candidate = { runId: "77", runAttempt: "1", sourceSha: SOURCE };
  const repository = { id: 99, full_name: "bogusyogi/rightkit" };
  const zip = Buffer.from("zip bytes");
  const name = scanReceiptArtifactName(1);
  const digest = `sha256:${sha256Hex(zip)}`;
  const art = { id: 5, name, digest, expired: false, workflow_run: { id: 77, head_sha: SOURCE, head_branch: "main", repository_id: 99, head_repository_id: 99 } };
  const state = {
    jobs: { total_count: 2, jobs: [{ name: "guard", conclusion: "success" }, { name: "scans", conclusion: "success", run_id: 77, run_attempt: 1, head_sha: SOURCE }] },
    listing: { total_count: 1, artifacts: [{ id: 5, name, digest }] }, artifact: art, zip, files: [SCAN_RECEIPT_FILE], receipt: receipt(), lock: LOCK, ...over,
  };
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "scan-receipt-"));
  const candidateDir = path.join(dir, "candidate"); fs.mkdirSync(candidateDir);
  if (state.lock) fs.writeFileSync(path.join(candidateDir, "Cargo.lock"), state.lock);
  const api = async (route, _env, _fetch, binary) => {
    if (binary) return state.zip;
    if (route.includes("/jobs?")) return state.jobs;
    if (route.includes("/artifacts?name=")) return state.listing;
    if (route === "actions/artifacts/5") return state.artifact;
    throw new Error(`unexpected route ${route}`);
  };
  const unpack = ({ destination }) => { fs.mkdirSync(destination, { recursive: true }); fs.writeFileSync(path.join(destination, SCAN_RECEIPT_FILE), typeof state.receipt === "string" ? state.receipt : JSON.stringify(state.receipt)); return state.files; };
  const run = () => verifyOriginalScanReceipt({ candidate, repository, env: {}, candidateDir, fetchImpl: null, workDir: path.join(dir, "work"), api, artifactMatches, unpack });
  return { run, state };
}

test("original-run scan receipt verifies when present, passed and bound", async () => {
  assert.equal((await world().run()).passed, true);
});

test("seal-check fails closed on a missing, failed, foreign or unbound scan receipt", async () => {
  const cases = {
    "scans job absent": { jobs: { total_count: 1, jobs: [{ name: "guard", conclusion: "success" }] } },
    "scans job failed": { jobs: { total_count: 1, jobs: [{ name: "scans", conclusion: "failure", run_id: 77, run_attempt: 1, head_sha: SOURCE }] } },
    "scans job skipped": { jobs: { total_count: 1, jobs: [{ name: "scans", conclusion: "skipped", run_id: 77, run_attempt: 1, head_sha: SOURCE }] } },
    "scans job other commit": { jobs: { total_count: 1, jobs: [{ name: "scans", conclusion: "success", run_id: 77, run_attempt: 1, head_sha: "b".repeat(40) }] } },
    "no receipt artifact": { listing: { total_count: 0, artifacts: [] } },
    "two receipt artifacts": { listing: { total_count: 2, artifacts: [{ id: 5, name: scanReceiptArtifactName(1) }, { id: 6, name: scanReceiptArtifactName(1) }] } },
    "expired artifact": { artifact: { ...world().state.artifact, expired: true } },
    "foreign run artifact": { artifact: { ...world().state.artifact, workflow_run: { id: 1, head_sha: SOURCE, head_branch: "main", repository_id: 99, head_repository_id: 99 } } },
    "other commit artifact": { artifact: { ...world().state.artifact, workflow_run: { id: 77, head_sha: "b".repeat(40), head_branch: "main", repository_id: 99, head_repository_id: 99 } } },
    "tampered zip bytes": { zip: Buffer.from("other bytes") },
    "extra file in artifact": { files: [SCAN_RECEIPT_FILE, "extra.json"] },
    "wrong file name": { files: ["other.json"] },
    "not JSON": { receipt: "not json" },
    "failed receipt": { receipt: receipt({ passed: false, problems: ["x"] }) },
    "receipt for another commit": { receipt: receipt({ sourceCommit: "b".repeat(40), headCommit: "b".repeat(40) }) },
    "receipt for another lockfile": { lock: Buffer.from("changed lock") },
    "candidate without Cargo.lock": { lock: null },
  };
  for (const [name, over] of Object.entries(cases)) await assert.rejects(world(over).run(), /scan receipt refused/, name);
});

// ---- workflow contracts ------------------------------------------------------------------------------------

test("publish.yml scans-before-every-build-lane contract holds and rejects every scans mutation", () => {
  const doc = parseYaml(fs.readFileSync(new URL("../../.github/workflows/publish.yml", import.meta.url), "utf8"));
  assert.equal(assertPublishDoc(doc), true);
  assert.ok(SCANS_MUTATIONS.length >= 30);
  for (const [name, mutate] of SCANS_MUTATIONS) {
    const copy = structuredClone(doc); mutate(copy);
    assert.throws(() => assertPublishDoc(copy), undefined, name);
  }
  // Every job other than guard and scans transitively needs scans, and each build lane needs it directly.
  const needs = (job) => [].concat(doc.jobs[job].needs ?? []);
  const reaches = (job, target, seen = new Set()) => needs(job).some((n) => n === target || (!seen.has(n) && seen.add(n) && reaches(n, target, seen)));
  for (const job of Object.keys(doc.jobs).filter((j) => j !== "guard" && j !== "scans")) assert.ok(reaches(job, "scans"), job);
  for (const job of Object.keys(doc.jobs).filter((j) => j.startsWith("build-"))) assert.ok(needs(job).includes("scans"), job);
});

test("publish-reviewed.yml seal-check verifies the scan receipt and the contract rejects its removal", () => {
  const doc = parseYaml(fs.readFileSync(new URL("../../.github/workflows/publish-reviewed.yml", import.meta.url), "utf8"));
  assert.equal(assertReviewedDoc(doc), true);
  const step = doc.jobs["seal-check"].steps.find((s) => /scan-receipt\.mjs verify/.test(s.run ?? ""));
  assert.ok(step, "seal-check verifies the scan receipt");
  assert.equal(doc.jobs["seal-check"].steps.indexOf(step) < doc.jobs["seal-check"].steps.findIndex((s) => s.id === "admission"), true);
  const scanMutations = REVIEWED_MUTATIONS.filter(([name]) => /^scan receipt/.test(name));
  assert.ok(scanMutations.length >= 10);
  for (const [name, mutate] of scanMutations) { const copy = structuredClone(doc); mutate(copy); assert.throws(() => assertReviewedDoc(copy), undefined, name); }
});
