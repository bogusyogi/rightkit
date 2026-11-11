// Pure tests of the gate-acceptance runner's assertion logic (gate-acceptance.mjs). Nothing here runs the gate, a compile or git.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import {
  CHECKS, CLAUSES, COVERAGE_SCHEMA, INSTALL_CLAUSES, INSTALL_STAGES, NEGATIVES, RECEIPT_SCHEMA, STEPS, assertCleanCheckout, assertManifestEdit, assertNegative, assertOneFileEdit, assertRecovery,
  assertWarmRerun, brokerDigestProblems, brokerUnchanged, buildPlan, buildResult, bypassRefused, cargoWorkspaceOf, checkUtcStamps, classABeforeCompile, compileRanFresh, coveragePaths, dirtyProof,
  exitedBySignal, formatPlan, formatSummary, hookProblems, ladderStages, launchSpec, makeDirtyCheckout, mergeCoverage, outputDirsProblems, qualifyRefusal, reclaimLine, repositoryKey,
  restoreDirtyCheckout, ruleTierProblems, runningBrokerProcess, sameListing, skippedRows, snapshotDirProblems, stageTimings, summarizeClause, validateCoverage, validateDoorReceipt, validateRenderReceipt,
  validateSourceIdentity, vectorsProblems, workspaceRootOf, worktreeCount,
} from "./gate-acceptance.mjs";

import {
  BROKER_FORBIDDEN_CARGO, BUDGETS_SCHEMA, BUDGET_STAGES, NN, NN07_ALLOWLIST, NN_IDS, assertAdviceCached, assertBatching, assertBudget, assertDependencyReuse, assertDigestStable,
  assertEnvDigestScope, assertKeysStable, assertLanes, assertNoBrokerNonCompiling, assertNoClassARan, assertNoClassBRan, assertNoDuplicateSpawns, assertOneExclusionOnCheck,
  assertNoRealCargoMetadata, assertOtherWorkspacesIdle, assertRulesCached, assertRustfmtSpawns, assertSnapshotProof, assertUnitListShare, evaluateNonNegotiables, findInstalledGate,
  gateIdentityProblems, isExecutingRow, loadBudgets, nnForStep, normalizeSpawn, parseLedger, programOf, risenBudgets, stageMarks, unitSelectorsOf, validateBudgets,
} from "./gate-acceptance.mjs";

const sha = (s) => createHash("sha256").update(s).digest("hex");
const row = (id, coverage, o = {}) => ({ id, class: "A", coverage, status: "pass", key: `k:${id}`, reason: "r", ...o });
const report = (scans, extra = {}) => {
  const c = (w) => scans.filter((s) => s.coverage === w).length;
  return { schema: COVERAGE_SCHEMA, commit: "c".repeat(40), startedAt: "2026-11-05T00:00:00.000Z", endedAt: "2026-11-05T00:00:01.000Z", totals: { planned: scans.length, hit: c("hit"), ran: c("ran"), skipped: c("skipped") }, scans, ...extra };
};
const map = (rows) => new Map(rows.map((r) => [r.id, r]));

test("clause registry has the 67 operative rows with the M/P/G counts of criteria.md", () => {
  const all = Object.values(CLAUSES);
  assert.equal(all.length, 67);
  const n = (t) => all.filter((c) => c.ev === t).length;
  assert.deepEqual([n("M"), n("P"), n("G")], [39, 20, 8]);
});

test("every asserted clause exists in the registry and every step names real clauses", () => {
  for (const id of Object.keys(CHECKS)) assert.ok(CLAUSES[id], id);
  for (const s of STEPS) for (const id of s.clauses) assert.ok(CLAUSES[id], `${s.id} ${id}`);
});

test("plan maps every clause, flags unchecked ones with a reason, runs nothing", () => {
  const plan = buildPlan();
  assert.equal(plan.clauses.length, 67);
  for (const c of plan.clauses) if (!c.implemented) assert.ok(c.unchecked, c.id);
  const text = formatPlan(plan);
  assert.match(text, /S8  kill and recover/);
  assert.match(text, /unchecked: .*GC-A12\(G\)/);
  assert.equal(buildPlan({ steps: ["S2"] }).steps.length, 1);
});

