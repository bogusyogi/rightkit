// rightkit#1301, one model (rustright#190): every compile of a snapshot stage is a carried request or nested under one. A pnpm, node or QA
// step of a snapshot stage is a carried ADAPTER job (carried-adapter-run.mjs, snapshot-broker.mjs carryBrokerAdapter); the Tauri CLI's
// Cargo calls run NESTED under it; the broker signs the list of those children and the gate verifies it (verifyBrokerLedger) and reads the
// bundle root from it (carriedOutputDir). The broker is a FIXTURE (test-support/fake-broker.mjs) that models the real adapter job: the broker's
// own PATH and environment (RUSTRIGHT_PARENT_REQUEST_ID set, every other variable dropped), so a wrapper on the caller's PATH or a grant in its
// environment would not reach the job's children, as they would not for real. No cargo, no real broker; the shape of the signed
// `nestedChildren` list is RustRight's final one (rustright#190 candidate db6cefd9): signed in snapshotEvidence, mirrored on the receipt.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { buildLaunch, snapshotInstallSkip } from "./build-launch.mjs";
import { readLedger, UNSCOPED } from "./cargo-ledger.mjs";
import { CARRIED_CARGO_MAP_ENV, carriedOutputDir, installCarrier, runCarriedAdapter } from "./carried-cargo.mjs";
import { carriedCallNonce, qualificationNonce } from "./provenance-verify.mjs";
import { runCargoSync } from "./managed-cargo.mjs";
import { runRecipeBuild } from "./recipe-build.mjs";
import { brokerSection, verifyBrokerLedger } from "./snapshot-broker.mjs";
import { verifyNestedChildren } from "./snapshot-request.mjs";
import { spawnSync } from "./background-process.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const POSIX = process.platform === "win32" ? "the fixture client and adapter PATH are POSIX only" : false;
const GATE = "/gate/bin/rightkit-gate.mjs";
const HERE = path.dirname(fileURLToPath(import.meta.url));
const ADAPTER_RUN = path.join(HERE, "carried-adapter-run.mjs");

/** A fixture broker host with a Tauri app (`native`, its own Cargo workspace) and the unit `packages/release` in both trees, the carrier installed. */
function host(t) {
  const fx = makeFakeBroker(t, { readOnlySnapshots: true });
  for (const root of [fx.checkout, fx.snapshot]) {
    fs.mkdirSync(path.join(root, "packages", "release"), { recursive: true });
    fs.writeFileSync(path.join(root, "packages", "release", "package.json"), "{}\n");
  }
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout, native: path.join(fx.checkout, "native") }, section: brokerSection(fx.sb), platform: process.platform, env: {} });
  t.after(() => installed.cleanup());
  const env = { PATH: process.env.PATH, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE, ...installed.env };
  const adapter = (name, args, { cwd = fx.snapshot, over = {}, stdio = "capture" } = {}) => runCarriedAdapter(name, args, { cwd, env: { ...env, ...over }, stdio }, { map: JSON.parse(fs.readFileSync(installed.env[CARRIED_CARGO_MAP_ENV], "utf8")), err: () => {} });
  const entries = () => readLedger(installed.ledgerDir, UNSCOPED);
  return { fx, installed, env, adapter, entries };
}
/** What `pnpm exec tauri build` does under the job: one nested compile in the app's directory. */
const TAURI = { adapter: { children: [{ argv: ["build", "--locked", "--profile", "dev"], cwd: "native" }] } };
const TAURI_ARGS = ["exec", "tauri", "build", "--debug"];

