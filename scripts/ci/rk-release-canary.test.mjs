import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";
import zlib from "node:zlib";
import { killRemoteLeg, CHECKS, CanaryInfraError, DIGEST_FILES, EXIT, RECEIPT_NAME, admissionInfra, buildManifest, canaryDigests, compareAll, compareCells, expectedMatch, expectedProblems, fetchBaseline, fetchDependency, loadExpected, loadMatrix, matrixProblems, packCandidate, parseArgs, receiptPath, resolveAppShas, runCanary, runLeg, writeReceipt } from "./rk-release-canary.mjs";
import { readTarball } from "./rk-release-canary-leg.mjs";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const sha256 = (b) => crypto.createHash("sha256").update(b).digest("hex");
const tmp = (t, p = "rk-canary-") => { const d = fs.mkdtempSync(path.join(os.tmpdir(), p)); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };

const mk = (over = {}) => ({ id: "app", app: "app", platform: "mac", side: "candidate", host: "darwin", status: "pass", failNames: [], gateFailNames: [], fatal: null, compilerCache: true, treeChanged: false, treeDelta: [], gatesRun: false, ...over });
const fatal = (signature) => ({ status: "fatal", fatal: { signature, recoverable: false } });
const ctx = { version: "0.6.4", app: "app", platform: "mac" };
const cmp = (candidate, baseline, extra = {}) => compareCells({ candidate: mk(candidate), baseline: mk({ side: "baseline", ...baseline }), ...ctx, ...extra });
const kinds = (r, severity) => r.findings.filter((f) => !severity || f.severity === severity).map((f) => `${f.kind}${f.match && f.severity !== "report" ? `:${f.match}` : ""}`).sort();

// ---------- compareCells: the full table ----------
test("compareCells: identical passes are ok", () => {
  const r = cmp({}, {});
  assert.equal(r.verdict, "ok");
  assert.deepEqual(r.findings, []);
});
test("compareCells: a new [FAIL] row blocks; the baseline's own failures are reported, never blocking", () => {
  const r = cmp({ status: "fail", failNames: ["cargo-profile", "ownership"] }, { status: "fail", failNames: ["ownership"] });
  assert.equal(r.verdict, "regression");
  assert.deepEqual(kinds(r, "block"), ["new-fail-row:cargo-profile"]);
  assert.deepEqual(kinds(r, "report"), ["pre-existing-fail"]);
  const same = cmp({ status: "fail", failNames: ["ownership"] }, { status: "fail", failNames: ["ownership"] });
  assert.equal(same.verdict, "ok");
});
test("compareCells: pass -> not-pass blocks (as fail and as fatal)", () => {
  const f = cmp({ status: "fail", failNames: ["ownership"] }, {});
  assert.deepEqual(kinds(f, "block"), ["new-fail-row:ownership", "pass-to-not-pass:pass->fail"]);
  const x = cmp(fatal("boom"), {});
  assert.deepEqual(kinds(x, "block"), ["pass-to-not-pass:pass->fatal"]);
});
test("compareCells: a changed fatal signature blocks; the same fatal is only reported; fail -> fatal blocks", () => {
  assert.deepEqual(kinds(cmp(fatal("boom v2"), fatal("boom")), "block"), ["fatal-signature-changed:boom v2"]);
  const same = cmp(fatal("boom"), fatal("boom"));
  assert.equal(same.verdict, "ok");
  assert.deepEqual(kinds(same, "report"), ["pre-existing-fatal"]);
  assert.deepEqual(kinds(cmp(fatal("boom"), { status: "fail", failNames: ["ownership"] }), "block"), ["fatal-signature-changed:boom"]);
});
test("compareCells: a baseline fatal that hid the rows does not excuse the candidate's rows", () => {
  const r = cmp({ status: "fail", failNames: ["ownership"] }, fatal("old fatal"));
  assert.deepEqual(kinds(r, "block"), ["new-fail-row:ownership"]);
});
test("compareCells: a baseline refused at its app-check guard evaluated nothing: the candidate's rows and gate rows are reported, a crash still blocks (rightkit#1218)", () => {
  const guard = fatal("the app check found problems in <path>; nothing was started. Fix the above and commit, then run it again.");
  let r = cmp({ status: "fail", failNames: ["ownership"], gatesRun: true, gateFailNames: ["gate:release-switches"] }, guard);
  assert.deepEqual(kinds(r, "block"), []);
  assert.deepEqual(kinds(r, "report").sort(), ["gate-row", "new-fail-row"]);
  assert.equal(r.verdict, "ok");
  r = cmp(fatal("preflight found 4 blocking problem(s); the build would fail on these"), guard);
  assert.deepEqual(kinds(r, "block"), []);
  assert.ok(kinds(r, "report").some((k) => k.startsWith("fatal-signature-changed")));
  r = cmp({ status: "crash", detail: "boom" }, guard);
  assert.deepEqual(kinds(r, "block"), ["candidate-crash:boom"]);
  r = cmp({ status: "fail", failNames: ["ownership"], treeChanged: true, treeDelta: ["x"] }, guard);
  assert.ok(kinds(r, "block").includes("tree-write:x"));
  // A candidate itself refused at the guard is compared as before (same fatal: reported).
  r = cmp(guard, guard);
  assert.deepEqual(kinds(r, "block"), []);
});