test("nothing installs by default: dev and qualify leave the ladder unless --allow-install, and their clauses say why", () => {
  assert.deepEqual(INSTALL_STAGES, ["dev", "qualify"]);
  assert.deepEqual(ladderStages(), ["check", "compile", "test", "render", "package"]);
  assert.deepEqual(ladderStages({ allowInstall: true }), ["check", "compile", "test", "render", "dev", "package", "qualify"]);
  const s1 = STEPS.find((s) => s.id === "S1");
  assert.ok(!s1.runs.includes("--to dev") && !s1.runs.includes("--to qualify"));
  const plan = buildPlan();
  assert.deepEqual(plan.installStagesSkipped, ["dev", "qualify"]);
  for (const id of INSTALL_CLAUSES) assert.equal(plan.clauses.find((c) => c.id === id).unchecked ?? "needs install approval", "needs install approval");
  assert.match(formatPlan(plan), /pass --allow-install only with Adrian's approval/);
  assert.equal(summarizeClause("GC-T03", [], { allowInstall: false }).reason, "needs install approval");
  assert.notEqual(summarizeClause("GC-T03", [], { allowInstall: true }).reason, "needs install approval");
  assert.equal(buildPlan({ allowInstall: true }).installStagesSkipped.length, 0);
});

test("coverage validation: good report passes; unknown field, bad totals, empty reason all fail", () => {
  assert.ok(validateCoverage(report([row("a", "hit"), row("b", "ran")])).ok);
  assert.match(validateCoverage(report([{ ...row("a", "hit"), extra: 1 }])).problems.join(), /unknown field "extra"/);
  const bad = report([row("a", "hit")]); bad.totals.hit = 2;
  assert.match(validateCoverage(bad).problems.join(), /totals/);
  assert.match(validateCoverage(report([row("a", "skipped", { status: "skipped", reason: "" })])).problems.join(), /reason/);
  assert.match(validateCoverage(report([row("a", "hit", { status: "fail" })])).problems.join(), /hit row/);
  assert.match(validateCoverage(report([row("a", "hit")], { cacheDisabled: "x" })).problems.join(), /bypassed/);
  assert.match(validateCoverage(report([row("a", "hit"), row("a", "hit")])).problems.join(), /duplicate/);
  assert.match(validateCoverage({ ...report([]), schema: "other" }).problems.join(), /schema/);
  assert.equal(validateCoverage(null).ok, false);
});

test("mergeCoverage prefers a real answer over a not-in-pass skip", () => {
  const a = report([row("x", "skipped", { status: "not-in-pass" }), row("y", "ran")]);
  const b = report([row("x", "hit"), row("y", "skipped", { status: "not-in-pass" })]);
  const m = mergeCoverage([a, b]);
  assert.equal(m.get("x").coverage, "hit"); assert.equal(m.get("y").coverage, "ran");
  assert.equal(skippedRows(m).length, 0);
  assert.equal(skippedRows(map([row("z", "skipped", { status: "skipped" })])).length, 1);
});

test("UTC stamps: unstamped line fails; JSON and the verdict marker are exempt", () => {
  const ok = "2026-11-05T10:00:00.123Z rightkit-gate: class A full pass\n{\n  \"a\": 1\n}\nrightkit-gate: verdict deterministic x\n";
  assert.ok(checkUtcStamps(ok).ok);
  assert.match(checkUtcStamps("rightkit-gate: no stamp\n").problems[0], /no UTC stamp/);
  assert.equal(checkUtcStamps("").ok, false);
});

test("log parsing: stage timings, coverage paths, reclaim line", () => {
  const log = "2026-11-05T10:00:00Z rightkit-gate: [check] passed in 12.3s\n2026-11-05T10:00:01Z rightkit-gate: [compile] failed in 4.0s\n2026-11-05T10:00:02Z rightkit-gate: coverage report /a b/x.json: 5 planned scan(s), 1 hit, 4 ran, 0 skipped; scans took 1.0s\n";
  assert.deepEqual(stageTimings(log), [{ stage: "check", ok: true, seconds: 12.3 }, { stage: "compile", ok: false, seconds: 4 }]);
  assert.deepEqual(coveragePaths(log), ["/a b/x.json"]);
  const l = reclaimLine("2026-11-05T10:00:00Z rightkit-gate: snapshot lock held t2 (reclaimed from darwin:9:Wed Nov  5 10:00:00 2026: owner darwin:9:Wed Nov  5 10:00:00 2026 is gone and the broker showed the snapshot idle for commit abc)\n");
  assert.equal(l.token, "t2"); assert.match(l.from, /^darwin:9:/); assert.match(l.proof, /broker showed the snapshot idle/);
  assert.equal(reclaimLine("rightkit-gate: snapshot lock held t2\n"), null);
  assert.equal(reclaimLine("rightkit-gate: build ownership held h2 (reclaimed from h1: gone)\n"), null, "the retired ownership line is no reclaim");
});

const COMMIT = "a".repeat(40); const TREE = "b".repeat(40);
const goodReceipt = () => ({ schema: RECEIPT_SCHEMA, commit: COMMIT, app: ".", binding: { tree: TREE, locks: {}, gate: sha("g"), toolchain: {}, executor: "mac" }, stages: {
  check: { ok: true, at: "2026-11-05T10:00:00.000Z", ruleTier: "compile", snapshot: { path: "/s", tree: TREE, intactAfter: true }, snapshotLock: "h" },
  compile: { ok: true, at: "2026-11-05T10:00:05.000Z", outputs: { dirs: ["/o/ws/g/1/target/aarch64-apple-darwin/release"], truncated: false }, snapshot: { path: "/s", tree: TREE, intactAfter: true }, snapshotLock: "h" } } });

test("door receipt: good passes; wrong tree, broken snapshot, extra field, verdict, ok=false all fail", () => {
  const args = { commit: COMMIT, expectedTree: TREE, stages: ["check", "compile"] };
  assert.ok(validateDoorReceipt(goodReceipt(), args).ok);
  let r = goodReceipt(); r.binding.tree = "c".repeat(40);
  assert.match(validateDoorReceipt(r, args).problems.join(), /binding\.tree/);
  r = goodReceipt(); r.stages.compile.snapshot.intactAfter = false;
  assert.match(validateDoorReceipt(r, args).problems.join(), /intactAfter/);
  r = goodReceipt(); r.stages.compile.surprise = 1;
  assert.match(validateDoorReceipt(r, args).problems.join(), /unknown field "surprise"/);
  r = goodReceipt(); r.verdict = "pass";
  assert.match(validateDoorReceipt(r, args).problems.join(), /verdict/);
  r = goodReceipt(); r.stages.compile.ok = false;
  assert.match(validateDoorReceipt(r, args).problems.join(), /compile\.ok/);
  r = goodReceipt(); delete r.stages.compile;
  assert.match(validateDoorReceipt(r, args).problems.join(), /missing/);
  r = goodReceipt(); r.brokerException = {};
  assert.match(validateDoorReceipt(r, args).problems.join(), /outside the broker/);
});

test("Class A before compile; output dirs one set and no debug", () => {
  assert.ok(classABeforeCompile(goodReceipt()).ok);
  const r = goodReceipt(); r.stages.check.at = "2026-11-05T10:01:00.000Z";
  assert.equal(classABeforeCompile(r).ok, false);
  assert.equal(classABeforeCompile({ stages: {} }).ok, false);
  assert.ok(outputDirsProblems(goodReceipt()).ok);
  const d = goodReceipt(); d.stages.compile.outputs.dirs = ["/o/target/debug"];
  assert.match(outputDirsProblems(d).problems.join(), /debug/);
  const two = goodReceipt(); two.stages.package = { ok: true, at: "x", outputs: { dirs: ["/other"] } };
  assert.match(outputDirsProblems(two).problems.join(), /differ/);
  assert.equal(outputDirsProblems({ stages: { check: {} } }).ok, false);
});

test("warm rerun: a ran or skipped row on an unchanged commit fails; there is no uncached family (NN01)", () => {
  const prev = map([row("rustfmt:1", "ran"), row("pnpm-lockfile:app", "ran"), row("secret-scan", "ran")]);
  const next = map([row("rustfmt:1", "hit", { key: "k:rustfmt:1" }), row("pnpm-lockfile:app", "hit", { key: "k:pnpm-lockfile:app" }), row("secret-scan", "hit", { key: "k:secret-scan" })]);
  assert.ok(assertWarmRerun(prev, next).ok);
  assert.match(assertWarmRerun(prev, map([row("rustfmt:1", "ran"), row("secret-scan", "hit")])).problems.join(), /rustfmt:1: Class A row ran/);
  assert.match(assertWarmRerun(prev, map([row("rustfmt:1", "hit", { key: "other" }), row("secret-scan", "hit")])).problems.join(), /key changed/);
  assert.match(assertWarmRerun(prev, map([row("rustfmt:1", "hit"), row("secret-scan", "skipped", { status: "skipped" })])).problems.join(), /never a pass/);
  assert.match(assertWarmRerun(prev, map([row("rustfmt:1", "hit"), row("pnpm-lockfile:app", "ran"), row("secret-scan", "hit")])).problems.join(), /pnpm-lockfile:app: Class A row ran/);
  assert.match(assertWarmRerun(prev, map([])).problems.join(), /proves nothing/);
});

test("one-file edit: exactly one rustfmt row ran, other keys equal", () => {
  const prev = map([row("rustfmt:a", "hit"), row("rustfmt:b", "hit"), row("secret-scan", "hit"), row("tsc", "hit")]);
  const next = map([row("rustfmt:a", "ran", { key: "new-a" }), row("rustfmt:b", "hit"), row("secret-scan", "ran", { key: "new" }), row("tsc", "hit")]);
  assert.ok(assertOneFileEdit(prev, next).ok);
  assert.match(assertOneFileEdit(prev, map([row("rustfmt:a", "ran", { key: "n1" }), row("rustfmt:b", "ran", { key: "n2" }), row("tsc", "hit")])).problems.join(), /2 rustfmt/);
  assert.match(assertOneFileEdit(prev, map([row("rustfmt:a", "ran", { key: "n" }), row("tsc", "ran", { key: "n" }), row("rustfmt:b", "hit")])).problems.join(), /tsc: ran but does not read/);
  assert.match(assertOneFileEdit(prev, map([row("rustfmt:a", "ran", { key: "n" }), row("tsc", "hit", { key: "moved" })])).problems.join(), /key changed/);
});

test("one-file edit: a row that ran with the key it already had (or no key) is an unexplained miss", () => {
  const prev = map([row("rustfmt:a", "hit"), row("secret-scan", "hit")]);
  const same = assertOneFileEdit(prev, map([row("rustfmt:a", "ran"), row("secret-scan", "hit")]));
  assert.equal(same.ok, false); assert.match(same.problems.join(), /rustfmt:a: ran with the same key/);
  assert.match(assertOneFileEdit(prev, map([row("rustfmt:a", "ran", { key: null })])).problems.join(), /no cache key/);
  assert.match(assertOneFileEdit(map([]), map([row("rustfmt:a", "ran", { key: "n" })])).problems.join(), /absent from the previous run/);
});

const wsInfo = { workspace: "rk", workspaces: [".", "rk", "tools"], crates: { rk: ["leaf", "core"], tools: ["tool"], ".": [] } };
test("manifest edit: the edited workspace's rows ran with new keys and no other workspace's row ran", () => {
  const prev = map([row("cargo-metadata", "hit"), row("cargo-metadata:rk", "hit"), row("cargo-metadata:tools", "hit"), row("cargo-package-list:leaf", "hit"), row("cargo-package-list:tool", "hit")]);
  const good = map([row("cargo-metadata", "hit"), row("cargo-metadata:rk", "ran", { key: "n" }), row("cargo-metadata:tools", "hit"), row("cargo-package-list:leaf", "ran", { key: "n2" }), row("cargo-package-list:tool", "hit")]);
  assert.ok(assertManifestEdit(prev, good, wsInfo).ok, assertManifestEdit(prev, good, wsInfo).problems.join());
  const other = map([row("cargo-metadata", "hit"), row("cargo-metadata:rk", "ran", { key: "n" }), row("cargo-metadata:tools", "ran", { key: "n3" }), row("cargo-package-list:tool", "hit")]);
  assert.match(assertManifestEdit(prev, other, wsInfo).problems.join(), /cargo-metadata:tools: ran for workspace "tools"/);
  const otherPkg = map([row("cargo-metadata:rk", "ran", { key: "n" }), row("cargo-package-list:tool", "ran", { key: "n4" })]);
  assert.match(assertManifestEdit(prev, otherPkg, wsInfo).problems.join(), /cargo-package-list:tool: ran for workspace "tools"/);
  assert.match(assertManifestEdit(prev, map([row("cargo-metadata:rk", "hit"), row("cargo-metadata:tools", "hit")]), wsInfo).problems.join(), /answered hit, expected ran/);
  assert.match(assertManifestEdit(prev, map([row("cargo-metadata:tools", "hit")]), wsInfo).problems.join(), /no cargo-metadata row for workspace "rk"/);
  assert.match(assertManifestEdit(prev, map([row("cargo-metadata:rk", "ran")]), wsInfo).problems.join(), /without a changed key/);
  assert.equal(assertManifestEdit(prev, good, {}).ok, false);
  assert.equal(cargoWorkspaceOf("rk/crates/leaf/Cargo.toml", [".", "rk"]), "rk");
  assert.equal(cargoWorkspaceOf("Cargo.toml", ["."]), ".");
  assert.equal(cargoWorkspaceOf("solo/Cargo.toml", ["rk"]), "solo");
});

const negative = (id) => NEGATIVES.find((n) => n.id === id);
test("negatives assert the coverage row of the named scan or the exact rule code, never a word in the log", () => {
  const n01 = negative("N01"); const n02 = negative("N02"); const n15 = negative("N15"); const n27 = negative("N27");
  const run = (n, extra) => assertNegative({ exit: 1, signals: n.signals, expectRows: n.expectRows ?? [], laterStages: n.later, ...extra });
  // the word is in the log but the scan row is absent or did not fail: old behaviour (a /rustfmt/i signal) passed
  assert.match(run(n01, { text: "rustfmt is great\n", rows: map([row("rustfmt:2021", "hit")]) }).problems.join(), /no coverage row/);
  assert.match(run(n01, { text: "rustfmt\n", rows: null }).problems.join(), /no coverage report/);
  assert.ok(run(n01, { text: "x\n", rows: map([row("rustfmt:2021", "ran", { status: "fail" })]) }).ok);
  assert.ok(run(n01, { text: "x\n", rows: map([row("rustfmt:2021:2", "ran", { status: "fail" })]) }).ok);
  assert.match(run(n02, { text: "secret\n", rows: map([row("secret-scan", "ran")]) }).problems.join(), /no coverage row secret-scan ran\/fail/);
  assert.ok(run(n02, { text: "x\n", rows: map([row("secret-scan", "ran", { status: "fail" })]) }).ok);
  assert.match(run(n15, { text: "something about a profile\n" }).problems.join(), /profile-extra/);
  assert.ok(run(n15, { text: "app [profile-extra] [profile.dev] is not allowed: delete the table\n" }).ok);
  assert.match(run(n27, { text: "an exception was refused\n" }).problems.join(), /RK_BROKER_EXCEPTION_MISSING/);
  assert.ok(run(n27, { text: "rightkit-gate: BLOCKED: RK_BROKER_EXCEPTION_MISSING: the selected commit holds no .rightkit/broker-exceptions.json\n" }).ok);
  assert.deepEqual(NEGATIVES.filter((n) => n.signals.some((x) => x instanceof RegExp && x.flags.includes("i"))), []);
});

test("S1 qualify refusal must print a release-tier FAIL line, not just any BLOCKED or RK_ word", () => {
  assert.equal(qualifyRefusal({ exit: 1, text: "rightkit-gate: BLOCKED: RK_SOMETHING nope\n" }).ok, false);
  assert.equal(qualifyRefusal({ exit: 1, text: "2026-11-05T10:00:00Z FAIL mandatory-foundation (OW-05): x\n" }).ok, true);
  assert.deepEqual(qualifyRefusal({ exit: 1, text: "FAIL commit-citations (G28): c\nUNAPPROVED baseline entry a: b" }).named, ["commit-citations", "UNAPPROVED"]);
  assert.equal(qualifyRefusal({ exit: 1, text: "FAIL scratch-checkout (G23): compile tier\n" }).ok, false);
  assert.match(qualifyRefusal({ exit: 0, text: "FAIL mandatory-foundation (OW-05)" }).problems.join(), /not a refusal/);
  assert.match(qualifyRefusal({ exit: 1, text: "FAIL ownership-exceptions (OW-06)", receipt: { stages: { qualify: { ok: true } } } }).problems.join(), /qualify ok/);
});

test("negative case: needs non-zero exit, named signal, and no later stage passing", () => {
  const text = "2026-11-05T10:00:00Z rightkit-gate: RK_CARGO_WORKSPACE_LIST_MISSING x\n";
  assert.ok(assertNegative({ exit: 1, text, signals: [/RK_CARGO_WORKSPACE_LIST_MISSING/], laterStages: ["compile"] }).ok);
  assert.match(assertNegative({ exit: 0, text, signals: [] }).problems.join(), /exit 0/);
  assert.match(assertNegative({ exit: 1, text: "nope", signals: [/RK_X/] }).problems.join(), /does not name/);
  assert.match(assertNegative({ exit: 1, text: `${text}[compile] passed in 1.0s`, signals: [], laterStages: ["compile"] }).problems.join(), /passed after/);
  assert.match(assertNegative({ exit: null, text, signals: [] }).problems.join(), /did not exit/);
  assert.match(assertNegative({ exit: 1, text, signals: [], laterStages: ["compile"], receipt: { stages: { compile: { ok: true } } } }).problems.join(), /receipt/);
});

test("bypass refusal: must exit non-zero, say retired, and run no stage", () => {
  assert.ok(bypassRefused({ exit: 2, text: "--no-cache and RK_GATE_CACHE=0 are retired" }).ok);
  assert.equal(bypassRefused({ exit: 0, text: "retired" }).ok, false);
  assert.equal(bypassRefused({ exit: 2, text: "other" }).ok, false);
  assert.equal(bypassRefused({ exit: 2, text: "retired [check] passed in 1.0s" }).ok, false);
});

test("recovery: SIGKILL leaves the lock, the next run reclaims it on the stale-owner rule and replaces it", () => {
  const before = { token: "t1", pid: 42 };
  const next = { exit: 0, text: "x rightkit-gate: snapshot lock held t2 (reclaimed from pid 42: owner pid 42 on h is gone (no process answers to it))" };
  assert.ok(assertRecovery({ signal: "SIGKILL", before, next, after: null }).ok);
  assert.match(assertRecovery({ signal: "SIGKILL", before: null, next, after: null }).problems.join(), /no snapshot lock stood/);
  assert.match(assertRecovery({ signal: "SIGKILL", before: { pid: 42 }, next, after: null }).problems.join(), /malformed/);
  assert.match(assertRecovery({ signal: "SIGKILL", before, next: { exit: 0, text: "no reclaim" }, after: null }).problems.join(), /no 'reclaimed from'/);
  assert.match(assertRecovery({ signal: "SIGKILL", before, next: { exit: 0, text: "snapshot lock held t1 (reclaimed from pid 42: x)" }, after: null }).problems.join(), /not replaced/);
  assert.match(assertRecovery({ signal: "SIGKILL", before, next: { exit: 0, text: "build ownership held h2 (reclaimed from h1: gone)" }, after: null }).problems.join(), /no 'reclaimed from'/, "the retired ownership line is no reclaim");
  assert.match(assertRecovery({ signal: "SIGKILL", before, next, after: { token: "t1" } }).problems.join(), /still carries/);
  assert.match(assertRecovery({ signal: "SIGKILL", before, next: { exit: 1, text: next.text }, after: null }).problems.join(), /exited 1/);
  assert.ok(assertRecovery({ signal: "SIGINT", before: null, next: { exit: 0, text: "" }, after: null }).ok);
});

test("GC-I15: the running broker is read from launchd on the Mac and from the process list on Windows; Linux has none", () => {
  const launchd = (answers, psOut = "Wed Nov  5 14:57:59 2026\n") => (cmd, args) => (cmd === "launchctl" ? { stdout: answers[args[1].split("/").pop()] ?? "" } : (assert.equal(cmd, "ps"), { stdout: psOut }));
  const one = runningBrokerProcess({ platform: "darwin", uid: 501, run: launchd({ "dev.rustright.broker": "state = running\n\tpid = 4242\n" }) });
  assert.equal(one.pid, 4242); assert.equal(one.startedMs, Date.parse("Wed Nov  5 14:57:59 2026"));
  assert.equal(runningBrokerProcess({ platform: "darwin", uid: 501, run: launchd({}) }), null, "none running is unknown");
  assert.equal(runningBrokerProcess({ platform: "darwin", uid: 501, run: launchd({ "dev.rustright.broker": "pid = 1\n", "dev.rustright.broker.b": "pid = 2\n" }) }), null, "both entries (mid-cutover) is unknown");
  const winRun = (stdout) => (cmd, args) => { assert.equal(cmd, "powershell.exe"); assert.ok(args.includes("-EncodedCommand")); return { stdout }; };
  assert.deepEqual(runningBrokerProcess({ platform: "win32", run: winRun("60764 2026-11-05T14:57:59.0000000Z\r\n") }), { pid: 60764, startedMs: Date.parse("2026-11-05T14:57:59Z") });
  assert.equal(runningBrokerProcess({ platform: "win32", run: winRun("") }), null, "none or several (no line printed) is unknown");
  assert.equal(runningBrokerProcess({ platform: "win32", run: () => { throw new Error("no powershell"); } }), null);
  assert.equal(runningBrokerProcess({ platform: "linux", run: () => assert.fail("no spawn") }), null);
});


test("source identity, render receipt, vectors, broker digest", () => {
  const trees = { "": TREE, app: "d".repeat(40) };
  const id = { schema: "rightkit.source-identity.v1", commit: COMMIT, trees: { ...trees } };
  assert.ok(validateSourceIdentity(id, { commit: COMMIT, trees }).ok);
  assert.match(validateSourceIdentity({ ...id, trees: { "": TREE } }, { commit: COMMIT, trees }).problems.join(), /"app"/);
  assert.match(validateSourceIdentity({ ...id, trees: { ...trees, x: "1" } }, { commit: COMMIT, trees }).problems.join(), /unexpected/);
  assert.match(validateSourceIdentity({ ...id, extra: 1 }, { commit: COMMIT, trees }).problems.join(), /unknown field/);

  const png = Buffer.alloc(40); Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]).copy(png); png.writeUInt32BE(4, 16); png.writeUInt32BE(3, 20);
  const rr = { schema: "rightkit.render-views/1", commit: COMMIT, tree: TREE, appDir: ".", package: "p", bin: "b", features: [], executable: { path: "/o/bin", sha256: sha("e") }, buildOutputDir: "/o", host: {}, startedAt: "a", endedAt: "b", cases: [{ name: "c", png: "c/c.png", sha256: sha(png), bytes: png.length, width: 4, height: 3 }] };
  assert.ok(validateRenderReceipt(rr, { commit: COMMIT, png: () => png }).ok);
  assert.match(validateRenderReceipt({ ...rr, cases: [{ ...rr.cases[0], width: 9 }] }, { commit: COMMIT, png: () => png }).problems.join(), /width\/height/);
  assert.match(validateRenderReceipt(rr, { commit: COMMIT, png: () => Buffer.alloc(40) }).problems.join(), /bad magic/);
  assert.match(validateRenderReceipt(rr, { commit: COMMIT, png: () => null }).problems.join(), /cannot be read/);
  assert.match(validateRenderReceipt({ ...rr, executable: { path: "/elsewhere", sha256: sha("e") } }, { commit: COMMIT, png: () => png }).problems.join(), /inside buildOutputDir/);

  const src = { schema: "rightkit.vectors-source/1", note: "n", source: {}, files: { "a.json": { sha256: sha("A") } } };
  const up = { "a.json": Buffer.from("A") };
  assert.ok(vectorsProblems({ source: src, files: { "a.json": Buffer.from("A") }, upstream: up }).ok);
  assert.match(vectorsProblems({ source: src, files: { "a.json": Buffer.from("B") }, upstream: up }).problems.join(), /vendored sha256/);
  assert.match(vectorsProblems({ source: src, files: { "a.json": Buffer.from("A"), "z.json": Buffer.from("Z") }, upstream: up }).problems.join(), /not pinned/);
  assert.match(vectorsProblems({ source: src, files: { "a.json": Buffer.from("A") }, upstream: {} }).problems.join(), /no copy to compare/);
  assert.match(vectorsProblems({ source: src, files: { "a.json": Buffer.from("A") }, upstream: { "a.json": Buffer.from("C") } }).problems.join(), /RustRight/);
  assert.ok(brokerDigestProblems({ manifest: { brokerDigest: sha("1") }, pins: { brokerDigest: sha("1") } }).ok);
  assert.match(brokerDigestProblems({ manifest: { brokerDigest: sha("1") }, pins: { brokerDigest: sha("2") } }).problems.join(), /!=/);
  assert.equal(brokerDigestProblems({ manifest: null, pins: null }).ok, false);
});