test("a Tauri build under a carried adapter: the child's compile is nested, the gate's own verification accepts the signed list, and the bundle root is the nested child's output directory", { skip: POSIX }, async (t) => {
  const h = host(t);
  h.fx.setBehavior(TAURI);
  const r = h.adapter("rightkit-pnpm", TAURI_ARGS);
  assert.equal(r.status, 0, r.stderr);
  // One carried request: the adapter job, run in the checkout counterpart of the snapshot directory the step ran from.
  const jobs = h.fx.adapterCalls();
  assert.equal(jobs.length, 1);
  assert.deepEqual([jobs[0].adapter, jobs[0].argv, jobs[0].cwd], ["rightkit-pnpm", TAURI_ARGS, h.fx.checkout]);
  assert.equal(jobs[0].jobCwd, h.fx.snapshot, "the job itself runs in the snapshot");
  // The broker's environment, not the caller's: the broker's PATH, the parent request id and capability token, and none of the caller's variables.
  assert.equal(jobs[0].jobPath, h.fx.adapterBin);
  assert.deepEqual(jobs[0].jobEnvNames, ["HOME", "PATH", "RUSTRIGHT_PARENT_CAPABILITY_TOKEN", "RUSTRIGHT_PARENT_REQUEST_ID"]);
  assert.ok(jobs[0].callerEnvNamesDropped.includes(CARRIED_CARGO_MAP_ENV), "the carrier map does not reach the job's children");
  // The compile was started by the job's child and nested under it (the broker's `cargo` on the job's PATH), in the snapshot.
  const nested = JSON.parse(fs.readFileSync(path.join(h.fx.state, "db", `${jobs[0].id}.children.jsonl`), "utf8").trim());
  assert.deepEqual([nested.argv, nested.cwd, nested.exitCode], [["build", "--locked", "--profile", "dev"], path.join(h.fx.snapshot, "native"), 0]);
  assert.deepEqual(r.nestedChildren, [nested]);
  assert.equal(r.outputDir, h.fx.outputDir, "the job's own output directory is the receipt's");
  const { entries, problems } = h.entries();
  assert.deepEqual(problems, []);
  assert.deepEqual(entries.map((e) => [e.status, e.shim, e.adapter, e.requestId]), [["verified", "adapter", "rightkit-pnpm", jobs[0].id]]);
  assert.deepEqual(await h.installed.problems(h.fx.sb), [], "the gate verifies the receipt and its nested children again itself");
  // The stage's output directories are the signed ones, and the bundle root comes from the nested child's, never from a directory the child reported.
  const childOutput = path.join(h.fx.outputDir, "nested", "native");
  assert.deepEqual(h.installed.outputDirs().sort(), [h.fx.outputDir, childOutput].sort());
  assert.deepEqual(h.installed.outputsByWorkspace(), [[path.join(h.fx.checkout, "native"), childOutput]]);
  assert.equal(carriedOutputDir(h.env, path.join(h.fx.snapshot, "native", "Cargo.toml")), childOutput);
  assert.equal(carriedOutputDir(h.env, path.join(h.fx.snapshot, "crates", "a", "Cargo.toml")), null, "another workspace borrows nothing");
  // A carried Cargo call of the same workspace made AFTER the build names the broker's own target: the bundle root (preferNested) still follows
  // the compile the Tauri CLI made, whichever call came last.
  const manifest = path.join(h.fx.snapshot, "native", "Cargo.toml");
  assert.equal(runCargoSync(["build", "--locked"], { cwd: path.join(h.fx.snapshot, "native"), env: h.env, encoding: "utf8" }, { platform: "darwin" }).status, 0);
  assert.equal(carriedOutputDir(h.env, manifest), h.fx.outputDir, "the last verified call of the workspace");
  assert.equal(carriedOutputDir(h.env, manifest, { preferNested: true }), childOutput, "the Tauri CLI's own compile");
});