test("compareCells: a tree write is a regression, even if the baseline wrote too", () => {
  const delta = ["+?? sub/stray.txt"];
  assert.deepEqual(kinds(cmp({ treeChanged: true, treeDelta: delta }, {}), "block"), ["tree-write:+?? sub/stray.txt"]);
  assert.equal(cmp({ treeChanged: true, treeDelta: delta }, { treeChanged: true, treeDelta: delta }).verdict, "regression");
});
test("compareCells: a candidate crash blocks and can never be excused", () => {
  const entries = CHECKS.map((check) => ({ version: "0.6.4", app: "app", platform: "mac", check, match: "exit 139", reason: "r".repeat(12), issue: "#1" }));
  entries.push({ version: "0.6.4", app: "app", platform: "mac", check: "candidate-crash", match: "exit 139", reason: "r".repeat(12), issue: "#1" });
  const r = cmp({ status: "crash", detail: "exit 139" }, { status: "crash" }, { expected: entries });
  assert.equal(r.verdict, "regression");
  assert.deepEqual(kinds(r, "block"), ["candidate-crash:exit 139"]);
  assert.equal(expectedMatch({ kind: "candidate-crash", match: "exit 139" }, entries, ctx), null);
});
test("compareCells: a baseline crash with a PASSING candidate is reported as unproven and stays non-blocking", () => {
  const r = cmp({}, { status: "crash" });
  assert.equal(r.verdict, "ok");
  assert.deepEqual(kinds(r, "report"), ["unproven"]);
});
test("compareCells: a baseline crash with a candidate that fails or is fatal is incomplete, never green", () => {
  for (const candidate of [{ status: "fail", failNames: ["ownership"] }, fatal("boom")]) {
    const r = cmp(candidate, { status: "crash" });
    assert.equal(r.verdict, "incomplete", candidate.status);
    assert.deepEqual(kinds(r, "incomplete"), ["baseline-crash-unproven:" + candidate.status]);
  }
  const roll = all(full({ "baseline.a.win": { status: "crash" }, "candidate.a.win": { status: "fail", failNames: ["x"] } }));
  assert.equal(roll.verdict, "incomplete");
  const soft = all(full({ "baseline.soft": { status: "crash" }, "candidate.soft": { status: "fatal" } }));
  assert.equal(soft.verdict, "green", "a report-only app cannot hold the canary");
});
test("compareCells: fixes are reported", () => {
  const r = cmp({}, { status: "fail", failNames: ["ownership"] });
  assert.equal(r.verdict, "ok");
  assert.deepEqual(kinds(r, "report"), ["fixed", "fixed-row"]);
});
test("compareCells: gate rows block when the baseline lacks --gates, unless expected", () => {
  const candidate = { status: "fail", failNames: [], gateFailNames: ["gate:build-features"], gatesRun: true };
  const r = cmp(candidate, { gatesRun: false });
  assert.deepEqual(kinds(r, "block"), ["gate-row:gate:build-features", "pass-to-not-pass:pass->fail"]);
  const entry = (check, match) => ({ version: "0.6.4", app: "app", platform: "mac", check, match, reason: "known gate gap, tracked", issue: "bogusyogi/rightkit#940" });
  const excused = cmp(candidate, { gatesRun: false }, { expected: [entry("gate-row", "gate:build-features"), entry("pass-to-not-pass", "pass->fail")] });
  assert.equal(excused.verdict, "ok");
  assert.ok(excused.findings.every((f) => f.severity === "report"));
  assert.equal(excused.findings.find((f) => f.kind === "gate-row").excusedBy.issue, "bogusyogi/rightkit#940");
  assert.deepEqual(kinds(cmp({ ...candidate, status: "pass", gateFailNames: [] }, {}), "block"), []);
});
test("compareCells: infrastructure never yields a verdict: timeout, RECOVERABLE, budget and missing cells are incomplete", () => {
  for (const status of ["timeout", "recoverable", "infra", "budget"]) {
    assert.equal(cmp({ status }, {}).verdict, "incomplete", `candidate ${status}`);
    assert.equal(cmp({}, { status }).verdict, "incomplete", `baseline ${status}`);
  }
  assert.equal(compareCells({ candidate: null, baseline: mk(), ...ctx }).verdict, "incomplete");
  assert.equal(compareCells({ candidate: mk(), baseline: null, ...ctx }).verdict, "incomplete");
  assert.equal(cmp({}, {}, { hostRepresentative: false }).verdict, "incomplete");
});
test("compareCells: incomplete next to a real regression is still a regression", () => {
  const r = cmp({ treeChanged: true, treeDelta: ["+x"] }, {}, { hostRepresentative: false });
  assert.equal(r.verdict, "regression");
});
test("compareCells: a report-only app never turns the canary red or incomplete", () => {
  const r = cmp({ status: "fail", failNames: ["ownership"], treeChanged: true, treeDelta: ["+x"] }, {}, { blocking: false });
  assert.equal(r.verdict, "ok");
  assert.ok(r.findings.every((f) => f.severity === "report"));
  assert.ok(r.findings.some((f) => f.reportOnly === "block"));
  assert.equal(cmp({ status: "timeout" }, {}, { blocking: false }).verdict, "ok");
});