test("helpers: repository key, listings, worktree count", () => {
  assert.equal(repositoryKey("/x"), sha("/x").slice(0, 16));
  assert.ok(sameListing("l", "a", "a").ok); assert.equal(sameListing("l", "a", "b").ok, false);
  assert.equal(worktreeCount("worktree /a\nHEAD x\n\nworktree /b\n"), 2);
});

test("clause summary: no evidence is unchecked never pass; any fail wins; a blocking missing part keeps it unchecked", () => {
  assert.equal(summarizeClause("GC-A12", []).status, "unchecked");
  assert.match(summarizeClause("GC-A12", []).reason, /writes no artifact/);
  assert.equal(summarizeClause("GC-A04", [{ status: "unchecked", evidence: [], problems: ["x"] }]).status, "unchecked");
  assert.equal(summarizeClause("GC-A04", [{ status: "pass", evidence: ["e"], problems: [] }]).status, "pass");
  const f = summarizeClause("GC-A04", [{ status: "pass", evidence: ["a"], problems: [] }, { status: "fail", evidence: ["b"], problems: ["bad"], label: "S1" }]);
  assert.equal(f.status, "fail"); assert.deepEqual(f.evidence, ["a", "b"]); assert.match(f.problems[0], /S1: bad/);
  assert.equal(summarizeClause("GC-A04", [{ status: "pass", evidence: ["a"], problems: [] }, { status: "unchecked", evidence: [], problems: ["not here"] }]).status, "pass");
  const half = summarizeClause("GC-T19", [{ status: "pass", evidence: ["a"], problems: [] }, { status: "unchecked", blocksPass: true, evidence: [], problems: ["release half missing"] }]);
  assert.equal(half.status, "unchecked"); assert.match(half.reason, /release half missing/);
});

const allUnchecked = () => Object.fromEntries(Object.keys(CLAUSES).map((id) => [id, []]));
const nnAllPass = () => NN_IDS.map((id) => ({ id, status: "pass", problems: [] }));
const rb = (o) => buildResult({ host: "mac", gate: {}, broker: {}, commit: COMMIT, steps: [], clauseChecks: allUnchecked(), startedAt: "a", endedAt: "b", nonNegotiables: nnAllPass(), ...o });
const passing = (extra = {}) => ({ ...allUnchecked(), "GC-A04": [{ status: "pass", evidence: ["r.json"], problems: [] }], ...extra });
const step = (id, checks) => ({ id, title: id, checks, notes: [] });

test("result: ok needs a passing clause, no failing clause, no failing or unfinished step, no failing step-only check", () => {
  const empty = rb({});
  assert.equal(empty.schema, "rightkit.gate-acceptance/1"); assert.equal(empty.clauses.length, 67); assert.equal(empty.totals.pass, 0); assert.equal(empty.ok, false);
  assert.equal(rb({ clauseChecks: passing(), steps: [step("S1", ["pass"])] }).ok, false, "NN34: a partial run never passes");
  const bad = rb({ clauseChecks: passing({ "GC-A21": [{ status: "fail", evidence: ["l"], problems: ["p"] }] }) });
  assert.equal(bad.ok, false); assert.equal(bad.totals.fail, 1);
  // old behaviour: ok ignored the steps, so a failed S9 / S0 / S1 check printed PASS
  assert.equal(rb({ clauseChecks: passing(), steps: [step("S9", ["pass", "fail"])] }).ok, false);
  assert.equal(rb({ clauseChecks: passing(), steps: [step("S1", ["pass"]), step("S0", ["fail"])] }).ok, false);
  assert.equal(rb({ clauseChecks: passing(), steps: [step("S3", [])] }).ok, false);
  assert.match(rb({ clauseChecks: passing(), steps: [step("S3", [])] }).blockers.join(), /step S3 did not run as specified/);
  assert.equal(rb({ clauseChecks: passing(), steps: STEPS.map((x) => step(x.id, x.byDesignUnchecked ? [] : ["pass"])) }).ok, true, "S5 is unchecked by design: a full run with it unchecked passes");
  assert.equal(rb({ clauseChecks: passing(), steps: [step("S1", ["pass"])], stepChecks: [{ step: "S1", name: "qualify refusal", ok: false, problems: ["x"] }] }).ok, false);
  assert.equal(rb({ clauseChecks: passing(), steps: [step("S1", ["pass", "unchecked"])] }).ok, false);
});