test("the old assumptions fail as they would for real: a wrapper first on the caller's PATH, a stage grant and the carrier map are all dropped by the broker, so the child's compile can only be the nested one", { skip: POSIX }, (t) => {
  const h = host(t);
  h.fx.setBehavior(TAURI);
  const wrapperDir = fs.mkdtempSync(path.join(h.fx.root, "wrapper-"));
  fs.writeFileSync(path.join(wrapperDir, "cargo"), "#!/bin/sh\necho wrapper >> \"$0.ran\"\nexit 0\n", { mode: 0o755 });
  const r = h.adapter("rightkit-pnpm", TAURI_ARGS, { over: { PATH: `${wrapperDir}:${process.env.PATH}`, RUSTRIGHT_SNAPSHOT_GRANT: '{"path":"/x","sha256":"x"}', RIGHTKIT_STAGE_VARIABLE: "1", CARGO: path.join(wrapperDir, "cargo") } });
  assert.equal(r.status, 0, r.stderr);
  assert.equal(fs.existsSync(path.join(wrapperDir, "cargo.ran")), false, "the caller's wrapper never ran");
  const [job] = h.fx.adapterCalls();
  assert.ok(job.callerEnvNamesDropped.includes("RIGHTKIT_STAGE_VARIABLE"), "a variable the broker does not register is dropped");
  assert.ok(![...job.jobEnvNames, ...job.callerEnvNamesDropped].includes("RUSTRIGHT_SNAPSHOT_GRANT"), "a stage grant is never forwarded to the client at all");
  assert.equal(r.nestedChildren.length, 1, "the compile was the broker's nested request");
  // And a compile with no parent (a child that reached the broker around its job) is a root request, which has no carrier: it fails.
  const orphan = spawnSync(path.join(h.fx.adapterBin, "cargo"), ["build"], { cwd: path.join(h.fx.snapshot, "native"), env: { PATH: process.env.PATH }, encoding: "utf8" });
  assert.notEqual(orphan.status, 0);
  assert.match(orphan.stderr, /RK_SNAPSHOT_CARRIER_REQUIRED/);
});

test("an unverified child fails the stage: a signed child that exited non-zero or with no exit code, an UNFINISHED child, another parent, no list signed or mirrored, a mirror that differs, a list that is not exactly the shape, a repeated child, and more than the cap", { skip: POSIX }, async (t) => {
  for (const [tamper, pattern] of [
    ["childFailed", /exited 1, not 0/], ["childNullExit", /exited with no exit code, not 0/], ["childUnfinished", /UNFINISHED/], ["childOtherParent", /names parent .*neither this job/],
    ["noNestedChildren", /carries no nestedChildren list/], ["nestedChildrenNotSigned", /carries no nestedChildren list/], ["nestedChildrenNotMirrored", /receipt carries no nestedChildren list/],
    ["nestedCopyDiffers", /receipt nestedChildren differs from the signed list/], ["childExtraField", /is not exactly/], ["childDuplicated", /has no request id of its own/],
  ]) {
    const h = host(t);
    h.fx.setBehavior({ ...TAURI, tamper });
    const r = h.adapter("rightkit-pnpm", TAURI_ARGS);
    assert.notEqual(r.status, 0, tamper);
    assert.match(r.stderr, new RegExp(`RK_ADAPTER_BROKER_UNVERIFIED.*${pattern.source}`), tamper);
    const { entries, problems } = h.entries();
    assert.equal(entries[0].status, "unverified", tamper);
    assert.ok(problems.length >= 1, `${tamper}: the stage's ledger reports the call`);
    assert.equal(carriedOutputDir(h.env, path.join(h.fx.snapshot, "native", "Cargo.toml")), null, `${tamper}: an unverified call names no output directory`);
  }
  // The child itself failing (exit 3) while the job swallowed it: the signed list says so, and the call is unverified though the job exited 0.
  const h = host(t);
  h.fx.setBehavior({ adapter: { ...TAURI.adapter, swallow: true, childExit: 3 } });
  const r = h.adapter("rightkit-pnpm", TAURI_ARGS);
  assert.notEqual(r.status, 0);
  assert.match(r.stderr, /exited 3, not 0/);
  assert.ok(h.entries().problems.length >= 1);
});