// ---------- expected findings ----------
const ENTRY = { version: "0.6.3", app: "app", platform: "mac", check: "new-fail-row", match: "ownership", reason: "ownership copies in app are tracked", issue: "bogusyogi/rightkit#931" };
test("an expected entry for 0.6.3 does not excuse 0.6.4 (or another app, platform, check or match)", () => {
  const candidate = { status: "fail", failNames: ["ownership"] };
  const at = (version, over = {}) => compareCells({ candidate: mk(candidate), baseline: mk({ status: "pass" }), expected: [{ ...ENTRY, ...over }], version, app: "app", platform: "mac" });
  const only063 = at("0.6.3");
  assert.deepEqual(kinds(only063, "block"), ["pass-to-not-pass:pass->fail"], "the row is excused, the pass->fail it causes is not");
  assert.equal(only063.findings.find((f) => f.kind === "new-fail-row").severity, "report");
  assert.deepEqual(kinds(at("0.6.4", {}), "block"), ["new-fail-row:ownership", "pass-to-not-pass:pass->fail"], "0.6.3's entry does nothing for 0.6.4");
  assert.deepEqual(kinds(at("0.6.3", { app: "other" }), "block"), ["new-fail-row:ownership", "pass-to-not-pass:pass->fail"]);
  assert.deepEqual(kinds(at("0.6.3", { platform: "win" }), "block"), ["new-fail-row:ownership", "pass-to-not-pass:pass->fail"]);
  assert.deepEqual(kinds(at("0.6.3", { match: "ownership2" }), "block"), ["new-fail-row:ownership", "pass-to-not-pass:pass->fail"]);
});
test("expected-file validation: exact, version-bound, no wildcards, no candidate-crash, issue and reason required", () => {
  assert.deepEqual(expectedProblems({ schema: 1, entries: [] }), []);
  assert.deepEqual(expectedProblems({ schema: 1, entries: [ENTRY] }), []);
  assert.match(expectedProblems({ schema: 2, entries: [] })[0], /schema 1/);
  assert.match(expectedProblems({ schema: 1, entries: [{ ...ENTRY, version: "0.6.*" }] }).join(), /exact x\.y\.z|wildcard/);
  assert.match(expectedProblems({ schema: 1, entries: [{ ...ENTRY, match: "own*" }] }).join(), /wildcard/);
  assert.match(expectedProblems({ schema: 1, entries: [{ ...ENTRY, app: "*" }] }).join(), /wildcard/);
  assert.match(expectedProblems({ schema: 1, entries: [{ ...ENTRY, check: "candidate-crash" }] }).join(), /check must be one of/);
  assert.match(expectedProblems({ schema: 1, entries: [{ ...ENTRY, issue: "later" }] }).join(), /issue reference/);
  assert.match(expectedProblems({ schema: 1, entries: [{ ...ENTRY, reason: "x" }] }).join(), /say why/);
  assert.match(expectedProblems({ schema: 1, entries: [{ ...ENTRY, platform: "linux" }] }).join(), /platform/);
  assert.match(expectedProblems({ schema: 1, entries: [ENTRY, ENTRY] }).join(), /duplicates/);
  assert.deepEqual(expectedProblems(loadExpected(REPO)), []);
});

// ---------- compareAll ----------
const MATRIX = { apps: [
  { id: "a", repoDir: "a", platforms: ["mac", "win"], blocking: true },
  { id: "ios", repoDir: "ios", platforms: ["ios"], blocking: true },
  { id: "soft", repoDir: "soft", platforms: ["mac"], blocking: false },
] };
const side = (s, id, platform, over = {}) => mk({ id, app: id, side: s, platform, host: platform === "win" ? "win32" : "darwin", ...over });
const full = (over = {}) => ["candidate", "baseline"].flatMap((s) => [side(s, "a", "mac", over[`${s}.a.mac`]), side(s, "a", "win", over[`${s}.a.win`]), side(s, "ios", "ios", over[`${s}.ios`]), side(s, "soft", "mac", over[`${s}.soft`])]);
const all = (cells, extra = {}) => compareAll({ matrix: MATRIX, candidateCells: cells.filter((c) => c.side === "candidate"), baselineCells: cells.filter((c) => c.side === "baseline"), expected: { entries: [] }, version: "0.6.4", ...extra });
test("compareAll: all-green cells are green on both hosts", () => {
  const r = all(full());
  assert.equal(r.verdict, "green");
  assert.equal(r.cells.length, 4);
  assert.deepEqual(r.representative, { darwin: true, win32: true });
});
test("compareAll: a host whose baseline never showed compiler-cache: broker-managed makes that host's cells incomplete", () => {
  const r = all(full({ "baseline.a.win": { compilerCache: false } }));
  assert.equal(r.verdict, "incomplete");
  assert.deepEqual(r.cells.filter((c) => c.verdict === "incomplete").map((c) => `${c.id}/${c.platform}`), ["a/win"]);
  assert.ok(r.cells.find((c) => c.id === "a" && c.platform === "win").findings.some((f) => f.kind === "host-not-representative"));
});
test("compareAll: a missing cell (leg lost) is incomplete, never green; a non-blocking cell cannot hold the canary", () => {
  const cells = full().filter((c) => !(c.id === "a" && c.platform === "win"));
  assert.equal(all(cells).verdict, "incomplete");
  const soft = full({ "candidate.soft": { status: "timeout" } });
  assert.equal(all(soft).verdict, "green");
});
test("compareAll: a regression anywhere wins over incomplete, and stale expected entries are listed", () => {
  const cells = full({ "candidate.a.mac": { status: "fail", failNames: ["x"] }, "candidate.ios": { status: "timeout" } });
  const r = all(cells, { expected: { entries: [{ ...ENTRY, version: "0.6.4", app: "gone" }] } });
  assert.equal(r.verdict, "regression");
  assert.equal(r.staleExpected.length, 1);
});

