// Pure tests of the gate-acceptance runner's assertion logic (gate-acceptance.mjs). Nothing here runs the gate, a compile or git.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import {
  CHECKS, CLAUSES, COVERAGE_SCHEMA, INSTALL_CLAUSES, INSTALL_STAGES, NEGATIVES, RECEIPT_SCHEMA, STEPS, assertCleanCheckout, assertManifestEdit, assertNegative, assertOneFileEdit, assertRecovery,
  assertWarmRerun, brokerDigestProblems, brokerUnchanged, buildPlan, buildResult, bypassRefused, cargoWorkspaceOf, checkUtcStamps, classABeforeCompile, compileRanFresh, coveragePaths, dirtyProof,
  exitedBySignal, formatPlan, formatSummary, hookProblems, ladderStages, launchSpec, makeDirtyCheckout, mergeCoverage, outputDirsProblems, qualifyRefusal, reclaimLine, repositoryKey,
  restoreDirtyCheckout, ruleTierProblems, sameListing, skippedRows, snapshotDirProblems, stageTimings, summarizeClause, validateCoverage, validateDoorReceipt, validateRenderReceipt,
  validateSourceIdentity, vectorsProblems, workspaceRootOf, worktreeCount,
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
  const l = reclaimLine("2026-11-05T10:00:00Z rightkit-gate: build ownership held h2 (reclaimed from h1: owner pid 9 is gone; 2 sub-handle(s) REAPED)\n");
  assert.equal(l.handle, "h2"); assert.equal(l.from, "h1"); assert.match(l.proofs, /REAPED/);
  assert.equal(reclaimLine("rightkit-gate: build ownership held h2\n"), null);
});

const COMMIT = "a".repeat(40); const TREE = "b".repeat(40);
const goodReceipt = () => ({ schema: RECEIPT_SCHEMA, commit: COMMIT, app: ".", binding: { tree: TREE, locks: {}, gate: sha("g"), toolchain: {}, executor: "mac" }, stages: {
  check: { ok: true, at: "2026-11-05T10:00:00.000Z", ruleTier: "compile", snapshot: { path: "/s", tree: TREE, intactAfter: true }, ownership: "h" },
  compile: { ok: true, at: "2026-11-05T10:00:05.000Z", outputs: { dirs: ["/o/ws/g/1/target/aarch64-apple-darwin/release"], truncated: false }, snapshot: { path: "/s", tree: TREE, intactAfter: true }, ownership: "h" } } });

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