test("a child whose cwd is outside the snapshot, or whose output directory is inside the snapshot, relative or not a managed generation target, is refused; a refused child is not listed, so the job's own failure is what fails the call", { skip: POSIX }, (t) => {
  for (const [tamper, pattern] of [
    ["childOutsideSnapshot", /outside the snapshot/], ["childOutputInsideSnapshot", /is inside the snapshot/], ["childOutputUnmanaged", /not inside a managed generation target/], ["childOutputRelative", /neither null nor absolute/],
  ]) {
    const h = host(t);
    h.fx.setBehavior({ ...TAURI, tamper });
    const r = h.adapter("rightkit-pnpm", TAURI_ARGS);
    assert.notEqual(r.status, 0, tamper);
    assert.match(r.stderr, pattern, tamper);
    assert.equal(h.entries().entries[0].status, "unverified", tamper);
  }
  // The broker's own nested containment (rustright#190 D7): a compile outside the snapshot is refused and is NOT in the list; the job fails.
  const h = host(t);
  h.fx.setBehavior({ adapter: { children: [{ argv: ["build", "--locked"], cwd: path.relative(h.fx.snapshot, h.fx.checkout) }] } });
  const r = h.adapter("rightkit-pnpm", TAURI_ARGS);
  assert.notEqual(r.status, 0, "the job's own failure is what fails the step");
  assert.deepEqual(h.fx.adapterCalls().length, 1);
  assert.equal(fs.existsSync(path.join(h.fx.state, "db", `${h.fx.adapterCalls()[0].id}.children.jsonl`)), false, "the refused child is not listed");
  // The cap is a property of the list: more than 8192 entries are refused (checked on the verifier; a list that size does not fit a status line).
  const child = { requestId: "c0", parentRequestId: "j", shim: "cargo", argv: ["build"], cwd: path.join(h.fx.snapshot, "native"), exitCode: 0, status: "SUCCEEDED", outputDir: null };
  const big = Array.from({ length: 8193 }, (_, i) => ({ ...child, requestId: `c${i}` }));
  const opts = { snapshotPath: h.fx.snapshot, checkout: h.fx.checkout, outputRoots: [h.fx.wsRoot], jobId: "j" };
  assert.equal(verifyNestedChildren({ nestedChildren: big.slice(0, 8192) }, { nestedChildren: big.slice(0, 8192) }, opts).length, 8192);
  assert.throws(() => verifyNestedChildren({ nestedChildren: big }, { nestedChildren: big }, opts), /more than the cap 8192/);
  // Depth >= 2 (rustright#190 ad9ecb37): a child's parent may be an earlier listed child, never a later or unknown one, never itself.
  const deep = [child, { ...child, requestId: "c1", parentRequestId: "c0" }];
  assert.equal(verifyNestedChildren({ nestedChildren: deep }, { nestedChildren: deep }, opts).length, 2);
  for (const bad of [[{ ...child, requestId: "c1", parentRequestId: "c2" }, { ...child, requestId: "c2" }], [{ ...child, parentRequestId: "c0" }], [{ ...child, parentRequestId: "x" }]]) {
    assert.throws(() => verifyNestedChildren({ nestedChildren: bad }, { nestedChildren: bad }, opts), /names parent .*neither this job/);
  }
});

test("an entry that compiles nothing has a null outputDir: it is verified, and never names a bundle root or a stage output directory", { skip: POSIX }, async (t) => {
  const h = host(t);
  h.fx.setBehavior({ adapter: { children: [{ argv: ["metadata", "--format-version", "1", "--no-deps"], cwd: "native" }] } });
  const r = h.adapter("rightkit-pnpm", TAURI_ARGS);
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(r.nestedChildren.map((c) => [c.shim, c.argv[0], c.outputDir, c.status, c.parentRequestId === h.fx.adapterCalls()[0].id]), [["cargo", "metadata", null, "SUCCEEDED", true]]);
  assert.deepEqual(await h.installed.problems(h.fx.sb), []);
  assert.deepEqual(h.installed.outputDirs(), [h.fx.outputDir], "only the job's own output directory");
  assert.deepEqual(h.installed.outputsByWorkspace(), []);
  assert.equal(carriedOutputDir(h.env, path.join(h.fx.snapshot, "native", "Cargo.toml"), { preferNested: true }), null, "no bundle root from a child with a null outputDir");
});