// ---------- broker admission and the other new findings ----------
const RNPC = "RK_NOT_PRIMARY_CHECKOUT";
const onMac = (r) => r.cells.find((c) => c.id === "a" && c.platform === "mac");
test("admissionInfra: a refusal is infrastructure only when the probe or the baseline shows the same code", () => {
  const crash = { status: "crash", admissionCode: RNPC, detail: "exit 1" };
  assert.equal(admissionInfra(crash, { side: "candidate", baseline: { status: "pass" }, refused: null }).status, "crash", "candidate-only refusal stays a crash");
  assert.equal(admissionInfra(crash, { side: "candidate", baseline: { ...crash } }).status, "infra", "the baseline shows the code");
  assert.equal(admissionInfra(crash, { side: "candidate", baseline: { status: "pass" }, refused: RNPC }).status, "infra", "the probe refused the code");
  assert.equal(admissionInfra(crash, { side: "candidate", baseline: { status: "pass" }, refused: "RK_REPOSITORY_BUSY" }).status, "crash", "another code does not count");
  assert.equal(admissionInfra(crash, { side: "baseline" }).status, "infra", "a baseline that shows the code is infrastructure itself");
  assert.equal(admissionInfra({ status: "pass", admissionCode: RNPC }, { side: "candidate", refused: RNPC }).status, "pass", "only crash and fail are classed");
  assert.equal(admissionInfra(null, { side: "candidate" }), null);
});
test("compareAll: a candidate-only refusal with a completing baseline still blocks", () => {
  const r = all(full({ "candidate.a.mac": { status: "crash", admissionCode: RNPC, detail: "exit 1" } }));
  assert.equal(r.verdict, "regression");
  assert.equal(onMac(r).verdict, "regression");
  assert.ok(onMac(r).findings.some((f) => f.kind === "candidate-crash" && f.severity === "block"));
});
test("compareAll: a baseline cell that shows the same admission code makes both refusals infrastructure (incomplete, never a verdict)", () => {
  const r = all(full({ "baseline.a.mac": { status: "crash", admissionCode: RNPC, detail: "exit 1" }, "candidate.a.mac": { status: "crash", admissionCode: RNPC, detail: "exit 1" } }));
  assert.equal(r.verdict, "incomplete");
  assert.equal(onMac(r).candidate.status, "infra");
  assert.equal(onMac(r).candidate.detail, `broker admission: ${RNPC}`);
  assert.equal(onMac(r).candidate.refusedAs, "crash");
  assert.equal(onMac(r).baseline.status, "infra");
  assert.deepEqual(onMac(r).findings.map((f) => f.kind).sort(), ["baseline-infra", "candidate-infra"]);
});
test("compareAll: a leg probe that refused the code makes the candidate refusal infrastructure, even when the baseline did not show it", () => {
  const gateRow = { status: "fail", failNames: [], gateFailNames: ["gate:build-features"], gatesRun: true, admissionCode: "RK_BROKER_UNAVAILABLE" };
  const r = all(full({ "candidate.a.mac": gateRow }), { admissionRefused: { darwin: "RK_BROKER_UNAVAILABLE" } });
  assert.equal(r.verdict, "incomplete");
  assert.equal(onMac(r).candidate.status, "infra");
  assert.ok(!onMac(r).findings.some((f) => f.kind === "gate-row"), "a refused gate row is not a gate regression");
  assert.equal(all(full({ "candidate.a.mac": gateRow }), { admissionRefused: { win32: "RK_BROKER_UNAVAILABLE" } }).verdict, "regression", "the refusal of another host does not count");
});
test("compareAll: a gate [FAIL] row that carries the code is infrastructure when the baseline shows it, blocking when only the candidate does", () => {
  const gateRow = { status: "fail", failNames: [], gateFailNames: ["gate:build-features"], gatesRun: true, admissionCode: RNPC };
  const shown = all(full({ "candidate.a.mac": gateRow, "baseline.a.mac": { status: "fail", failNames: [], gateFailNames: ["gate:build-features"], gatesRun: false, admissionCode: RNPC } }));
  assert.equal(onMac(shown).candidate.status, "infra");
  assert.equal(shown.verdict, "incomplete");
  const only = all(full({ "candidate.a.mac": gateRow }));
  assert.equal(only.verdict, "regression");
  assert.ok(onMac(only).findings.some((f) => f.kind === "gate-row" && f.severity === "block"));
});
test("compareAll: a completed candidate cell with the broker-managed compiler cache makes the host representative when no baseline cell could (#1219)", () => {
  const matrix = { apps: [{ id: "a", repoDir: "a", configPath: "c", platforms: ["mac"], blocking: true }] };
  const guard = { id: "a", platform: "mac", host: "darwin", status: "fatal", fatal: { signature: "the app check found problems in <path>; nothing was started. Fix the above and commit, then run it again." }, compilerCache: false, failNames: [], gateFailNames: [] };
  const candidate = { id: "a", platform: "mac", host: "darwin", status: "fail", compilerCache: true, failNames: ["ownership"], gateFailNames: [] };
  const r = compareAll({ matrix, candidateCells: [candidate], baselineCells: [guard], expected: { entries: [] }, version: "1.0.0" });
  assert.deepEqual(r.representative, { darwin: true });
  assert.equal(r.verdict, "green", JSON.stringify(r.cells[0].findings));
});

test("compareAll: host-not-representative says whether no baseline cell completed, or the baseline completed without broker-managed compiler cache", () => {
  const withoutCache = all(full({ "baseline.a.win": { compilerCache: false } }));
  const winCell = withoutCache.cells.find((c) => c.id === "a" && c.platform === "win");
  assert.ok(winCell.findings.some((f) => f.kind === "host-not-representative" && f.detail === "baseline completed without broker-managed compiler cache on win32"), JSON.stringify(winCell.findings));
  const noneCompleted = all(full({ "baseline.a.win": { status: "crash", compilerCache: false, detail: "exit 1" } }));
  const winNone = noneCompleted.cells.find((c) => c.id === "a" && c.platform === "win");
  assert.ok(winNone.findings.some((f) => f.kind === "host-not-representative" && f.detail === "no baseline cell completed on win32"), JSON.stringify(winNone.findings));
  assert.equal(noneCompleted.verdict, "incomplete");
});
test("compareAll: tolerated unnormalized blobs are a report-level finding and never change the verdict", () => {
  const r = all(full({ "candidate.a.mac": { unnormalizedBlobs: ["sub/doc.md"] } }));
  assert.equal(r.verdict, "green");
  assert.ok(onMac(r).findings.some((f) => f.kind === "unnormalized-blob" && f.severity === "report" && f.match === "sub/doc.md"));
  assert.deepEqual(onMac(r).candidate.unnormalizedBlobs, ["sub/doc.md"]);
});