test("warm rerun: a ran or skipped row on an unchanged commit fails; uncached families must run", () => {
  const prev = map([row("rustfmt:1", "ran"), row("pnpm-lockfile", "ran", { key: null }), row("secret-scan", "ran")]);
  const next = map([row("rustfmt:1", "hit", { key: "k:rustfmt:1" }), row("pnpm-lockfile", "ran", { key: null }), row("secret-scan", "hit", { key: "k:secret-scan" })]);
  assert.ok(assertWarmRerun(prev, next).ok);
  assert.match(assertWarmRerun(prev, map([row("rustfmt:1", "ran"), row("secret-scan", "hit")])).problems.join(), /rustfmt:1: Class A row ran/);
  assert.match(assertWarmRerun(prev, map([row("rustfmt:1", "hit", { key: "other" }), row("secret-scan", "hit")])).problems.join(), /key changed/);
  assert.match(assertWarmRerun(prev, map([row("rustfmt:1", "hit"), row("secret-scan", "skipped", { status: "skipped" })])).problems.join(), /never a pass/);
  assert.match(assertWarmRerun(prev, map([row("rustfmt:1", "hit"), row("pnpm-lockfile", "hit"), row("secret-scan", "hit")])).problems.join(), /uncached family answered hit/);
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

test("recovery: SIGKILL leaves a record, dry run explains, next run reclaims and replaces", () => {
  const before = { handle: "h1", pid: 42 };
  const dry = { exit: 0, text: "recover: recoverable: 2 REAPED" };
  const next = { exit: 0, text: "x rightkit-gate: build ownership held h2 (reclaimed from h1: gone)" };
  assert.ok(assertRecovery({ signal: "SIGKILL", before, dry, next, after: null }).ok);
  assert.match(assertRecovery({ signal: "SIGKILL", before: null, dry, next, after: null }).problems.join(), /no owner record/);
  assert.match(assertRecovery({ signal: "SIGKILL", before, dry: { exit: 2, text: "recover: REFUSED" }, next, after: null }).problems.join(), /dry run/);
  assert.match(assertRecovery({ signal: "SIGKILL", before, dry, next: { exit: 0, text: "no reclaim" }, after: null }).problems.join(), /no 'reclaimed from'/);
  assert.match(assertRecovery({ signal: "SIGKILL", before, dry, next: { exit: 0, text: "build ownership held h1 (reclaimed from h1: x)" }, after: null }).problems.join(), /not replaced/);
  assert.match(assertRecovery({ signal: "SIGKILL", before, dry, next, after: { handle: "h1" } }).problems.join(), /still carries/);
  assert.match(assertRecovery({ signal: "SIGKILL", before, dry, next: { exit: 1, text: next.text }, after: null }).problems.join(), /exited 1/);
  assert.ok(assertRecovery({ signal: "SIGINT", before: null, dry: null, next: { exit: 0, text: "" }, after: null }).ok);
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
const rb = (o) => buildResult({ host: "mac", gate: {}, broker: {}, commit: COMMIT, steps: [], clauseChecks: allUnchecked(), startedAt: "a", endedAt: "b", ...o });
const passing = (extra = {}) => ({ ...allUnchecked(), "GC-A04": [{ status: "pass", evidence: ["r.json"], problems: [] }], ...extra });
const step = (id, checks) => ({ id, title: id, checks, notes: [] });

test("result: ok needs a passing clause, no failing clause, no failing or unfinished step, no failing step-only check", () => {
  const empty = rb({});
  assert.equal(empty.schema, "rightkit.gate-acceptance/1"); assert.equal(empty.clauses.length, 67); assert.equal(empty.totals.pass, 0); assert.equal(empty.ok, false);
  assert.equal(rb({ clauseChecks: passing(), steps: [step("S1", ["pass"])] }).ok, true);
  const bad = rb({ clauseChecks: passing({ "GC-A21": [{ status: "fail", evidence: ["l"], problems: ["p"] }] }) });
  assert.equal(bad.ok, false); assert.equal(bad.totals.fail, 1);
  // old behaviour: ok ignored the steps, so a failed S9 / S0 / S1 check printed PASS
  assert.equal(rb({ clauseChecks: passing(), steps: [step("S9", ["pass", "fail"])] }).ok, false);
  assert.equal(rb({ clauseChecks: passing(), steps: [step("S1", ["pass"]), step("S0", ["fail"])] }).ok, false);
  assert.equal(rb({ clauseChecks: passing(), steps: [step("S3", [])] }).ok, false);
  assert.match(rb({ clauseChecks: passing(), steps: [step("S3", [])] }).blockers.join(), /step S3 did not run as specified/);
  assert.equal(rb({ clauseChecks: passing(), steps: [step("S5", [])] }).ok, true, "S5 is unchecked by design");
  assert.equal(rb({ clauseChecks: passing(), steps: [step("S1", ["pass"])], stepChecks: [{ step: "S1", name: "qualify refusal", ok: false, problems: ["x"] }] }).ok, false);
  assert.equal(rb({ clauseChecks: passing(), steps: [step("S1", ["pass", "unchecked"])] }).ok, false);
});

test("verdict text: PASS always carries the asserted and unchecked counts, never a bare PASS", () => {
  const ok = rb({ clauseChecks: passing(), steps: [step("S1", ["pass"])] });
  assert.equal(ok.verdict, "PASS (1 of 67 asserted, 66 unchecked; partial run: steps S1 only)");
  assert.match(formatSummary(ok), /^gate acceptance PASS \(1 of 67 asserted, 66 unchecked/);
  for (const text of [formatSummary(ok), formatSummary(rb({}))]) assert.doesNotMatch(text, /(?<!NOT )PASS(?! \()/);
  const full = rb({ clauseChecks: passing(), steps: STEPS.map((x) => step(x.id, x.byDesignUnchecked ? [] : ["pass"])) });
  assert.equal(full.verdict, "PASS (1 of 67 asserted, 66 unchecked)");
  const bad = rb({ clauseChecks: passing({ "GC-A21": [{ status: "fail", evidence: [], problems: ["p"] }] }) });
  assert.match(bad.verdict, /^NOT PASSED \(2 of 67 asserted, 65 unchecked; 1 failed clause\(s\)/);
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
const SCRATCH = "/Volumes/D/scratch/gate-acceptance";
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