test("verdict text: PASS always carries the asserted and unchecked counts, never a bare PASS", () => {
  const ok = rb({ clauseChecks: passing(), steps: [step("S1", ["pass"])] });
  assert.equal(ok.verdict, "NOT PASSED (1 of 67 asserted, 66 unchecked; non-negotiables 19 of 19 passed; partial run: steps S1 only; 0 failed clause(s); 1 blocker(s))");
  assert.match(formatSummary(ok), /^gate acceptance NOT PASSED \(1 of 67 asserted, 66 unchecked; non-negotiables 19 of 19 passed/);
  for (const text of [formatSummary(ok), formatSummary(rb({}))]) assert.doesNotMatch(text, /(?<!NOT )PASS(?! \()/);
  const full = rb({ clauseChecks: passing(), steps: STEPS.map((x) => step(x.id, x.byDesignUnchecked ? [] : ["pass"])) });
  assert.equal(full.verdict, "PASS (1 of 67 asserted, 66 unchecked; non-negotiables 19 of 19 passed)");
  const bad = rb({ clauseChecks: passing({ "GC-A21": [{ status: "fail", evidence: [], problems: ["p"] }] }) });
  assert.match(bad.verdict, /^NOT PASSED \(2 of 67 asserted, 65 unchecked; non-negotiables 19 of 19 passed; 1 failed clause\(s\)/);
  assert.match(formatSummary(bad), /BLOCKER clause GC-A21 failed/);
});

test("a failed or receiptless baseline run fails GC-A04 and GC-A21 instead of leaving them unchecked", () => {
  const base = { stage: "check", commit: COMMIT, tree: TREE, args: ["--to", "check"], log: "2026-11-05T10:00:00Z rightkit-gate: [check] failed in 1.0s\n", logPath: "/l", receiptPath: "/r.json", ladder: true, stageIndex: 0, coverageProblems: [], coveragePaths: [] };
  const failed = { ...base, exit: 1, receipt: null };
  const a04 = summarizeClause("GC-A04", [].concat(CHECKS["GC-A04"]({ runs: { "S1:check": failed } })));
  assert.equal(a04.status, "fail"); assert.match(a04.problems.join(), /exited 1/);
  assert.equal(summarizeClause("GC-A21", [].concat(CHECKS["GC-A21"]({ runs: { "S1:check": failed } }))).status, "fail");
  const noReceipt = { ...base, exit: 0, receipt: null };
  assert.match(summarizeClause("GC-A04", [].concat(CHECKS["GC-A04"]({ runs: { "S1:check": noReceipt } }))).problems.join(), /no readable receipt/);
  // an expected refusal or a negative is not a baseline failure
  assert.equal(CHECKS["GC-A04"]({ runs: { n: { ...failed, negative: "N01" } } }).status, "unchecked");
  assert.equal(CHECKS["GC-A04"]({ runs: { q: { ...failed, refusal: { ok: true, problems: [] } } } }).status, "unchecked");
});

test("a bad GC-I22 comparison: the RustRight contracts dir is derived from the workspace root and a missing dir fails, never skips", () => {
  assert.equal(workspaceRootOf("/w/claude/rightkit/packages/release"), "/w/claude");
  assert.equal(path.join(workspaceRootOf("/w/claude/rightkit/packages/release"), "rustright", "docs", "contracts"), "/w/claude/rustright/docs/contracts");
  const missing = CHECKS["GC-I22"]({ rustrightContracts: "/Volumes/D/scratch/gate-acceptance/no-such-dir" });
  assert.equal(missing.status, "fail"); assert.match(missing.problems.join(), /was not found/);
  const none = CHECKS["GC-I22"]({});
  assert.equal(none.status, "fail");
  assert.match(vectorsProblems({ source: { schema: "s", note: "n", source: {}, files: {} }, files: {}, upstream: null, upstreamDir: "/x" }).problems.join(), /\/x\) was not found/);
});

test("repository key hashes the path as the gate keys it: the verbatim \\\\?\\ spelling on Windows", () => {
  assert.equal(repositoryKey("/x", "darwin"), sha("/x").slice(0, 16));
  const win = "D:\\claude\\rightkit";
  assert.equal(repositoryKey(win, "win32"), sha(`\\\\?\\${win}`).slice(0, 16));
  assert.notEqual(repositoryKey(win, "win32"), sha(win).slice(0, 16));
  assert.equal(repositoryKey(`\\\\?\\${win}`, "win32"), repositoryKey(win, "win32"));
});

test("snapshot dirs: every dir of the repository key counts, a second spelling or a new dir fails GC-A01/GC-T14", () => {
  const key = "0123456789abcdef";
  assert.ok(snapshotDirProblems({ key, before: ["ffff"], after: [key, "ffff"] }).ok);
  assert.ok(snapshotDirProblems({ key, before: null, after: [key] }).ok);
  assert.match(snapshotDirProblems({ key, after: [key, `${key}.staging`] }).problems.join(), /second snapshot dir.*staging/);
  assert.match(snapshotDirProblems({ key, after: [key, `${key}x`] }).problems.join(), /second snapshot dir/);
  assert.match(snapshotDirProblems({ key, after: ["ffff"] }).problems.join(), /does not exist/);
  assert.match(snapshotDirProblems({ key, before: ["ffff"], after: [key, "ffff", "eeee"] }).problems.join(), /new snapshot dir appeared.*eeee/);
  const ctxFail = { managedRoot: "/m", s9: { worktree: "worktree /a\n", snapshotDirCheck: snapshotDirProblems({ key, after: [key, `${key}.old`] }) } };
  assert.equal(summarizeClause("GC-A01", [].concat(CHECKS["GC-A01"]({ runs: {}, ...ctxFail }))).status, "fail");
  assert.equal(summarizeClause("GC-T14", [].concat(CHECKS["GC-T14"]({ runs: {}, ...ctxFail }))).status, "fail");
});

test("S6 proof compares the snapshot with the commit and the dirty checkout, not the commit with itself", () => {
  const commitBytes = Buffer.from("committed\n"); const dirty = Buffer.from("committed\ndirty\n");
  const good = { commitBytes, checkoutBytes: dirty, snapshotBytes: commitBytes, untrackedInSnapshot: false, receiptTree: TREE, expectedTree: TREE };
  assert.ok(dirtyProof(good).ok);
  assert.match(dirtyProof({ ...good, snapshotBytes: dirty }).problems.join(), /differs from the commit.*dirty checkout's bytes/s);
  assert.match(dirtyProof({ ...good, snapshotBytes: null }).problems.join(), /does not hold the edited file/);
  assert.match(dirtyProof({ ...good, untrackedInSnapshot: true }).problems.join(), /untracked checkout file reached/);
  assert.match(dirtyProof({ ...good, checkoutBytes: commitBytes }).problems.join(), /was not dirty/);
  assert.match(dirtyProof({ ...good, receiptTree: "c".repeat(40) }).problems.join(), /compile's tree/);
  assert.match(dirtyProof({ ...good, identityProblems: ["source-identity.trees differ"] }).problems.join(), /source-identity/);
  // a compile that was only reused is not a compile on the dirty checkout
  assert.equal(compileRanFresh("2026-11-05T10:00:00Z rightkit-gate: [compile] already passed for this exact input (commit, tree, lockfiles, gate, toolchain) and its outputs are intact\n"), false);
  assert.equal(compileRanFresh("2026-11-05T10:00:00Z rightkit-gate: [compile] passed in 4.0s\n"), true);
  assert.equal(compileRanFresh(""), false);
  const unproved = summarizeClause("GC-A04", [].concat(CHECKS["GC-A04"]({ runs: {}, s6: { fresh: false, proof: null, why: "the compile was already recorded" } })));
  assert.equal(unproved.status, "unchecked"); assert.match(unproved.reason, /already recorded/);
  assert.equal(summarizeClause("GC-A04", [].concat(CHECKS["GC-A04"]({ runs: {}, s6: { fresh: true, proof: dirtyProof({ ...good, snapshotBytes: dirty }) } }))).status, "fail");
});

const COMPILE_AT = "2026-11-05T10:00:05.000Z";
test("GC-T05 reads this run's own ordering; a reused compile is skipped, not compared with an older run's timestamp", () => {
  const r = goodReceipt();
  const fresh = `2026-11-05T10:00:03.000Z rightkit-gate: [check] passed in 3.0s\n${COMPILE_AT} rightkit-gate: [compile] passed in 1.0s\n`;
  assert.ok(classABeforeCompile(r, fresh).ok);
  const late = "2026-11-05T10:00:09.000Z rightkit-gate: [check] passed in 9.0s\n2026-11-05T10:00:10.000Z rightkit-gate: [compile] passed in 1.0s\n";
  assert.match(classABeforeCompile(r, late).problems.join(), /after the compile stage began/);
  // the receipt kept the compile `at` of an earlier run, so check.at is later than compile.at although nothing is wrong
  const reusedReceipt = goodReceipt(); reusedReceipt.stages.check.at = "2026-11-05T11:00:00.000Z";
  const reusedLog = "2026-11-05T11:00:01.000Z rightkit-gate: [check] passed in 1.0s\n2026-11-05T11:00:02.000Z rightkit-gate: [compile] already passed for this exact input\n";
  const res = classABeforeCompile(reusedReceipt, reusedLog);
  assert.equal(res.ok, false); assert.equal(res.skipped, true);
  const clause = summarizeClause("GC-T05", [].concat(CHECKS["GC-T05"]({ runs: { "S2:compile": { receipt: reusedReceipt, exit: 0, log: reusedLog, receiptPath: "/r" } } })));
  assert.equal(clause.status, "unchecked"); assert.match(clause.reason, /reused a recorded compile/);
  assert.equal(summarizeClause("GC-T05", [].concat(CHECKS["GC-T05"]({ runs: { "S1:compile": { receipt: r, exit: 0, log: fresh, receiptPath: "/r" }, "S2:compile": { receipt: reusedReceipt, exit: 0, log: reusedLog, receiptPath: "/r2" } } }))).status, "pass");
  assert.ok(classABeforeCompile(goodReceipt()).ok);
});

test("GC-A21: a reused compile prints no timing line; a stage that ran owes one; unstamped child output stays a failure", () => {
  const stamped = (m) => `2026-11-05T10:00:00Z rightkit-gate: ${m}\n`;
  const run = (log, o = {}) => ({ stage: "compile", exit: 0, args: [], ladder: true, log, logPath: "/l", coverageProblems: [], coveragePaths: ["/c.json"], ...o });
  const eval21 = (r) => summarizeClause("GC-A21", [].concat(CHECKS["GC-A21"]({ runs: { x: r } })));
  assert.equal(eval21(run(stamped("[compile] already passed for this exact input"))).status, "pass");
  assert.equal(eval21(run(stamped("something ran"))).status, "fail");
  assert.equal(eval21(run(`${stamped("[compile] passed in 1.0s")}cargo says hello without a stamp\n`)).status, "fail");
});

test("GC-A23 reads the identity recorded right after each run, not the snapshot as later steps left it", () => {
  const trees = { "": TREE };
  const recorded = (commit, tree) => ({ file: "/s/.rightkit-source-identity.json", identity: { schema: "rightkit.source-identity.v1", commit, trees: { "": tree } }, trees });
  const runs = { "S1:check": { commit: COMMIT, sourceIdentity: recorded(COMMIT, TREE) }, "S3:check": { commit: "d".repeat(40), sourceIdentity: recorded("d".repeat(40), TREE) } };
  assert.equal(summarizeClause("GC-A23", [].concat(CHECKS["GC-A23"]({ runs }))).status, "pass");
  runs["S3:check"].sourceIdentity = recorded(COMMIT, TREE); // the snapshot was re-synced to another commit: identity names the wrong one
  assert.equal(summarizeClause("GC-A23", [].concat(CHECKS["GC-A23"]({ runs }))).status, "fail");
  assert.equal(summarizeClause("GC-A23", [].concat(CHECKS["GC-A23"]({ runs: { x: { commit: COMMIT, sourceIdentity: { file: "/f", error: "cannot read /f" } } } }))).status, "fail");
  assert.equal(CHECKS["GC-A23"]({ runs: {} }).status, "unchecked");
});

test("credited clauses: GC-T04 executor, GC-T19 rule tier, GC-I15 broker identity, GC-T20 hooks, GC-A22-classA rows, GC-I03/GC-I05 door half", () => {
  const run = (o = {}) => ({ stage: "check", exit: 0, commit: COMMIT, tree: TREE, receiptPath: "/r", logPath: "/l", ladder: true, stageIndex: 0, receipt: { ...goodReceipt(), binding: { ...goodReceipt().binding, executor: "broker" } }, ...o });
  const sum = (id, ctx) => summarizeClause(id, [].concat(CHECKS[id](ctx)), { allowInstall: false });
  assert.equal(sum("GC-T04", { runs: { a: run() } }).status, "pass");
  const direct = run(); direct.receipt.binding.executor = "direct";
  assert.match(sum("GC-T04", { runs: { a: direct } }).problems.join(), /expected "broker"/);
  const exc = run(); exc.receipt.brokerException = {}; assert.equal(sum("GC-T04", { runs: { a: exc } }).status, "fail");

  assert.ok(ruleTierProblems({ stages: { check: { ruleTier: "compile" } } }, "package").ok);
  assert.equal(ruleTierProblems({ stages: { check: { ruleTier: "compile" } } }, "qualify").ok, false);
  assert.equal(ruleTierProblems({ stages: { check: {} } }, "compile").ok, false);
  const tierRun = run(); tierRun.receipt.stages.check.ruleTier = "compile";
  const t19 = sum("GC-T19", { runs: { a: tierRun }, allowInstall: false });
  assert.equal(t19.status, "unchecked"); assert.match(t19.reason, /needs install approval/);
  const q = run({ stage: "qualify", refusal: { ok: true, problems: [] } });
  assert.equal(sum("GC-T19", { runs: { a: tierRun, q }, allowInstall: false }).status, "pass");
  assert.equal(sum("GC-T19", { runs: { a: run({ receipt: { ...tierRun.receipt, stages: { check: { ...tierRun.receipt.stages.check, ruleTier: "release" } } } }) }, allowInstall: true }).status, "fail");

  const b = { pid: 5, startedMs: 9 };
  assert.ok(brokerUnchanged(b, { ...b }).ok); assert.equal(brokerUnchanged(b, { pid: 6, startedMs: 9 }).ok, false); assert.equal(brokerUnchanged(null, b).ok, false);
  assert.equal(sum("GC-I15", { s0: { broker: b }, s9: { broker: { ...b } } }).status, "pass");
  assert.equal(sum("GC-I15", { s0: { broker: b }, s9: { broker: null } }).status, "fail");
  assert.equal(sum("GC-I15", {}).status, "unchecked");

  assert.ok(hookProblems({ files: { "pre-push.sample": "rightkit-gate", "post-commit": "echo hi" } }).ok);
  assert.match(hookProblems({ files: { "pre-push": "#!/bin/sh\nexec rightkit-gate --to check\n" } }).problems.join(), /pre-push runs the gate/);
  assert.equal(sum("GC-T20", { hooks: { dir: "/h", files: {} } }).status, "pass");
  assert.equal(sum("GC-T20", { hooks: { dir: "/h", files: { "pre-push": "node gate.mjs" } } }).status, "fail");
  assert.equal(sum("GC-T20", {}).status, "unchecked");

  const rows = map([row("ghsa-audit", "hit"), row("secret-scan", "ran"), row("feature-coverage", "hit")]);
  const covRun = { rows, coveragePaths: ["/c.json"] };
  assert.equal(sum("GC-A22-classA", { runs: { "S1:check": covRun } }).status, "pass");
  assert.equal(sum("GC-A22-classA", { runs: { "S1:check": { rows: map([row("ghsa-audit", "hit"), row("secret-scan", "skipped", { status: "skipped" })]), coveragePaths: [] } } }).status, "fail");
  const noFeature = sum("GC-A22-classA", { runs: { "S1:check": { rows: map([row("ghsa-audit:app", "hit"), row("secret-scan", "hit")]), coveragePaths: [] } } });
  assert.equal(noFeature.status, "unchecked"); assert.match(noFeature.reason, /feature-coverage/);
  assert.equal(sum("GC-A22-classA", { runs: { "S1:check": { rows: map([row("secret-scan", "hit"), row("feature-coverage", "hit")]), coveragePaths: [] } } }).status, "fail");

  const door = sum("GC-I03", { runs: { a: run({ receipt: { ...goodReceipt(), binding: goodReceipt().binding } , stage: "compile", stageIndex: 1, tree: TREE }) } });
  assert.equal(door.status, "pass");
  const broken = run(); broken.receipt = goodReceipt(); broken.receipt.stages.compile.snapshot.intactAfter = false;
  assert.equal(sum("GC-I03", { runs: { a: broken } }).status, "fail");
  assert.equal(sum("GC-I05", { runs: { a: run() } }).status, "pass");
});

test("exitedBySignal checks the signal (or 128+n, or the Windows kill code), not any non-zero exit", () => {
  assert.equal(exitedBySignal({ signal: "SIGKILL", exit: null, killSignal: "SIGKILL", platform: "darwin" }), true);
  assert.equal(exitedBySignal({ signal: null, exit: 137, killSignal: "SIGKILL", platform: "darwin" }), true);
  assert.equal(exitedBySignal({ signal: null, exit: 130, killSignal: "SIGINT", platform: "darwin" }), true);
  assert.equal(exitedBySignal({ signal: null, exit: 1, killSignal: "SIGKILL", platform: "darwin" }), false, "an ordinary failure is not a kill");
  assert.equal(exitedBySignal({ signal: "SIGTERM", exit: null, killSignal: "SIGKILL", platform: "darwin" }), false);
  assert.equal(exitedBySignal({ signal: null, exit: 0, killSignal: "SIGKILL", platform: "darwin" }), false);
  assert.equal(exitedBySignal({ signal: null, exit: 1, killSignal: "SIGKILL", platform: "win32" }), true);
  assert.equal(exitedBySignal({ signal: null, exit: 137, killSignal: "SIGKILL", platform: "win32" }), false);
});

test("launchSpec: a .cmd launcher goes through cmd.exe with verbatim, safely quoted arguments; unsafe ones throw", () => {
  const posix = launchSpec("/bin/gate", ["--to", "check"], { platform: "darwin" });
  assert.deepEqual([posix.command, posix.args], ["/bin/gate", ["--to", "check"]]);
  assert.deepEqual(launchSpec("D:\\g\\gate.exe", ["a"], { platform: "win32" }).command, "D:\\g\\gate.exe");
  const win = launchSpec("D:\\.rk\\agent-bin\\rightkit-host-gate.cmd", ["--to", "check", "--receipt-out", "D:\\scratch\\a b\\r.json"], { platform: "win32", comspec: "cmd.exe" });
  assert.equal(win.command, "cmd.exe"); assert.deepEqual(win.args.slice(0, 3), ["/d", "/s", "/c"]);
  assert.equal(win.args[3], '"D:\\.rk\\agent-bin\\rightkit-host-gate.cmd --to check --receipt-out "D:\\scratch\\a b\\r.json""');
  assert.equal(win.options.windowsVerbatimArguments, true);
  for (const bad of ["a&calc", "a|b", "%PATH%", 'a"b', "a^b", "a>b", "a\nb"]) assert.throws(() => launchSpec("g.cmd", [bad], { platform: "win32" }), /not safe/);
});

// --- S6 dirty-checkout setup and restore, against a temporary repository -------------------------------------------------------------------------
// The disk policy's scratch root where it exists, the system temp directory on a hosted runner or cloud box.
const SCRATCH = path.join(fs.existsSync("/Volumes/D/scratch") ? "/Volumes/D/scratch" : os.tmpdir(), "gate-acceptance");
const IDENT = { GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@invalid", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@invalid" };
const g = (cwd, ...args) => { const r = spawnSync("git", args, { cwd, encoding: "utf8", env: { ...process.env, ...IDENT } }); assert.equal(r.status, 0, `git ${args.join(" ")}: ${r.stderr}`); return r.stdout; };
function tempRepo() {
  fs.mkdirSync(SCRATCH, { recursive: true });
  const dir = fs.mkdtempSync(path.join(SCRATCH, "t-"));
  g(dir, "init", "-q", "-b", "main");
  fs.writeFileSync(path.join(dir, "README.md"), "hello\n"); fs.writeFileSync(path.join(dir, "lib.rs"), "fn main() {}\n");
  g(dir, "add", "."); g(dir, "commit", "-q", "-m", "init");
  return dir;
}
const cleanup = (dir) => fs.rmSync(dir, { recursive: true, force: true });

test("S6 dirty checkout: restores tracked and untracked state exactly and keeps a pre-existing user stash", () => {
  const dir = tempRepo();
  try {
    fs.appendFileSync(path.join(dir, "README.md"), "user work in progress\n");
    g(dir, "stash", "push", "-q", "-m", "user stash");
    const userStash = g(dir, "stash", "list", "--format=%H").trim();
    assert.match(userStash, /^[0-9a-f]{40}$/);
    const before = g(dir, "stash", "list");
    const state = makeDirtyCheckout(dir, { file: "README.md", untracked: ".gate-acceptance-untracked" });
    assert.notEqual(g(dir, "status", "--porcelain", "--untracked-files=all").trim(), "", "the checkout is dirty while S6 runs");
    assert.equal(g(dir, "stash", "list", "--format=%H").trim().split("\n").length, 2, "our stash stands beside the user's");
    assert.match(fs.readFileSync(path.join(dir, "README.md"), "utf8"), /second dirty edit/);
    const res = restoreDirtyCheckout(dir, state);
    assert.ok(res.ok, res.problems.join());
    assert.equal(g(dir, "status", "--porcelain", "--untracked-files=all").trim(), "");
    assert.equal(fs.readFileSync(path.join(dir, "README.md"), "utf8"), "hello\n");
    assert.equal(fs.existsSync(path.join(dir, ".gate-acceptance-untracked")), false);
    assert.equal(g(dir, "stash", "list", "--format=%H").trim(), userStash, "the user's stash is still there, untouched");
    assert.equal(g(dir, "stash", "list").replace(/stash@\{\d+\}/g, ""), before.replace(/stash@\{\d+\}/g, ""));
  } finally { cleanup(dir); }
});

test("S6 dirty checkout: refuses to start on a dirty fixture and touches nothing", () => {
  const dir = tempRepo();
  try {
    fs.writeFileSync(path.join(dir, "stray.txt"), "mine\n");
    assert.equal(assertCleanCheckout(dir).ok, false);
    assert.throws(() => makeDirtyCheckout(dir, { file: "README.md", untracked: ".gate-acceptance-untracked" }), /refusing to make a dirty checkout: the checkout is not clean/);
    assert.equal(fs.readFileSync(path.join(dir, "README.md"), "utf8"), "hello\n");
    assert.equal(g(dir, "stash", "list").trim(), "");
    assert.equal(fs.readFileSync(path.join(dir, "stray.txt"), "utf8"), "mine\n");
    fs.rmSync(path.join(dir, "stray.txt"));
    fs.appendFileSync(path.join(dir, "README.md"), "tracked edit\n");
    assert.throws(() => makeDirtyCheckout(dir, { file: "README.md", untracked: "u" }), /not clean/);
    assert.equal(fs.readFileSync(path.join(dir, "README.md"), "utf8"), "hello\ntracked edit\n", "a user's tracked edit is never overwritten");
  } finally { cleanup(dir); }
});

test("S6 dirty checkout: a restore that cannot find its stash reports it instead of dropping another one", () => {
  const dir = tempRepo();
  try {
    fs.appendFileSync(path.join(dir, "lib.rs"), "// wip\n"); g(dir, "stash", "push", "-q", "-m", "user stash");
    const userStash = g(dir, "stash", "list", "--format=%H").trim();
    const state = makeDirtyCheckout(dir, { file: "README.md", untracked: "u.tmp" });
    g(dir, "stash", "drop", "stash@{0}"); // somebody dropped ours
    const res = restoreDirtyCheckout(dir, state);
    assert.equal(res.ok, false); assert.match(res.problems.join(), /not in the stash list/);
    assert.equal(g(dir, "stash", "list", "--format=%H").trim(), userStash);
    assert.equal(fs.readFileSync(path.join(dir, "README.md"), "utf8"), "hello\n");
  } finally { cleanup(dir); }
});

// ---------------------------------------------------------------------------------------------------------------------------------------------
// Non-negotiables NN01-NN19: one pure test per assertion, each failing on a synthetic violation and passing on a clean input; then the same violations
// injected into a whole clean evidence set, so the wiring (which run, which evidence) is tested too.
// ---------------------------------------------------------------------------------------------------------------------------------------------
const rowB = (id, coverage, o = {}) => row(id, coverage, { class: "B", ...o });
const led = (cmd, args = [], cwd = "/w", o = {}) => ({ pid: 1, at: "2026-11-05T12:00:00.000Z", ms: 1, sync: true, cmd, args, cwd, status: 0, ...o });

test("parseLedger reads one JSON entry per line and names malformed lines; programOf and normalizeSpawn", () => {
  const ok = parseLedger(`${JSON.stringify(led("/usr/bin/git", ["status"]))}\n\n${JSON.stringify(led("node", []))}\n`);
  assert.equal(ok.entries.length, 2); assert.deepEqual(ok.problems, []);
  const badLines = parseLedger(`{nope\n${JSON.stringify({ cmd: "x" })}\n`);
  assert.equal(badLines.entries.length, 0); assert.equal(badLines.problems.length, 2);
  assert.equal(programOf("C:\\Tools\\rustright.exe"), "rustright"); assert.equal(programOf("/a/b/git"), "git");
  assert.deepEqual(normalizeSpawn(led("git rev-parse HEAD", [], "/r")).args, ["rev-parse", "HEAD"]);
});

test("NN01: an unchanged commit runs no Class A row", () => {
  assert.ok(assertNoClassARan(map([row("rustfmt:2021", "hit"), row("pnpm-lockfile:app", "hit"), rowB("clippy:x", "ran")])).ok, "a Class B row is NN02's");
  const r = assertNoClassARan(map([row("rustfmt:2021", "hit"), row("swift-typecheck:x", "ran", { key: null })]));
  assert.equal(r.ok, false); assert.match(r.problems.join(), /swift-typecheck:x: Class A row ran on an unchanged commit/);
  assert.equal(assertNoClassARan(map([row("pnpm-lockfile:app", "ran")])).ok, false, "no uncached family is exempt any more");
});

test("NN02: an unchanged commit runs no Class B row; a row that failed before and fails again is reported, not re-run silently", () => {
  assert.ok(assertNoClassBRan(map([rowB("clippy:a", "hit"), row("rustfmt:2021", "ran")])).ok);
  const r = assertNoClassBRan(map([rowB("clippy:a", "hit"), rowB("clippy:b", "ran")]));
  assert.equal(r.ok, false); assert.match(r.problems.join(), /clippy:b: Class B row ran/);
  const known = assertNoClassBRan(map([rowB("clippy:a", "ran", { status: "fail" })]), map([rowB("clippy:a", "ran", { status: "fail" })]));
  assert.ok(known.ok); assert.match(known.reported.join(), /failed before and still fails/);
  assert.equal(assertNoClassBRan(map([rowB("clippy:a", "ran", { status: "pass" })]), map([rowB("clippy:a", "ran", { status: "fail" })])).ok, false, "a row that passes now but ran again is a rerun");
  assert.equal(assertNoClassBRan(map([row("rustfmt:2021", "hit")])).ok, false, "no Class B row at all is no evidence");
});

test("NN03: one edited .rs file reruns only the rows that read it (no exemption for pnpm-lockfile)", () => {
  const prev = map([row("rustfmt:2021", "hit"), row("secret-scan", "hit"), row("pnpm-lockfile:app", "hit"), row("tsc:app", "hit")]);
  assert.ok(assertOneFileEdit(prev, map([row("rustfmt:2021", "ran", { key: "n" }), row("secret-scan", "ran", { key: "n" }), row("pnpm-lockfile:app", "hit"), row("tsc:app", "hit")])).ok);
  const lock = assertOneFileEdit(prev, map([row("rustfmt:2021", "ran", { key: "n" }), row("pnpm-lockfile:app", "ran", { key: "n" })]));
  assert.equal(lock.ok, false); assert.match(lock.problems.join(), /pnpm-lockfile:app: ran but does not read the edited file/);
  assert.equal(assertOneFileEdit(prev, map([row("rustfmt:a", "ran", { key: "n" }), row("rustfmt:b", "ran", { key: "m" })])).ok, false, "two rustfmt buckets");
});

test("NN04: one edited manifest reruns only its workspace's rows", () => {
  const info = { workspace: "rk", workspaces: [".", "rk", "tools"], crates: {} };
  const clean = map([row("cargo-metadata:rk", "ran", { key: "n" }), row("cargo-metadata:tools", "hit"), row("ghsa-audit:tools", "hit"), row("cargo-deny:rk", "hit")]);
  assert.ok(assertOtherWorkspacesIdle(clean, info).ok);
  for (const id of ["cargo-metadata:tools", "ghsa-audit:tools", "cargo-audit:tools", "cargo-deny:tools"]) {
    const r = assertOtherWorkspacesIdle(map([row("cargo-metadata:rk", "ran", { key: "n" }), row(id, "ran", { key: "n" })]), info);
    assert.equal(r.ok, false, id); assert.match(r.problems.join(), /workspace "tools" ran/);
  }
  assert.equal(assertOtherWorkspacesIdle(map([row("cargo-metadata:tools", "hit")]), info).ok, false, "no row of the edited workspace is no evidence");
  assert.equal(assertOtherWorkspacesIdle(clean, { workspace: undefined }).ok, false);
});

test("NN05: keys are stable across two identical runs", () => {
  const a = map([row("rustfmt:2021", "hit"), row("secret-scan", "hit"), rowB("clippy:a", "hit")]);
  assert.ok(assertKeysStable(a, map([row("rustfmt:2021", "hit"), row("secret-scan", "hit"), rowB("clippy:a", "hit")])).ok);
  assert.match(assertKeysStable(a, map([row("rustfmt:2021", "hit", { key: "other" }), row("secret-scan", "hit"), rowB("clippy:a", "hit")])).problems.join(), /rustfmt:2021: key differs/);
  assert.match(assertKeysStable(a, map([row("rustfmt:2021", "hit"), row("secret-scan", "hit")])).problems.join(), /clippy:a: present in only one/);
  assert.match(assertKeysStable(map([row("x", "ran", { key: null })]), map([row("x", "ran", { key: null })])).problems.join(), /no cache key/);
  assert.ok(assertKeysStable(map([row("x", "skipped", { status: "not-in-pass", key: null })]), map([])).ok, "a not-in-pass row is no answer");
});

test("NN06: only executing rows carry the environment digest", () => {
  const D = "d".repeat(64);
  const clean = map([row("rustfmt:2021", "hit", { key: '{"files":[["a","1"]]}' }), row("tsc:app", "hit", { key: `{"id":"tsc:app","env":"${D}"}` }), row("oxlint:app", "hit", { key: '{"files":[]}' })]);
  assert.ok(assertEnvDigestScope(clean, [D]).ok, "tsc executes and may carry it");
  for (const key of [`{"files":[],"suffix":"${D}"}`, `{"env":"${D.slice(0, 40)}"}`, `x:${D}`]) {
    const r = assertEnvDigestScope(map([row("rustfmt:2021", "hit", { key })]), [D]);
    assert.equal(r.ok, false, key); assert.match(r.problems.join(), /rustfmt:2021: a non-executing row's key carries the environment digest/);
  }
  assert.equal(assertEnvDigestScope(clean, []).ok, false, "no digest in any log is no evidence");
  assert.equal(isExecutingRow("pnpm-lockfile:app"), true); assert.equal(isExecutingRow("secret-scan"), false);
});

test("NN07: no identical child process twice in one run; an allowlisted probe needs a reason", () => {
  assert.ok(assertNoDuplicateSpawns([led("/usr/bin/git", ["rev-parse", "HEAD"]), led("git", ["rev-parse", "HEAD"], "/other"), led("git", ["status"])]).ok, "another cwd and other args are not repeats");
  const r = assertNoDuplicateSpawns([led("/usr/bin/git", ["rev-parse", "--show-toplevel"]), led("git", ["rev-parse", "--show-toplevel"]), led("ps", ["-A"])]);
  assert.equal(r.ok, false); assert.match(r.problems.join(), /2x git rev-parse --show-toplevel/); assert.equal(r.repeats, 1);
  assert.equal(assertNoDuplicateSpawns([led("git rev-parse HEAD"), led("git", ["rev-parse", "HEAD"])]).ok, false, "a shell-string start is the same as the argv one");
  const dup = [led("ps", ["-A"]), led("ps", ["-A"])];
  assert.ok(assertNoDuplicateSpawns(dup, { allow: [{ program: "ps", args: ["-A"], reason: "the process table must be re-read after the broker starts" }] }).ok);
  assert.match(assertNoDuplicateSpawns(dup, { allow: [{ program: "ps", args: ["-A"], reason: "" }] }).problems.join(), /allowlist entry without/);
  assert.deepEqual(NN07_ALLOWLIST, [], "the allowlist is empty until a repeat is justified from the code");
});

test("NN08: rustfmt is one process per bucket and no cargo fmt starts", () => {
  const ran = map([row("rustfmt:2021", "ran"), row("rustfmt:2018", "hit")]);
  assert.ok(assertRustfmtSpawns([led("rustfmt", ["--version"]), led("rustfmt", ["--edition", "2021", "a.rs", "b.rs"])], ran).ok);
  assert.ok(assertRustfmtSpawns([led("rustfmt", ["--version"])], map([row("rustfmt:2021", "hit")])).ok, "a warm run formats nothing");
  assert.match(assertRustfmtSpawns([led("rustfmt", ["a.rs"]), led("rustfmt", ["b.rs"])], ran).problems.join(), /2 rustfmt process\(es\) formatted for 1 rustfmt bucket/);
  assert.match(assertRustfmtSpawns([led("rustfmt", ["a.rs"])], map([row("rustfmt:2021", "hit")])).problems.join(), /1 rustfmt process\(es\) formatted for 0/);
  for (const e of [led("cargo", ["fmt", "--check"]), led("cargo", ["+stable", "fmt", "-p", "x"]), led("rustright", ["cargo", "fmt"]), led("cargo-fmt", ["--check"])]) assert.match(assertRustfmtSpawns([e], ran).problems.join(), /cargo fmt started/, JSON.stringify(e));
  assert.equal(assertRustfmtSpawns([], null).ok, false);
});

test("NN09: non-compiling cargo never goes through the broker", () => {
  assert.ok(assertNoBrokerNonCompiling([led("rustright", ["cargo", "clippy", "--workspace"]), led("cargo", ["metadata"]), led("rustright", ["cargo", "--version"])]).ok, "compiling calls and direct cargo are not this rule");
  for (const sub of BROKER_FORBIDDEN_CARGO) {
    const r = assertNoBrokerNonCompiling([led("/bin/rustright", ["cargo", sub, "--x"])]);
    assert.equal(r.ok, false, sub); assert.match(r.problems.join(), new RegExp(`rustright cargo ${sub}`));
  }
  assert.equal(assertNoBrokerNonCompiling([led("/tmp/rk-cargo-shim-1/bin/cargo", ["metadata", "--no-deps"])]).ok, false, "the gate's shim carries into the broker");
  assert.equal(assertNoBrokerNonCompiling([led("rustright", ["--root", "/r", "cargo", "+nightly", "tree"])]).ok, false);
});

test("NN10: a warm run starts no real cargo metadata", () => {
  assert.ok(assertNoRealCargoMetadata([led("cargo", ["--version"], "/s/rk"), led("git", ["status"])]).ok);
  const r = assertNoRealCargoMetadata([led("/Users/a/.cargo/bin/cargo", ["metadata", "--no-deps", "--format-version", "1"], "/snap/rk"), led("cargo", ["metadata"], "/snap/tools")]);
  assert.equal(r.ok, false); assert.equal(r.starts, 2); assert.match(r.problems.join(), /2 real cargo metadata start/);
  assert.equal(assertNoRealCargoMetadata([led("rustright", ["cargo", "metadata"])]).ok, false);
});

test("NN11: a warm run proves the snapshot from the resident index; the tree is hashed at most once per run", () => {
  const line = (h, w, r) => `2026-11-05T12:00:00.000Z rightkit-gate: snapshot proof: tree-hashes=${h} verify-walks=${w} resident-proofs=${r}\n`;
  assert.ok(assertSnapshotProof(line(0, 0, 2), { warm: true }).ok);
  assert.ok(assertSnapshotProof(line(1, 1, 0), { warm: false }).ok, "a cold run hashes once and walks");
  assert.match(assertSnapshotProof(line(2, 0, 2), { warm: false }).problems.join(), /hashed 2 times/);
  assert.match(assertSnapshotProof(line(0, 1, 1), { warm: true }).problems.join(), /walked the snapshot 1 time/);
  assert.match(assertSnapshotProof(line(0, 0, 0), { warm: true }).problems.join(), /no resident proof/);
  assert.match(assertSnapshotProof("no such line\n", { warm: true }).problems.join(), /has no `snapshot proof:/);
});

const D1 = "1".repeat(64);
const depLine = (match) => `2026-11-05T12:00:00.000Z rightkit-gate: snapshot dependencies root=app pnpm=11.25.0 (pinned 11.25.0) store=/s lockfile=sha256:abc ms=40 reuse-check-ms=20 dependency-digest-checks=1 reuse-setup=ready reuse-match=${match} reuse-components=none\n2026-11-05T12:00:01.000Z rightkit-gate: class A bytes tracked=a all=a env-at-start=${D1} env=${D1}\n`;
test("NN12: installed dependencies are reused from pnpm's install record and the digest is stable", () => {
  assert.ok(assertDependencyReuse(depLine("hit"), [led("pnpm", ["--version"], "/s/app")]).ok);
  assert.match(assertDependencyReuse(depLine("record-mismatch"), []).problems.join(), /reuse-match=record-mismatch, expected hit/);
  assert.match(assertDependencyReuse(depLine("hit"), [led("pnpm", ["install", "--offline", "--frozen-lockfile"], "/s/app")]).problems.join(), /pnpm install started on a warm run/);
  assert.match(assertDependencyReuse("nothing\n", []).problems.join(), /no `snapshot dependencies root=/);
  assert.match(assertDependencyReuse(depLine("hit").replace(`env=${D1}`, `env=${"2".repeat(64)}`), []).problems.join(), /changed during the run/);
  assert.match(assertDependencyReuse("snapshot dependencies root=app not installed: no lockfile\n", []).problems.join(), /no `snapshot dependencies root/, "a not-installed root is no reuse evidence");
  assert.ok(assertDigestStable(depLine("hit"), depLine("hit")).ok);
  assert.match(assertDigestStable(depLine("hit"), depLine("hit").replaceAll(D1, "3".repeat(64))).problems.join(), /differs between identical runs/);
  assert.equal(assertDigestStable("x", depLine("hit")).ok, false);
});

test("NN13: --to check takes exactly one exclusion, the snapshot lock, and no build-ownership lease", () => {
  const lock = (n = 1) => `2026-11-05T12:00:00.000Z rightkit-gate: snapshot lock held t${n}\n`;
  assert.ok(assertOneExclusionOnCheck(`${lock()}2026-11-05T12:00:01.000Z rightkit-gate: snapshot /s is commit x\n`).ok);
  for (const word of ["held", "joined"]) assert.match(assertOneExclusionOnCheck(`${lock()}2026-11-05T12:00:00.000Z rightkit-gate: build ownership ${word} abc-123\n`).problems.join(), /build-ownership lease, which the gate no longer has/);
  assert.match(assertOneExclusionOnCheck("2026-11-05T12:00:00.000Z rightkit-gate: snapshot /s is commit x\n").problems.join(), /took 0 snapshot lock/);
  assert.match(assertOneExclusionOnCheck(`${lock(1)}${lock(2)}`).problems.join(), /took 2 snapshot lock/);
});

test("NN14: preflight and the app rules run once per tree", () => {
  assert.ok(assertRulesCached("2026-11-05T12:00:00.000Z check: 654e8b663404 app rules: cached pass (tree 0b7e)\n2026-11-05T12:00:00.000Z preflight: 654e8b663404 cached pass (tree 0b7e)\n").ok);
  assert.match(assertRulesCached("2026-11-05T12:00:00.000Z check: 654e8b663404 passed 14 rules (compile tier) in 0.1s\n").problems.join(), /app rules ran again/);
  assert.match(assertRulesCached("2026-11-05T12:00:00.000Z check: x app rules: cached pass (tree y)\n2026-11-05T12:00:00.000Z preflight: ok in 4.3s\n").problems.join(), /preflight ran again/);
});

test("NN15: advice scans are cached per lock set and UTC day or hour", () => {
  const prev = map([row("ghsa-audit:rk", "hit"), row("cargo-deny:rk", "hit"), row("pnpm-audit:app", "hit")]);
  assert.ok(assertAdviceCached(prev, map([row("ghsa-audit:rk", "hit"), row("cargo-deny:rk", "hit"), row("pnpm-audit:app", "hit")]), { startedAt: "2026-11-05T12:00:00Z", prevStartedAt: "2026-11-05T11:59:00Z" }).ok);
  for (const id of ["dependency-freshness", "ghsa-audit:rk", "pnpm-audit:app", "cargo-deny:rk", "cargo-audit:rk"]) {
    const r = assertAdviceCached(map([row(id, "hit")]), map([row(id, "ran")]), { startedAt: "2026-11-05T12:00:00Z", prevStartedAt: "2026-11-05T11:59:00Z" });
    assert.equal(r.ok, false, id); assert.match(r.problems.join(), /ran on a warm run of the same UTC day/);
  }
  const crossed = assertAdviceCached(prev, map([row("ghsa-audit:rk", "ran", { key: "new-day" }), row("cargo-deny:rk", "hit")]), { startedAt: "2026-11-06T00:00:01Z", prevStartedAt: "2026-11-05T23:59:59Z" });
  assert.ok(crossed.ok, "a moved key across a UTC boundary is explained");
  assert.equal(assertAdviceCached(prev, map([row("ghsa-audit:rk", "ran")]), { startedAt: "2026-11-06T00:00:01Z", prevStartedAt: "2026-11-05T23:59:59Z" }).ok, false, "the same key across a boundary is still a rerun");
  assert.equal(assertAdviceCached(prev, map([row("rustfmt:2021", "hit")])).ok, false, "no advice row is no evidence");
});

test("NN16: a unit list shares one Class A and runs no per-unit format check", () => {
  const rep = (n) => `2026-11-05T12:00:00.000Z rightkit-gate: coverage report /r/${n}.json: 3 planned scan(s), 3 hit, 0 ran, 0 skipped\n`;
  const memo = "2026-11-05T12:00:01.000Z rightkit-gate: [unit:check] class A (compile tier) of this unit list already passed on 654e8b663404: not run again\n";
  assert.ok(assertUnitListShare(rep(1) + memo, [led("git", ["status"])], { units: 2 }).ok);
  assert.match(assertUnitListShare(rep(1) + rep(2) + memo, [], { units: 2 }).problems.join(), /2 coverage report\(s\)/);
  assert.match(assertUnitListShare(rep(1), [], { units: 2 }).problems.join(), /0 later unit\(s\) reused the shared Class A/);
  assert.match(assertUnitListShare(rep(1) + memo, [led("cargo", ["fmt", "-p", "rightkit-a", "--check"])], { units: 2 }).problems.join(), /cargo fmt started for a unit list/);
  assert.match(assertUnitListShare(rep(1), [], { units: 1 }).problems.join(), /needs two/);
});

test("NN17: Class A lanes equal the host's cores and Class B batches compatible rows", () => {
  assert.ok(assertLanes("x class A scans 12 at a time, class B: y", { cores: 12 }).ok);
  assert.match(assertLanes("x class A scans 4 at a time", { cores: 12 }).problems.join(), /4 lane\(s\) on a 12-core host/);
  assert.match(assertLanes("x class A scans 12 at a time", { cores: 12, jobsOverride: "4" }).problems.join(), /RK_GATE_JOBS=4/);
  assert.match(assertLanes("no plan line", { cores: 12 }).problems.join(), /no `class A scans N at a time` line/);
  const note = "t rightkit-gate: note: class B batching: 37 compile scan(s) in 12 configuration group(s); each group runs as one invocation (29 scan(s) in 4 shared group(s)), a failed group is bisected\nt rightkit-gate: note: class B batching: 4 compile scan(s) stay their own invocation: sidecar-bound\n";
  const few = Array.from({ length: 10 }, (_, i) => led("rustright", ["cargo", "clippy", "-p", `c${i}`], `/s/rk${i}`));
  assert.ok(assertBatching(note, few, { exit: 0 }).ok);
  const many = Array.from({ length: 40 }, (_, i) => led("rustright", ["cargo", "clippy", "-p", `c${i}`], `/s/rk${i}`));
  assert.match(assertBatching(note, many, { exit: 0 }).problems.join(), /40 compiling cargo invocation\(s\) for 12 configuration group\(s\) and 4 solo/);
  assert.match(assertBatching(note.replace("(29 scan(s) in 4 shared group(s))", "(0 scan(s) in 0 shared group(s))"), few, { exit: 0 }).problems.join(), /no compatible group was batched/);
  assert.match(assertBatching("no note", few, { exit: 0 }).problems.join(), /no `class B batching:/);
  assert.ok(assertBatching(note, many, { exit: 1 }).ok, "a failed run bisects: the invocation bound is judged only on a passing run");
});

test("NN18: stage times come from named stamped lines; a budget that is null fails", () => {
  const t0 = Date.parse("2026-11-05T12:00:00.000Z"); const ts = (ms) => new Date(t0 + ms).toISOString();
  const log = [`${ts(200)} rightkit-gate: snapshot lock held h`, `${ts(500)} rightkit-gate: snapshot /s/abc is commit ${"a".repeat(40)} tree ${"b".repeat(40)} (written=0 removed=0 unchanged=77)`,
    `${ts(600)} rightkit-gate: --to compile for aaaaaaaaaaaa: check -> compile; executor broker`, `${ts(1000)} rightkit-gate: 120 scan(s) for 77 changed file(s) vs aaaaaaaaaaaa; class A scans 12 at a time`,
    `${ts(1400)} rightkit-gate: snapshot dependencies root=app pnpm=1`, `${ts(1500)} rightkit-gate: snapshot dependencies root=lib pnpm=1`].join("\n");
  const m = stageMarks(log, t0);
  assert.deepEqual(m.problems, []); assert.deepEqual(m.marks, { ownership: 200, snapshotSync: 300, planning: 400, dependencyStep: 400 });
  assert.equal(stageMarks(log.replace(/^.*snapshot lock held.*\n/, ""), t0).marks.ownership, 0, "a run that takes no lock has no lock stage");
  assert.equal(stageMarks(log.replace(/^.*snapshot lock held.*\n/, ""), t0).marks.snapshotSync, 500, "...and its sync is timed from the run start");
  assert.match(stageMarks(log.replace(/^.*class A scans.*\n/m, ""), t0).problems.join(), /no stamped `<n> scan\(s\)/);
  const budgets = { schema: BUDGETS_SCHEMA, hosts: { mac: { warmCheckMs: 5000, warmCompileMs: 6000, stages: { ownership: 400, snapshotSync: 400, dependencyStep: 600, planning: 500 } }, dell: { warmCheckMs: null, warmCompileMs: null, stages: { ownership: null, snapshotSync: null, dependencyStep: null, planning: null } } } };
  assert.ok(validateBudgets(budgets).ok);
  const run = (o = {}) => ({ label: "S2:check", kind: "check", ms: 4000, marks: m.marks, ...o });
  assert.ok(assertBudget({ host: "mac", budgets, runs: [run(), run({ kind: "compile", ms: 5999 })] }).ok);
  assert.match(assertBudget({ host: "mac", budgets, runs: [run({ ms: 5001 })] }).problems.join(), /took 5001 ms, budget 5000 ms/);
  assert.match(assertBudget({ host: "mac", budgets, runs: [run({ marks: { ...m.marks, dependencyStep: 601 } })] }).problems.join(), /stage dependencyStep took 601 ms, budget 600 ms/);
  assert.match(assertBudget({ host: "mac", budgets, runs: [run({ marks: { ...m.marks, planning: null } })] }).problems.join(), /stage planning could not be timed/);
  const dell = assertBudget({ host: "dell", budgets, runs: [run()] });
  assert.equal(dell.ok, false); assert.match(dell.problems.join(), /no budget measured for dell: warmCheckMs/); assert.match(dell.problems.join(), /no budget measured for dell: stages\.snapshotSync/);
  assert.equal(assertBudget({ host: "mac", budgets, runs: [] }).ok, false);
  assert.equal(assertBudget({ host: "linux", budgets, runs: [run()] }).ok, false);
  assert.equal(validateBudgets({ ...budgets, schema: "x" }).ok, false);
  assert.equal(validateBudgets({ ...budgets, hosts: { mac: { ...budgets.hosts.mac, warmCheckMs: 0 }, dell: budgets.hosts.dell } }).ok, false, "a budget is positive or null");
  assert.equal(validateBudgets({ ...budgets, hosts: { mac: budgets.hosts.mac } }).ok, false);
});

test("NN18: the committed budgets file is valid, names its measurement, and leaves the Dell unmeasured (null)", () => {
  const loaded = loadBudgets();
  assert.equal(loaded.problem, null, loaded.problem ?? "");
  const mac = loaded.budgets.hosts.mac;
  assert.equal(mac.warmCompileMs, null, "no passing warm compile was measured, so it stays null and NN18 fails on it");
  for (const v of [mac.warmCheckMs, ...BUDGET_STAGES.map((s) => mac.stages[s])]) assert.ok(Number.isFinite(v) && v > 0);
  assert.match(loaded.budgets.method, /best measured optimized-state time x ?1\.25/);
  assert.match(loaded.budgets.method, /Provisional/);
  assert.match(loaded.budgets.method, /only ever go down/);
  assert.ok(Array.isArray(loaded.budgets.measured?.mac?.sessions) && loaded.budgets.measured.mac.sessions.length === 3, "three measured sessions on the Mac");
  assert.equal(loaded.budgets.hosts.dell.warmCheckMs, null);
  assert.equal(loadBudgets("/nonexistent/budgets.json").budgets, null);
});

test("NN19: the gate under test is the installed sealed gate; a checkout gate must be named by --gate", () => {
  const installed = { version: "0.6.46", entry: "/m/trusted-gates/x/node_modules/@rightkit/release/bin/rightkit-gate.mjs" };
  const base = { gateArg: undefined, defaultGate: "/m/agent-bin/rightkit-host-gate", launcherExists: true, launcherVersion: "0.6.46", launcherText: "#!/usr/bin/env node\n", installed, checkoutRoot: "/w/rightkit" };
  const ok = gateIdentityProblems(base);
  assert.ok(ok.ok); assert.equal(ok.kind, "installed");
  assert.match(gateIdentityProblems({ ...base, launcherVersion: "0.6.47" }).problems.join(), /not the installed gate/);
  assert.match(gateIdentityProblems({ ...base, launcherText: "exec node /w/rightkit/packages/release/bin/rightkit-gate.mjs \"$@\"" }).problems.join(), /runs the checkout gate \S+ and no --gate names it/);
  assert.match(gateIdentityProblems({ ...base, installed: null }).problems.join(), /no sealed installed gate/);
  assert.match(gateIdentityProblems({ ...base, launcherExists: false, launcherVersion: null }).problems.join(), /does not exist/);
  assert.match(gateIdentityProblems({ ...base, launcherVersion: null }).problems.join(), /printed no version/);
  const named = gateIdentityProblems({ ...base, gateArg: "/s/checkout-gate.sh", launcherText: "exec node /w/rightkit/packages/release/bin/rightkit-gate.mjs", launcherVersion: "0.6.47" });
  assert.ok(named.ok); assert.equal(named.kind, "named", "named by --gate: accepted, and recorded as not the installed gate");
  assert.equal(gateIdentityProblems({ ...base, gateArg: base.defaultGate }).kind, "installed");
  // findInstalledGate: a sealed folder (named by its manifest's sha256) with the newest version wins; an unsealed one never does.
  const files = new Map(); const mk = (version, sealed = true) => { const manifest = `m${version}\n`; const name = sealed ? sha(manifest) : "f".repeat(64); files.set(`/m/trusted-gates/${name}.manifest.txt`, manifest); files.set(`/m/trusted-gates/${name}/node_modules/@rightkit/release/package.json`, JSON.stringify({ name: "@rightkit/release", version })); return name; };
  mk("0.6.9"); mk("0.6.46"); mk("0.6.99", false);
  const found = findInstalledGate("/m", { list: () => [...new Set([...files.keys()].map((k) => k.split("/")[3]).filter((n) => /^[a-f0-9]{64}$/.test(n)))], read: (f) => { if (!files.has(f)) throw new Error("ENOENT"); return files.get(f); } });
  assert.equal(found.version, "0.6.46");
  assert.equal(findInstalledGate("/nowhere"), null);
});

// ---- the same violations, injected into a whole clean evidence set -------------------------------------------------------------------------------

const T0 = Date.parse("2026-11-05T12:00:00.000Z"); const tsAt = (ms) => new Date(T0 + ms).toISOString();
const ENV = "e".repeat(64); const CORES = 12;
const cleanLog = (stage, { n = 1, lease = true, match = "hit", walks = 0, hashes = 0, resident = 2, lanes = CORES, rulesCached = true, shared = 29 } = {}) => [
  lease && `${tsAt(200)} rightkit-gate: snapshot lock held h${n}`,
  `${tsAt(300)} rightkit-gate: snapshot /s/abc is commit ${"a".repeat(40)} tree ${"b".repeat(40)} (written=0 removed=0 unchanged=77)`,
  `${tsAt(400)} rightkit-gate: --to ${stage} for aaaaaaaaaaaa: ${stage}; executor broker`,
  `${tsAt(500)} ${rulesCached ? "check: aaaaaaaaaaaa app rules: cached pass (tree bbbbbbbbbbbb)" : "check: aaaaaaaaaaaa passed 14 rules (compile tier) in 0.1s"}`,
  stage === "compile" && `${tsAt(550)} rightkit-gate: note: class B batching: 37 compile scan(s) in 12 configuration group(s); each group runs as one invocation (${shared} scan(s) in ${shared ? 4 : 0} shared group(s)), bisected`,
  stage === "compile" && `${tsAt(560)} rightkit-gate: note: class B batching: 4 compile scan(s) stay their own invocation: sidecar-bound`,
  `${tsAt(700)} rightkit-gate: 120 scan(s) for 77 changed file(s) vs aaaaaaaaaaaa; class A scans ${lanes} at a time, class B: the broker queues`,
  `${tsAt(900)} rightkit-gate: snapshot dependencies root=app pnpm=11.25.0 store=/s lockfile=sha256:abc ms=40 reuse-check-ms=20 dependency-digest-checks=1 reuse-setup=ready reuse-match=${match}`,
  `${tsAt(1000)} rightkit-gate: coverage report /r/c${n}.json: 4 planned scan(s), 4 hit, 0 ran, 0 skipped`,
  `${tsAt(1100)} rightkit-gate: class A bytes tracked=a all=a env-at-start=${ENV} env=${ENV}`,
  `${tsAt(1200)} rightkit-gate: snapshot proof: tree-hashes=${hashes} verify-walks=${walks} resident-proofs=${resident}`,
].filter(Boolean).join("\n");
const baseRows = (cov = "hit", over = {}) => map([
  row("rustfmt:2021", cov, { key: '{"id":"rustfmt:2021","files":[["a","1"]]}', ...over["rustfmt:2021"] }), row("secret-scan", cov, { key: '{"id":"secret-scan"}', ...over["secret-scan"] }), row("license", cov, { key: '{"id":"license"}', ...over.license }),
  row("cargo-metadata:rk", cov, { key: '{"id":"cargo-metadata:rk"}', ...over["cargo-metadata:rk"] }), row("cargo-metadata:tools", cov, { key: '{"id":"cargo-metadata:tools"}', ...over["cargo-metadata:tools"] }),
  row("ghsa-audit:rk", cov, { key: '{"day":"2026-11-05","id":"ghsa-audit:rk"}', ...over["ghsa-audit:rk"] }), row("cargo-deny:tools", cov, { key: '{"day":"2026-11-05","id":"cargo-deny:tools"}', ...over["cargo-deny:tools"] }),
  row("tsc:app", cov, { key: `{"id":"tsc:app","env":"${ENV}"}`, ...over["tsc:app"] }), row("pnpm-lockfile:app", cov, { key: `{"id":"pnpm-lockfile:app","env":"${ENV}"}` }),
  rowB("clippy:rk-core", cov, { key: '{"id":"clippy:rk-core"}', ...over["clippy:rk-core"] }),
]);
const cleanLedger = (extra = []) => [led("git", ["rev-parse", "--show-toplevel"], "/w"), led("git", ["rev-parse", "HEAD"], "/w"), led("rustfmt", ["--version"], "/tmp"), led("pnpm", ["--version"], "/s/app"), led("node", ["--version"], "/w"), ...extra];
function cleanCtx() {
  const mkRun = (label, stage, o = {}) => ({ label, stage, exit: 0, ms: stage === "compile" ? 4000 : 3000, startedMs: T0, args: ["--to", stage], log: cleanLog(stage, { n: label.length }), logPath: `/o/${label}.log`, coveragePaths: [`/r/${label}.json`], ledgerPath: `/o/${label}.jsonl`, ledger: { entries: cleanLedger(), problems: [] }, coverageStarts: ["2026-11-05T12:00:00.000Z"], rows: baseRows("hit"), ...o });
  const runs = {};
  for (const [label, stage] of [["S1:check", "check"], ["S1:compile", "compile"], ["S2:check", "check"], ["S2:compile", "compile"], ["S2:check2", "check"], ["S2:compile2", "compile"]]) runs[label] = mkRun(label, stage);
  const editRows = (over) => baseRows("hit", over);
  runs["S3:check"] = mkRun("S3:check", "check", { rows: map([row("rustfmt:2021", "ran", { key: "new-1" }), row("secret-scan", "ran", { key: "new-2" }), row("license", "ran", { key: "new-3" }), ...[...baseRows("hit").values()].filter((r) => !["rustfmt:2021", "secret-scan", "license"].includes(r.id))]) });
  runs["S4:check"] = mkRun("S4:check", "check", { rows: map([row("cargo-metadata:rk", "ran", { key: "new-m" }), ...[...baseRows("hit").values()].filter((r) => r.id !== "cargo-metadata:rk")]), manifestInfo: { workspace: "rk", workspaces: [".", "rk", "tools"], crates: {} } });
  void editRows;
  runs["S10:units"] = mkRun("S10:units", "check", { units: ["cargo:rightkit-a", "cargo:rightkit-b"], log: `${cleanLog("check")}\n${tsAt(1300)} rightkit-gate: [unit:check] class A (compile tier) of this unit list already passed on aaaaaaaaaaaa: not run again` });
  const budgets = { schema: BUDGETS_SCHEMA, hosts: { mac: { warmCheckMs: 5000, warmCompileMs: 6000, stages: { ownership: 1000, snapshotSync: 1000, dependencyStep: 3000, planning: 1000 } }, dell: { warmCheckMs: null, warmCompileMs: null, stages: { ownership: null, snapshotSync: null, dependencyStep: null, planning: null } } } };
  return { runs, wanted: STEPS.map((s) => s.id), host: "mac", budgets, budgetProblem: null, cores: CORES, jobsEnv: undefined, gateIdentity: { ok: true, problems: [], kind: "installed" }, gateIdentityEvidence: "x" };
}
const nnStatus = (ctx) => Object.fromEntries(evaluateNonNegotiables(ctx).map((r) => [r.id, r.status]));
const logOf = (ctx, label, f) => { ctx.runs[label].log = f(ctx.runs[label].log); };
const ledgerAdd = (ctx, label, ...entries) => { ctx.runs[label].ledger.entries.push(...entries); };

test("a clean evidence set passes all 19 non-negotiables", () => {
  const results = evaluateNonNegotiables(cleanCtx());
  assert.equal(results.length, 19);
  for (const r of results) assert.equal(r.status, "pass", `${r.id}: ${r.problems.join(" | ")}`);
});

const MUTATIONS = {
  NN01: (c) => { c.runs["S2:check2"].rows = baseRows("hit", { "rustfmt:2021": { coverage: "ran" } }); },
  NN02: (c) => { c.runs["S2:compile2"].rows = baseRows("hit", { "clippy:rk-core": { coverage: "ran" } }); },
  NN03: (c) => { c.runs["S3:check"].rows.set("cargo-metadata:tools", row("cargo-metadata:tools", "ran", { key: "moved" })); },
  NN04: (c) => { c.runs["S4:check"].rows.set("cargo-metadata:tools", row("cargo-metadata:tools", "ran", { key: "moved" })); },
  NN05: (c) => { c.runs["S2:compile2"].rows = baseRows("hit", { "clippy:rk-core": { key: "drifted" } }); },
  NN06: (c) => { c.runs["S2:check"].rows = baseRows("hit", { "rustfmt:2021": { key: `{"id":"rustfmt:2021","x":"${ENV}"}` } }); },
  NN07: (c) => ledgerAdd(c, "S2:check", led("git", ["rev-parse", "--show-toplevel"], "/w")),
  NN08: (c) => ledgerAdd(c, "S1:check", led("cargo", ["fmt", "--check"], "/s/rk")),
  NN09: (c) => ledgerAdd(c, "S3:check", led("rustright", ["cargo", "metadata", "--no-deps"], "/s/rk")),
  NN10: (c) => ledgerAdd(c, "S2:compile2", led("cargo", ["metadata", "--no-deps"], "/s/rk")),
  NN11: (c) => logOf(c, "S2:check2", (l) => l.replace("verify-walks=0", "verify-walks=2")),
  NN12: (c) => logOf(c, "S2:compile", (l) => l.replace("reuse-match=hit", "reuse-match=record-mismatch")),
  NN13: (c) => logOf(c, "S4:check", (l) => `${tsAt(100)} rightkit-gate: snapshot lock held h9\n${l}`),
  NN14: (c) => logOf(c, "S2:check", (l) => l.replace("app rules: cached pass", "passed 14 rules")),
  NN15: (c) => { c.runs["S2:check2"].rows = baseRows("hit", { "ghsa-audit:rk": { coverage: "ran" } }); },
  NN16: (c) => { c.runs["S10:units"].log = `${c.runs["S10:units"].log}\n${tsAt(1400)} rightkit-gate: coverage report /r/second.json: 1 planned scan(s)`; },
  NN17: (c) => logOf(c, "S2:check", (l) => l.replace(`class A scans ${CORES} at a time`, "class A scans 4 at a time")),
  NN18: (c) => { c.runs["S2:check2"].ms = 5001; },
  NN19: (c) => { c.gateIdentity = { ok: false, problems: ["checkout gate without --gate"], kind: "installed" }; },
};
for (const nn of NN) {
  test(`${nn.id} wiring: ${nn.title} -- fails exactly on its own violation`, () => {
    assert.ok(MUTATIONS[nn.id], `${nn.id} has a mutation`);
    const ctx = cleanCtx(); MUTATIONS[nn.id](ctx);
    const status = nnStatus(ctx);
    assert.equal(status[nn.id], "fail", `${nn.id} must fail on its violation`);
  });
}
test("a violation of one non-negotiable does not fail the unrelated ones", () => {
  for (const [id, mutate] of [["NN13", MUTATIONS.NN13], ["NN19", MUTATIONS.NN19], ["NN18", MUTATIONS.NN18], ["NN04", MUTATIONS.NN04]]) {
    const ctx = cleanCtx(); mutate(ctx);
    const failed = Object.entries(nnStatus(ctx)).filter(([, s]) => s === "fail").map(([k]) => k);
    assert.deepEqual(failed, [id], `${id} alone`);
  }
});

test("missing evidence fails a selected non-negotiable and never leaves it unchecked", () => {
  const noLedger = cleanCtx(); for (const r of Object.values(noLedger.runs)) r.ledger = null;
  const s = nnStatus(noLedger);
  for (const id of ["NN07", "NN08", "NN09", "NN10", "NN12", "NN16", "NN17"]) assert.equal(s[id], "fail", `${id} without a ledger`);
  assert.match(evaluateNonNegotiables(noLedger).find((r) => r.id === "NN07").problems.join(), /does not emit RK_GATE_SPAWN_LEDGER/);
  const noWarm = cleanCtx(); delete noWarm.runs["S2:check2"];
  const w = nnStatus(noWarm); for (const id of ["NN01", "NN05", "NN10", "NN14", "NN15", "NN18"]) assert.equal(w[id], "fail", `${id} without the second warm run`);
  const noUnits = cleanCtx(); delete noUnits.runs["S10:units"]; noUnits.unitsWhy = "the fixture has 0 units";
  assert.match(evaluateNonNegotiables(noUnits).find((r) => r.id === "NN16").problems.join(), /fixture has 0 units/);
  const nullBudget = cleanCtx(); nullBudget.host = "dell";
  const d = evaluateNonNegotiables(nullBudget).find((r) => r.id === "NN18");
  assert.equal(d.status, "fail"); assert.match(d.problems.join(), /no budget measured for dell/);
  const failedRun = cleanCtx(); failedRun.runs["S2:compile2"].exit = 1;
  assert.equal(nnStatus(failedRun).NN02, "fail", "a red compile is no evidence of a quiet Class B");
  assert.equal(nnStatus(failedRun).NN18, "fail");
  assert.equal(nnStatus(failedRun).NN01, "pass", "its Class A rows are still evidence of NN01");
  const noRows = cleanCtx(); noRows.runs["S2:check"].exit = 1; noRows.runs["S2:check"].rows = null;
  assert.equal(nnStatus(noRows).NN01, "fail");
  const noS3 = cleanCtx(); delete noS3.runs["S3:check"]; noS3.s3Why = "S3 made no run: the fixture has no tracked rk/**/src/lib.rs";
  assert.match(evaluateNonNegotiables(noS3).find((r) => r.id === "NN03").problems.join(), /no tracked rk/);
});

test("a subset run evaluates only the non-negotiables whose steps all ran and marks the rest not-evaluated; NN19 is always evaluated", () => {
  const ctx = cleanCtx(); ctx.wanted = ["S2"];
  const s = nnStatus(ctx);
  assert.equal(s.NN01, "pass"); assert.equal(s.NN18, "pass"); assert.equal(s.NN19, "pass");
  for (const id of ["NN03", "NN04", "NN06", "NN07", "NN16"]) assert.equal(s[id], "not-evaluated", id);
  const r = buildResult({ host: "mac", gate: {}, broker: {}, commit: COMMIT, steps: [{ id: "S2", title: "S2", checks: ["pass"], notes: [] }], clauseChecks: { ...allUnchecked(), "GC-A04": [{ status: "pass", evidence: ["r"], problems: [] }] }, startedAt: "a", endedAt: "b", nonNegotiables: evaluateNonNegotiables(ctx) });
  assert.equal(r.ok, false, "NN34: a partial run never passes"); assert.match(r.verdict, /non-negotiables 9 of 19 passed, 10 not evaluated \(steps not selected\); partial run: steps S2 only/);
});

test("the verdict includes the non-negotiables: a failed one blocks, and a full run with one missing or not evaluated is not a pass", () => {
  const nn = (over = {}) => NN_IDS.map((id) => ({ id, status: over[id] ?? "pass", problems: over[id] ? ["x"] : [] }));
  const full = STEPS.map((x) => step(x.id, x.byDesignUnchecked ? [] : ["pass"]));
  const run = (nonNegotiables, steps = full) => rb({ clauseChecks: passing(), steps, nonNegotiables });
  const ok = run(nn());
  assert.equal(ok.ok, true); assert.equal(ok.verdict, "PASS (1 of 67 asserted, 66 unchecked; non-negotiables 19 of 19 passed)");
  const failed = run(nn({ NN07: "fail" }));
  assert.equal(failed.ok, false); assert.match(failed.blockers.join(), /non-negotiable NN07 failed/); assert.match(failed.verdict, /^NOT PASSED .*non-negotiables 18 of 19 passed, 1 failed/);
  assert.match(formatSummary(failed), /NN07 fail: x/);
  assert.match(run(nn({ NN03: "not-evaluated" })).blockers.join(), /NN03 was not evaluated in a full run/);
  assert.match(run(nn().slice(1)).blockers.join(), /NN01 has no result/);
  assert.equal(run(nn({ NN03: "not-evaluated" }), [step("S1", ["pass"])]).ok, false, "a partial run may leave them not-evaluated but never passes (NN34)");
});

test("the plan lists the non-negotiables per step and the S10 unit-list step exists", () => {
  const plan = buildPlan(); const text = formatPlan(plan);
  assert.equal(plan.nonNegotiables.length, 19);
  assert.match(text, /S2  warm rerun, same commit \(twice\)[^\n]*\n[^\n]*\n\s+non-negotiables: NN01 NN02 NN05 NN06 NN07/);
  assert.match(text, /S10  unit list/); assert.match(text, /NN16  steps S10/);
  assert.match(text, /NN19: ?|NN19 \(evaluated in every run\)/);
  assert.deepEqual(nnForStep("S3"), ["NN03", "NN07", "NN08", "NN09", "NN13"]);
  assert.equal(buildPlan({ steps: ["S2"] }).nonNegotiables.find((n) => n.id === "NN03").selected, false);
  assert.deepEqual(unitSelectorsOf(["crates/rightkit-b/Cargo.toml", "packages/pkg/package.json", "crates/rightkit-a/Cargo.toml", "rk/crates/x/Cargo.toml"], 2), ["cargo:rightkit-a", "cargo:rightkit-b"]);
  assert.deepEqual(unitSelectorsOf(["rk/crates/x/Cargo.toml"], 2), []);
});

test("NN36: a budget number changed to null counts as a change; a null that was never measured does not", () => {
  const was = { hosts: { mac: { a: 5, b: null, c: 3 } } };
  assert.equal(risenBudgets(was, { hosts: { mac: { a: null, b: null, c: 3 } } }).length, 1);
  assert.equal(risenBudgets(was, { hosts: { mac: { a: 5, b: null, c: 3 } } }).length, 0);
});