// ---------- matrix ----------
test("the committed matrix is valid, covers every named app, and the Dell node is pinned", () => {
  const matrix = loadMatrix(REPO);
  assert.deepEqual(matrixProblems(matrix, { exists: () => true }), []);
  const byId = Object.fromEntries(matrix.apps.map((a) => [a.id, a]));
  assert.deepEqual(Object.keys(byId).sort(), ["coderight-ios", "coderight-tauri", "cutright", "ember", "genright", "heardright", "legion", "mailright", "pulse", "postright", "scraperight", "viewright"].sort());
  assert.equal(byId.heardright.configPath, "tauri-app-next/right-release.config.mjs");
  assert.equal(byId.cutright.configPath, "apps/cutright/right-release.config.mjs");
  assert.equal(byId["coderight-tauri"].configPath, "apps/coderight-tauri/right-release.config.mjs");
  assert.deepEqual(byId.pulse.platforms, ["win", "mac", "ios"]);
  assert.deepEqual(byId["coderight-ios"].platforms, ["ios"]);
  assert.equal(byId["coderight-ios"].blocking, true);
  assert.equal(byId.ember.blocking, false);
  assert.equal(byId.legion.blocking, false);
  for (const id of ["heardright", "mailright", "viewright", "scraperight", "cutright", "coderight-tauri", "genright", "postright"]) assert.deepEqual(byId[id].platforms, ["mac", "win"], id);
  assert.match(matrix.hosts.win32.node, /^C:\\Users\\adrds\\AppData\\Local\\nvm\\v26\.8\.1\\node\.exe$/);
});
test("every matrix path exists in the Mac workspace (skipped where the workspace is absent)", (t) => {
  const matrix = loadMatrix(REPO);
  if (!fs.existsSync(matrix.hosts.darwin.workspaceRoot)) return t.skip("no workspace on this machine");
  assert.deepEqual(matrixProblems(matrix), []);
});
test("matrix problems: duplicate ids, absolute paths, unknown platform, missing path", () => {
  const base = loadMatrix(REPO);
  const bad = { ...base, apps: [{ ...base.apps[0], configPath: "/etc/x" }, { ...base.apps[0], platforms: ["linux"] }, { ...base.apps[1], id: "zzz" }] };
  const problems = matrixProblems(bad, { exists: (p) => !p.includes("zzz") && !p.includes(base.apps[1].repoDir) }).join("\n");
  assert.match(problems, /duplicated/);
  assert.match(problems, /configPath must be a relative path/);
  assert.match(problems, /platforms must be from/);
  assert.match(problems, /does not exist/);
});

// ---------- inputs ----------
test("resolveAppShas: ls-remote once per repository on origin's refs/heads/main; failures are per repository", async () => {
  const calls = [];
  const run = async (cmd, args) => {
    calls.push([cmd, ...args].join(" "));
    if (args.includes("get-url")) return { code: 0, stdout: `git@example.com:org/${path.basename(args[1])}.git\n`, stderr: "" };
    if (args[1].includes("broken")) return { code: 128, stdout: "", stderr: "fatal: unable to access" };
    return { code: 0, stdout: `${"a".repeat(40)}\trefs/heads/main\n`, stderr: "" };
  };
  const apps = [{ repoDir: "coderight" }, { repoDir: "coderight" }, { repoDir: "pulse" }, { repoDir: "broken" }];
  const repos = await resolveAppShas(apps, { workspaceRoot: "/ws", run });
  assert.deepEqual(repos.coderight, { origin: "git@example.com:org/coderight.git", sha: "a".repeat(40) });
  assert.match(repos.broken.error, /ls-remote .* refs\/heads\/main/);
  assert.equal(calls.filter((c) => c.includes("ls-remote git@example.com:org/coderight.git refs/heads/main")).length, 1);
  assert.equal(calls.filter((c) => c.startsWith("git ls-remote") && c.includes("broken")).length, 2, "one retry, then the error stands");
});

function registry(t, { tamper = false } = {}) {
  const bytes = (name) => zlib.gzipSync(Buffer.from(`tarball of ${name}`));
  const integrity = (b) => `sha512-${crypto.createHash("sha512").update(b).digest("base64")}`;
  const calls = [];
  const fetchImpl = async (url) => {
    calls.push(url);
    const json = (body) => ({ ok: true, status: 200, json: async () => body });
    if (url.endsWith("/@rightkit%2Frelease")) { const b = bytes("release-0.6.2"); return json({ "dist-tags": { latest: "0.6.2" }, versions: { "0.6.2": { dist: { tarball: "https://r/release-0.6.2.tgz", integrity: integrity(tamper ? bytes("other") : b) } } } }); }
    if (url.endsWith("/smol-toml")) { const b = bytes("smol"); return json({ "dist-tags": { latest: "1.9.9" }, versions: { "1.9.1": { dist: { tarball: "https://r/smol-1.9.1.tgz", integrity: integrity(b) } } } }); }
    const body = url.includes("release") ? bytes("release-0.6.2") : bytes("smol");
    return { ok: true, status: 200, arrayBuffer: async () => body.buffer.slice(body.byteOffset, body.byteOffset + body.length) };
  };
  return { fetchImpl, calls, outDir: tmp(t) };
}
test("fetchBaseline reads dist-tags.latest and checks the tarball against the packument integrity; fetchDependency pins smol-toml 1.9.1", async (t) => {
  const r = registry(t);
  const baseline = await fetchBaseline({ fetchImpl: r.fetchImpl, outDir: r.outDir });
  assert.equal(baseline.version, "0.6.2");
  assert.equal(sha256(fs.readFileSync(baseline.file)), baseline.sha256);
  const dep = await fetchDependency({ fetchImpl: r.fetchImpl, outDir: r.outDir, name: "smol-toml", version: "1.9.1" });
  assert.equal(dep.version, "1.9.1");
  assert.ok(r.calls.some((u) => u.endsWith("/smol-toml")));
});
test("fetchBaseline: a tarball that differs from the packument integrity is refused after one retry", async (t) => {
  const r = registry(t, { tamper: true });
  await assert.rejects(fetchBaseline({ fetchImpl: r.fetchImpl, outDir: r.outDir }), (e) => e instanceof CanaryInfraError && /integrity/.test(e.message) && /after one retry/.test(e.message));
});

test("packCandidate packs packages/release as committed (pnpm pack at the SHA), without tests", async (t) => {
  if (spawnSync("pnpm", ["--version"]).status !== 0) return t.skip("pnpm not on PATH");
  const sha = spawnSync("git", ["-C", REPO, "rev-parse", "HEAD"], { encoding: "utf8" }).stdout.trim();
  const version = JSON.parse(spawnSync("git", ["-C", REPO, "show", `${sha}:packages/release/package.json`], { encoding: "utf8" }).stdout).version;
  const outDir = tmp(t);
  const run = (await import("./rk-release-canary.mjs")).exec;
  const packed = await packCandidate({ root: REPO, sha, version, outDir, run });
  const names = readTarball(fs.readFileSync(packed.file)).map((e) => e.path);
  assert.ok(names.includes("package/cli/right-release.mjs"));
  assert.ok(!names.some((n) => n.endsWith(".test.mjs")));
  assert.equal(packed.dependencies["smol-toml"], "1.9.1");
  await assert.rejects(packCandidate({ root: REPO, sha, version: "0.0.1", outDir, run }), /expected @rightkit\/release@0\.0\.1/);
});