test("the workspace root is the primary toplevel and the cwds are exactly these: the runner runs in the snapshot step directory, the client in the primary counterpart that the carrier names (a per-call carrier, never a grant), the job in the snapshot", { skip: POSIX }, (t) => {
  const h = host(t);
  h.fx.setBehavior({ adapter: { children: [{ argv: ["build", "--locked"], cwd: "." }] } });
  const stepDir = path.join(h.fx.snapshot, "native");
  const r = spawnSync(process.execPath, [ADAPTER_RUN, "rightkit-pnpm", "--", ...TAURI_ARGS], { cwd: stepDir, env: h.env, encoding: "utf8" });
  assert.equal(r.status, 0, r.stderr);
  const [job] = h.fx.adapterCalls();
  assert.equal(job.cwd, path.join(h.fx.checkout, "native"), "the client's cwd is the primary counterpart of the runner's (snapshot) cwd");
  assert.equal(job.primaryCwd, job.cwd, "and it is what the carrier names");
  assert.equal(job.jobCwd, stepDir, "the broker runs the job in the snapshot counterpart");
  assert.equal(job.workspace, h.fx.checkout, "the request's workspace is the primary toplevel, not the step's directory");
  assert.equal(h.entries().entries[0].status, "verified");
  const ledger = h.entries().entries[0];
  assert.deepEqual([ledger.cwd, ledger.checkoutCwd, ledger.workspace], [stepDir, job.cwd, h.fx.checkout]);
  // A request whose workspace is not the primary toplevel (the old cwd-as-workspace answer) is unverified.
  h.fx.setBehavior({ workspaceIsCwd: true });
  const bad = h.adapter("rightkit-pnpm", ["run", "x"], { cwd: stepDir });
  assert.notEqual(bad.status, 0);
  assert.match(bad.stderr, /not the primary toplevel/);
});