// ---------- legs and the whole run ----------
test("buildManifest carries the pinned app commits, artifact digests and per-host paths, nothing host-secret", () => {
  const matrix = loadMatrix(REPO);
  const m = buildManifest({ sha: "s".repeat(40), version: "0.6.3", matrix, repos: { heardright: { origin: "o", sha: "a".repeat(40) } }, candidate: { sha256: "c", version: "0.6.3" }, baseline: { sha256: "b", version: "0.6.2", integrity: "i" }, dependency: { sha256: "d", version: "1.9.1", integrity: "j" } });
  assert.equal(m.artifacts.candidate.file, "candidate.tgz");
  assert.equal(m.repos.heardright.sha, "a".repeat(40));
  assert.equal(m.hosts.win32.workspaceRoot, "D:/claude");
  assert.equal(m.hosts.win32.node, undefined);
  assert.equal(m.apps.length, matrix.apps.length);
});

test("runLeg(win32) copies the bundle, runs the pinned node on the leg script through win, and copies the receipt back", async (t) => {
  const matrix = loadMatrix(REPO);
  const bundle = tmp(t);
  for (const f of ["candidate.tgz", "baseline.tgz", "smol-toml.tgz"]) fs.writeFileSync(path.join(bundle, f), f);
  fs.writeFileSync(path.join(bundle, "canary-leg.mjs"), "// leg");
  fs.writeFileSync(path.join(bundle, "manifest.json"), "{}");
  const sha = "d".repeat(40);
  const calls = [];
  const run = async (cmd, args) => {
    calls.push([cmd, ...args]);
    if (cmd === "scp" && args.at(-1).endsWith("leg-win32.json")) {
      fs.writeFileSync(args.at(-1), JSON.stringify({ schema: "rk-release-canary-leg.v1", host: "win32", sha, manifestSha256: sha256("{}"), legSha256: sha256("// leg"), cells: [] }));
    }
    return { code: 0, stdout: "", stderr: "" };
  };
  const leg = await runLeg("win32", { bundleDir: bundle, matrix, sha, run, winCommand: "win" });
  assert.equal(leg.error, undefined);
  const remote = calls.find((c) => c[0] === "win" && c[1].startsWith("& "));
  // The Windows leg root follows ci/release-canary.json hosts.win32.scratchRoot (D:/scratch/rk-canary-2 since 2026-10-27: the
  // previous root's directories were left locked on the Dell by an earlier leg's orphaned shells).
  const winRoot = "D:\\scratch\\rk-canary-2\\rk-canary\\";
  assert.ok(remote[1].startsWith("& 'C:\\Users\\adrds\\AppData\\Local\\nvm\\v26.8.1\\node.exe' '" + winRoot + sha + "\\canary-leg.mjs' --manifest '" + winRoot + sha + "\\manifest.json'"), remote[1]);
  const put = calls.find((c) => c[0] === "scp" && c.includes("win:D:/scratch/rk-canary-2/rk-canary/" + sha + "/"));
  assert.ok(put, "bundle goes to win:D:/scratch/rk-canary-2/rk-canary/<sha>/");
  assert.ok(put.some((a) => a.endsWith("canary-leg.mjs")) && put.some((a) => a.endsWith("smol-toml.tgz")) && put.some((a) => a.endsWith("manifest.json")));
});
test("runLeg rejects a leg receipt that does not match the bundle (digest, sha, host) after one retry", async (t) => {
  const matrix = loadMatrix(REPO);
  const bundle = tmp(t);
  fs.writeFileSync(path.join(bundle, "canary-leg.mjs"), "// leg"); fs.writeFileSync(path.join(bundle, "manifest.json"), "{}");
  let n = 0;
  const run = async (cmd, args, o) => {
    if (args.includes("--out")) { n += 1; fs.writeFileSync(args[args.indexOf("--out") + 1], JSON.stringify({ schema: "rk-release-canary-leg.v1", host: "darwin", sha: "d".repeat(40), manifestSha256: "0".repeat(64), legSha256: sha256("// leg"), cells: [] })); }
    return { code: 0, stdout: "", stderr: "" };
  };
  const leg = await runLeg("darwin", { bundleDir: bundle, matrix, sha: "d".repeat(40), run, node: "node" });
  assert.match(leg.error, /does not match the bundle \(manifest digest\)/);
  assert.equal(n, 2);
});

function canaryDeps(t, { cells, legErrors = {}, pins = true } = {}) {
  const out = tmp(t);
  const sha = "f".repeat(40);
  const legCalls = [];
  const deps = {
    exec: async (cmd, args) => (args.includes("rev-parse") ? { code: 0, stdout: `${sha}\n`, stderr: "" } : { code: 0, stdout: "", stderr: "" }),
    exists: () => true,
    resolveAppShas: async (apps) => Object.fromEntries(apps.map((a) => [a.repoDir, { origin: `o/${a.repoDir}`, sha: "a".repeat(40) }])),
    packCandidate: async ({ outDir, version }) => { const file = path.join(outDir, "candidate.tgz"); fs.writeFileSync(file, "c"); return { file, sha256: sha256("c"), version, dependencies: { "smol-toml": "1.9.1" } }; },
    fetchBaseline: async ({ outDir }) => { const file = path.join(outDir, "baseline.tgz"); fs.writeFileSync(file, "b"); return { file, sha256: sha256("b"), version: "0.6.2", integrity: "sha512-x" }; },
    fetchDependency: async ({ outDir }) => { const file = path.join(outDir, "smol-toml.tgz"); fs.writeFileSync(file, "d"); return { file, sha256: sha256("d"), version: "1.9.1", integrity: "sha512-y" }; },
    runLeg: async (host) => { legCalls.push(host); return legErrors[host] ? { host, error: legErrors[host] } : { host, receipt: { host, node: "v26", seconds: 1, sides: {}, sideErrors: {}, cells: cells(host) } }; },
  };
  return { out, sha, deps, legCalls };
}
const matrixCells = (over = () => ({})) => (host) => {
  const matrix = loadMatrix(REPO);
  const cells = [];
  for (const app of matrix.apps) for (const platform of app.platforms) {
    if ((platform === "win" ? "win32" : "darwin") !== host) continue;
    for (const s of ["baseline", "candidate"]) cells.push(mk({ id: app.id, app: app.id, platform, side: s, host, ...over(app.id, platform, s) }));
  }
  return cells;
};

test("runCanary: green run writes the receipt (schema, SHA, version, script and expected digests) under <out>/<sha>/", async (t) => {
  const { out, sha, deps, legCalls } = canaryDeps(t, { cells: matrixCells() });
  const r = await runCanary({ root: REPO, out, deps });
  assert.equal(r.code, EXIT.green);
  assert.deepEqual(legCalls.sort(), ["darwin", "win32"]);
  const file = path.join(out, sha, "@rightkit__release.canary.json");
  assert.equal(r.receiptFile, file);
  assert.equal(RECEIPT_NAME, "@rightkit__release.canary.json");
  const receipt = JSON.parse(fs.readFileSync(file, "utf8"));
  assert.equal(receipt.schema, "rk-release-canary.v1");
  assert.equal(receipt.sha, sha);
  assert.equal(receipt.version, JSON.parse(fs.readFileSync(path.join(REPO, "packages/release/package.json"), "utf8")).version);
  assert.equal(receipt.verdict, "green");
  assert.equal(receipt.ok, true);
  assert.deepEqual(receipt.digests, canaryDigests(REPO));
  assert.deepEqual(Object.keys(receipt.digests.scripts), [...DIGEST_FILES]);
  assert.ok(receipt.digests.expected);
  assert.equal(receipt.cells.length, loadMatrix(REPO).apps.reduce((n, a) => n + a.platforms.length, 0));
  assert.equal(receiptPath(out, sha), file);
  assert.ok(fs.existsSync(path.join(out, sha, "canary", "manifest.json")));
});
test("runCanary: a regression is code 10 and the receipt is not green", async (t) => {
  // genright/mac has no expected entry for an `ownership` row (ci/release-canary-expected.json excuses 0.6.9's newly visible rows
  // per app; mailright/mac's ownership row is one of them, rightkit#1218), so the synthetic row stays a regression.
  const { out, deps } = canaryDeps(t, { cells: matrixCells((id, platform, s) => (id === "genright" && platform === "mac" && s === "candidate" ? { status: "fail", failNames: ["ownership"] } : {})) });
  const r = await runCanary({ root: REPO, out, deps });
  assert.equal(r.code, EXIT.regression);
  assert.equal(r.receipt.ok, false);
  assert.ok(r.lines.some((l) => /BLOCK\s+genright\/mac new-fail-row "ownership"/.test(l)), r.lines.join("\n"));
});
test("runCanary: a lost Dell leg is incomplete (code 11), never green; the Mac leg's findings are kept", async (t) => {
  const { out, deps } = canaryDeps(t, { cells: matrixCells(), legErrors: { win32: "dell: scp bundle: connection refused" } });
  const r = await runCanary({ root: REPO, out, deps });
  assert.equal(r.code, EXIT.incomplete);
  assert.equal(r.receipt.ok, false);
  assert.equal(r.receipt.legs.win32.ok, false);
  assert.equal(r.receipt.legs.darwin.ok, true);
});
test("runCanary: an unresolvable blocking app commit or a registry outage is incomplete before any leg runs", async (t) => {
  const a = canaryDeps(t, { cells: matrixCells() });
  a.deps.resolveAppShas = async (apps) => Object.fromEntries(apps.map((x) => [x.repoDir, x.repoDir === "pulse" ? { error: "ls-remote failed" } : { origin: "o", sha: "a".repeat(40) }]));
  const ra = await runCanary({ root: REPO, out: a.out, deps: a.deps });
  assert.equal(ra.code, EXIT.incomplete);
  assert.deepEqual(a.legCalls, []);
  const b = canaryDeps(t, { cells: matrixCells() });
  b.deps.fetchBaseline = async () => { throw new CanaryInfraError("baseline fetch: HTTP 503 (after one retry)"); };
  const rb = await runCanary({ root: REPO, out: b.out, deps: b.deps });
  assert.equal(rb.code, EXIT.incomplete);
  assert.deepEqual(b.legCalls, []);
});
test("runCanary: a candidate whose smol-toml differs from the pin is a regression; stage 0 pin problems stop everything", async (t) => {
  const a = canaryDeps(t, { cells: matrixCells() });
  a.deps.packCandidate = async ({ outDir, version }) => { const file = path.join(outDir, "candidate.tgz"); fs.writeFileSync(file, "c"); return { file, sha256: "x", version, dependencies: { "smol-toml": "1.9.2" } }; };
  assert.equal((await runCanary({ root: REPO, out: a.out, deps: a.deps })).code, EXIT.regression);
  assert.deepEqual(a.legCalls, []);

  const root = tmp(t, "rk-canary-root-");
  fs.mkdirSync(path.join(root, "ci"), { recursive: true }); fs.mkdirSync(path.join(root, "packages/release"), { recursive: true });
  fs.copyFileSync(path.join(REPO, "ci/release-canary.json"), path.join(root, "ci/release-canary.json"));
  fs.copyFileSync(path.join(REPO, "ci/release-canary-expected.json"), path.join(root, "ci/release-canary-expected.json"));
  fs.writeFileSync(path.join(root, "packages/release/package.json"), JSON.stringify({ name: "@rightkit/release", version: "0.6.3" }));
  fs.writeFileSync(path.join(root, "packages/release/rightkit-versions.json"), JSON.stringify({ stagedNpm: { "@rightkit/release": "0.6.2" } }));
  const b = canaryDeps(t, { cells: matrixCells() });
  const r = await runCanary({ root, out: b.out, deps: b.deps });
  assert.equal(r.code, EXIT.regression);
  assert.deepEqual(b.legCalls, []);
  assert.ok(r.lines.some((l) => /stage 0: .*stagedNpm/.test(l)));
  assert.equal(r.receipt.stages.pins.ok, false);
});
test("runCanary --sha: a HEAD other than the checked commit is refused, and HEAD moving before the verdict is never green", async (t) => {
  const { out, sha, deps, legCalls } = canaryDeps(t, { cells: matrixCells() });
  const refused = await runCanary({ root: REPO, out, expectSha: "e".repeat(40), deps });
  assert.equal(refused.code, EXIT.usage);
  assert.match(refused.lines.join("\n"), /not the checked eeeeeeeeeeee; nothing canaried/);
  assert.deepEqual(legCalls, [], "no leg ran for an unchecked commit");
  // Another chat commits while the legs run: the second rev-parse answers a new commit.
  let reads = 0;
  deps.exec = async (cmd, args) => (args.includes("rev-parse") ? { code: 0, stdout: `${(reads += 1) === 1 ? sha : "b".repeat(40)}\n`, stderr: "" } : { code: 0, stdout: "", stderr: "" });
  const moved = await runCanary({ root: REPO, out, expectSha: sha, deps });
  assert.equal(moved.code, EXIT.incomplete);
  assert.match(moved.lines.join("\n"), /HEAD moved from ffffffffffff to bbbbbbbbbbbb while the canary ran/);
  assert.equal(parseArgs(["--sha", sha]).expectSha, sha);
  assert.throws(() => parseArgs(["--sha", "abc"]), /full 40-hex/);
});
test("runCanary: only @rightkit/release has a canary; a dirty tree is refused", async (t) => {
  const { out, deps } = canaryDeps(t, { cells: matrixCells() });
  assert.equal((await runCanary({ root: REPO, out, unit: "@rightkit/qa", deps })).code, EXIT.usage);
  deps.exec = async (cmd, args) => (args.includes("rev-parse") ? { code: 0, stdout: `${"f".repeat(40)}\n`, stderr: "" } : { code: 0, stdout: " M scripts/ci/x.mjs\n", stderr: "" });
  assert.equal((await runCanary({ root: REPO, out, deps })).code, EXIT.usage);
});
test("runCanary: a candidate refusal that the Mac leg's probe refused is incomplete (code 11), not a regression", async (t) => {
  const { out, deps } = canaryDeps(t, { cells: matrixCells() });
  const plain = deps.runLeg;
  deps.runLeg = async (host, opts) => {
    const leg = await plain(host, opts);
    if (host === "darwin") {
      leg.receipt.admissionRefused = RNPC;
      for (const c of leg.receipt.cells) if (c.id === "mailright" && c.side === "candidate" && c.platform === "mac") Object.assign(c, { status: "crash", admissionCode: RNPC, detail: "exit 1" });
    }
    return leg;
  };
  const r = await runCanary({ root: REPO, out, deps });
  assert.equal(r.code, EXIT.incomplete);
  assert.equal(r.receipt.legs.darwin.admissionRefused, RNPC);
  assert.equal(r.receipt.cells.find((c) => c.id === "mailright" && c.platform === "mac").candidate.detail, `broker admission: ${RNPC}`);
});
test("writeReceipt is atomic and parseArgs is strict", (t) => {
  const out = tmp(t);
  const file = writeReceipt(out, "a".repeat(40), { schema: "rk-release-canary.v1" });
  assert.equal(file, path.join(out, "a".repeat(40), "@rightkit__release.canary.json"));
  assert.deepEqual(fs.readdirSync(path.dirname(file)), ["@rightkit__release.canary.json"]);
  assert.throws(() => parseArgs(["--bogus"]), /unknown argument/);
  assert.equal(parseArgs(["--unit", "@rightkit/release", "--out", out]).out, out);
});