test("the gate re-verifies the ledger's nested children against the signed list: a ledger record that names other children, another cwd, or a replayed request is a problem", { skip: POSIX }, async (t) => {
  const h = host(t);
  h.fx.setBehavior(TAURI);
  assert.equal(h.adapter("rightkit-pnpm", TAURI_ARGS).status, 0);
  const { entries } = h.entries();
  const forge = (over) => verifyBrokerLedger([{ ...entries[0], ...over }], h.fx.sb);
  assert.deepEqual(await forge({}), []);
  assert.match((await forge({ nestedChildren: [] }))[0], /not the ones the broker's receipt signs/);
  assert.match((await forge({ nestedChildren: [{ ...entries[0].nestedChildren[0], outputDir: path.join(h.fx.outputDir, "nested", "forged") }] }))[0], /not the ones the broker's receipt signs/);
  assert.match((await forge({ cwd: path.join(h.fx.snapshot, "native") }))[0], /not the checkout counterpart/);
  assert.match((await forge({ workspace: path.join(h.fx.checkout, "native") }))[0], /not the primary toplevel/);
  assert.match((await forge({ cwd: h.fx.checkout }))[0], /outside the snapshot/);
  const seen = { nonces: new Set(), ids: new Set() };
  assert.deepEqual(await verifyBrokerLedger([entries[0]], h.fx.sb, { seen }), []);
  assert.match((await verifyBrokerLedger([entries[0]], h.fx.sb, { seen }))[0], /used twice/);
});

test("pack from a relative directory works: the pack's --dir is the unit's directory of the snapshot and only --pack-destination is outside it; an absolute --dir is refused and the call is unverified", { skip: POSIX }, (t) => {
  const h = host(t);
  const scratch = fs.mkdtempSync(path.join(h.fx.scratchRoot, "rk-local-publish-"));
  const out = path.join(scratch, "pack"); fs.mkdirSync(out);
  const ok = h.adapter("rightkit-pnpm", ["--dir", "packages/release", "pack", "--pack-destination", out]);
  assert.equal(ok.status, 0, ok.stderr);
  assert.deepEqual(h.fx.adapterCalls().map((c) => c.argv), [["--dir", "packages/release", "pack", "--pack-destination", out]]);
  assert.equal(h.entries().entries.at(-1).status, "verified");
  // An absolute --dir (the old scratch copy, or the snapshot's own absolute path) is refused by the broker's snapshot-mode grammar.
  for (const dir of [path.join(scratch, "package-source"), path.join(h.fx.snapshot, "packages", "release")]) {
    const out2 = path.join(scratch, `pack-${path.basename(dir)}`); fs.mkdirSync(out2);
    const bad = h.adapter("rightkit-pnpm", ["--dir", dir, "pack", "--pack-destination", out2]);
    assert.notEqual(bad.status, 0, dir);
    assert.match(bad.stderr, /RK_ADAPTER_ARGV_DENIED.*absolute: a snapshot call names its directory relative/, dir);
    assert.equal(h.entries().entries.at(-1).status, "unverified", dir);
  }
  assert.equal(h.fx.adapterCalls().length, 1, "no refused call became a job");
  // `install` is refused by the broker in a snapshot call (snapshotSubcommands exec, run, pack).
  const install = h.adapter("rightkit-pnpm", ["install", "--frozen-lockfile"]);
  assert.notEqual(install.status, 0);
  assert.match(install.stderr, /RK_ADAPTER_ARGV_DENIED.*install/);
});

test("a call from inside a broker job (a parent request in the environment), without a calling gate or from outside the snapshot is refused before anything runs", { skip: POSIX }, (t) => {
  const h = host(t);
  for (const [over, code] of [[{ RUSTRIGHT_PARENT_REQUEST_ID: "x" }, "RK_ADAPTER_BROKER_INHERITED_ENV"], [{ RUSTRIGHT_PARENT_CAPABILITY_TOKEN: "x" }, "RK_ADAPTER_BROKER_INHERITED_ENV"], [{ RUSTRIGHT_ATTEST_NONCE: "a".repeat(64) }, "RK_ADAPTER_BROKER_INHERITED_ENV"], [{ RUSTRIGHT_CALLING_GATE: "" }, "RK_ADAPTER_BROKER_NO_GATE"]]) {
    const r = h.adapter("rightkit-pnpm", ["run", "x"], { over });
    assert.equal(r.status, 101, code); assert.equal(r.refusal.code, code);
  }
  const outside = h.adapter("rightkit-pnpm", ["run", "x"], { cwd: h.fx.checkout });
  assert.equal(outside.refusal.code, "RK_ADAPTER_BROKER_CWD");
  assert.equal(h.adapter("Evil Adapter", ["run", "x"]).refusal.code, "RK_ADAPTER_BROKER_ADAPTER");
  assert.deepEqual(h.fx.adapterCalls(), [], "nothing reached the broker");
  assert.ok(h.entries().entries.every((e) => e.status === "refused"));
});

test("under a release-bound stage the adapter's nonce is derived from the stage nonce, its directory and its argv (the admission recomputes it), and the stage nonce never reaches the client; otherwise it is fresh", { skip: POSIX }, async (t) => {
  const h = host(t);
  const commit = "a".repeat(40), tree = "b".repeat(40);
  const stageNonce = qualificationNonce({ commit, tree, unit: "npm:@rightkit/release", stage: "compile" });
  const args = ["-r", "--workspace-concurrency=1", "run", "build"];
  assert.equal(h.adapter("rightkit-pnpm", args, { over: { RIGHTKIT_ATTEST_STAGE_NONCE: stageNonce } }).status, 0);
  const [job] = h.fx.adapterCalls();
  const receipt = await h.fx.sb.receiptOf(job.id);
  assert.equal(receipt.provenance.payload.nonce, carriedCallNonce({ stageNonce, cwd: ".", argv: args }));
  assert.notEqual(receipt.provenance.payload.nonce, stageNonce);
  assert.equal(job.stageNonceSeen, false);
  assert.equal(h.adapter("rightkit-pnpm", args).status, 0);
  const fresh = await h.fx.sb.receiptOf(h.fx.adapterCalls()[1].id);
  assert.notEqual(fresh.provenance.payload.nonce, carriedCallNonce({ stageNonce, cwd: ".", argv: args }));
  assert.deepEqual(await h.installed.problems(h.fx.sb), []);
});

test("build-launch: a snapshot stage's pnpm, node and QA steps are carried adapter jobs, an install step is skipped with a log line, and nothing changes without the snapshot route", () => {
  const CLIENT = "/managed/agent-bin/rustright";
  const base = { platform: "darwin", exists: (p) => p === CLIENT, which: (c) => (c === "rustright" ? [CLIENT] : []) };
  const snapshot = { ...base, env: { PATH: "/managed/agent-bin", RK_GATE_CHECKOUT: "/co" } };
  for (const args of [["install"], ["install", "--frozen-lockfile"], ["--dir", "hub", "install", "--frozen-lockfile"], ["-C", "hub", "i"]]) {
    assert.deepEqual(buildLaunch("pnpm", args, snapshot), { command: process.execPath, args: [ADAPTER_RUN, "--skip-install", ...args] }, args.join(" "));
    assert.ok(snapshotInstallSkip("pnpm", args, snapshot.env));
  }
  for (const args of [["run", "build"], ["--dir", "install", "run", "x"], ["exec", "install"], ["-r", "--workspace-concurrency=1", "run", "build"], ["pack"]]) assert.equal(snapshotInstallSkip("pnpm", args, snapshot.env), null, args.join(" "));
  // Not on the snapshot route (no map, no split): an install is the client's adapter job as before.
  assert.deepEqual(buildLaunch("pnpm", ["install"], { ...base, env: { PATH: "/managed/agent-bin" } }), { command: CLIENT, args: ["run", "rightkit-pnpm", "--", "install"] });
  // The skip is a log line and exit 0, running nothing.
  const skipped = spawnSync(process.execPath, [ADAPTER_RUN, "--skip-install", "--dir", "hub", "install", "--frozen-lockfile"], { encoding: "utf8" });
  assert.equal(skipped.status, 0);
  assert.match(skipped.stderr, /snapshot route: skipped `pnpm --dir hub install --frozen-lockfile`: the gate already installed/);
  // A direct (Linux) host's snapshot stage skips it too: the gate installed there as well.
  assert.deepEqual(buildLaunch("pnpm", ["install"], { env: { PATH: "/usr/bin", RK_GATE_CHECKOUT: "/co" }, platform: "linux", exists: () => false, which: () => [] }).args.slice(0, 2), [ADAPTER_RUN, "--skip-install"]);
});

test("the release stage goes through the same door: a recipe's pnpm, node and cargo steps are carried adapter jobs in a snapshot stage, its install step is skipped, and the door's own right-release stays in process", { skip: POSIX }, (t) => {
  const h = host(t);
  const calls = [];
  const spawn = (command, args, options) => { calls.push({ command, args, cwd: options.cwd, env: options.env }); return { status: 0 }; };
  const door = (cmd, args, host2) => buildLaunch(cmd, args, { ...host2, exists: (p) => p === "/managed/agent-bin/rustright", which: (c) => (c === "rustright" ? ["/managed/agent-bin/rustright"] : []) });
  const app = path.join(h.fx.snapshot, "native");
  fs.mkdirSync(path.join(app, "hub"), { recursive: true });
  const plan = { version: 1, steps: [
    { phase: "prepare", cmd: "pnpm", args: ["--dir", "hub", "install", "--frozen-lockfile"] },
    { phase: "prepare", cmd: "node", args: ["scripts/stage.mjs"] },
    { phase: "prepare", cmd: "right-release", args: ["stage-sidecar"] },
    { phase: "build", cmd: "pnpm", args: ["exec", "tauri", "build"], cwd: "." },
  ] };
  runRecipeBuild(plan, { appRoot: app, env: { ...h.env, PATH: "/managed/agent-bin:/usr/bin" }, platform: "darwin", spawnSync: spawn, door });
  assert.deepEqual(calls.map((c) => [path.basename(c.args[0]), ...c.args.slice(1, 3)]), [
    ["carried-adapter-run.mjs", "--skip-install", "--dir"],
    ["carried-adapter-run.mjs", "rightkit-node", "--"],
    ["right-release.mjs", "stage-sidecar"],
    ["carried-adapter-run.mjs", "rightkit-pnpm", "--"],
  ]);
  assert.ok(calls.every((c) => c.command === process.execPath));
  assert.deepEqual(calls[3].args.slice(3, 6), ["exec", "tauri", "build"], "the Tauri step is the adapter job; its compile is nested under it");
});