test("runLeg(win32): when the Dell leg times out, its node process tree is killed on the Dell, not just ssh", async (t) => {
  const matrix = loadMatrix(REPO);
  const bundle = tmp(t);
  for (const f of ["candidate.tgz", "baseline.tgz", "smol-toml.tgz"]) fs.writeFileSync(path.join(bundle, f), f);
  fs.writeFileSync(path.join(bundle, "canary-leg.mjs"), "// leg"); fs.writeFileSync(path.join(bundle, "manifest.json"), "{}");
  const sha = "d".repeat(40);
  const calls = [];
  const run = async (cmd, args) => { calls.push([cmd, ...args]); return { code: args[0]?.startsWith?.("& ") ? 1 : 0, stdout: "", stderr: "", timedOut: Boolean(args[0]?.startsWith?.("& ")) }; };
  const leg = await runLeg("win32", { bundleDir: bundle, matrix, sha, run });
  assert.match(leg.error, /timed out/);
  const kills = calls.filter((c) => c[0] === "win" && /taskkill\.exe \/PID \$_\.ProcessId \/T \/F/.test(c[1]));
  assert.equal(kills.length, 2, "once per attempt");
  assert.ok(kills[0][1].includes(`*${sha}*canary-leg.mjs*`));
  assert.ok((await killRemoteLeg({ run: async () => ({ code: 0 }), sha })).code === 0);
});

test("the pinned-digest guard covers NPM_VERSION_PINS_SHA256 (see rightkit-dispatch.test.mjs)", () => {
  const dispatchTest = fs.readFileSync(new URL("./rightkit-dispatch.test.mjs", import.meta.url), "utf8");
  assert.match(dispatchTest, /pinned\("NPM_VERSION_PINS_SHA256"\), digest\("\.\/release-version-pins\.mjs"\)/);
});
