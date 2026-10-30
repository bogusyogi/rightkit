// rightkit#1301: right-release's own Cargo calls on a broker host are carried (carried-cargo.mjs, the seam in managed-cargo.mjs
// `runCargoSync`). The broker is a FIXTURE: a fake `rustright` client that applies the reader's carrier rules (rustright-cli shims.rs
// snapshot_carrier: descriptor at most 4096 bytes, a regular file directly in <control root>/snapshot-requests of at most 4 MiB with exactly
// the carrier's fields, cargo only, no parent request, no admission-check call, a calling gate, an attested call never rewritten) and
// serves `rustright status <id>` with receipts signed in-test (test-support/fake-broker.mjs). No cargo, no real broker.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { appendLedger, readLedger, SCAN_TOKEN_ENV, UNSCOPED } from "./cargo-ledger.mjs";
import { CARRIED_CARGO_MAP_ENV, CARRIER_MISSING_CODE, carriedCargoMapOf, installCarrier, runCarriedCargo } from "./carried-cargo.mjs";
import { runCargoSync, runCargoAsync } from "./managed-cargo.mjs";
import { brokerSection, inheritedCarrierEnv } from "./snapshot-broker.mjs";
import * as requestModule from "./snapshot-request.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const POSIX = process.platform === "win32" ? "the fixture wrapper is POSIX only: a Windows child finds only an .exe" : false;
const GATE = "/gate/bin/rightkit-gate.mjs";
/** The platform the seam is asked about: a broker host, whatever machine runs the test. */
const MAC = { platform: "darwin" };

/** A fixture broker host with the stage's carrier installed (the broker-mode wrapper and the map), and a runner of seam calls. */
function host(t, { inputs = null, platform = process.platform, readOnlySnapshots = false } = {}) {
  const fx = makeFakeBroker(t, { inputs, readOnlySnapshots });
  const checkoutDirs = { ".": fx.checkout, native: path.join(fx.checkout, "native") };
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs, section: brokerSection(fx.sb), platform, env: {} });
  t.after(() => installed.cleanup());
  const clean = { PATH: process.env.PATH, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE };
  const env = { ...clean, ...installed.env };
  const cargo = (args, { cwd = path.join(fx.snapshot, "crates", "a"), over = {}, options = {} } = {}) => runCargoSync(args, { cwd, env: { ...env, ...over }, encoding: "utf8", ...options }, { ...MAC });
  const entries = () => readLedger(installed.ledgerDir, UNSCOPED);
  return { constants: requestModule, fx, installed, env, cargo, entries, a: path.join(fx.snapshot, "crates", "a"), native: path.join(fx.snapshot, "native"), requests: path.join(fx.controlRoot, "snapshot-requests") };
}

test("the seam: a stage's call is one carried request, from the call's directory, with its own nonce, the signed receipt verified and ledgered, and its output directory is the receipt's", { skip: POSIX }, async (t) => {
  const h = host(t);
  const r = h.cargo(["build", "--locked", "-p", "a"]);
  assert.equal(r.status, 0, r.stderr);
  assert.equal(r.outputDir, h.fx.outputDir, "the output directory is the receipt's");
  const calls = h.fx.calls();
  assert.deepEqual(calls.map((c) => c.argv), [["build", "--locked", "-p", "a"]]);
  assert.deepEqual(calls.map((c) => c.cwd), [path.join(h.fx.checkout, "crates", "a")], "the client ran in the checkout counterpart of the call's directory, never in the snapshot");
  assert.deepEqual(calls[0].env, { CARGO_TARGET_DIR: null, CARGO_BUILD_TARGET_DIR: null }, "no output directory reaches the broker");
  const { entries, problems } = h.entries();
  assert.deepEqual(problems, []);
  assert.deepEqual(entries.map((e) => [e.status, e.mode, e.outputDir]), [["verified", "broker", h.fx.outputDir]]);
  assert.match(entries[0].nonce, /^[0-9a-f]{64}$/);
  assert.deepEqual(await h.installed.problems(h.fx.sb), [], "the gate verifies the receipt again itself");
  assert.deepEqual(h.installed.outputDirs(), [h.fx.outputDir]);
  assert.deepEqual(fs.readdirSync(h.requests), [], "the carrier is removed after the call");
  // A second call has a fresh nonce and its own request.
  h.cargo(["check"], { cwd: h.native });
  const second = h.entries().entries;
  assert.equal(new Set(second.map((e) => e.nonce)).size, 2); assert.equal(new Set(second.map((e) => e.requestId)).size, 2);
  assert.deepEqual(await h.installed.problems(h.fx.sb), []);
});

test("the seam: --manifest-path becomes the working directory (the client refuses to rewrite an attested call), and cargo's output is captured or inherited as asked", { skip: POSIX }, (t) => {
  const h = host(t, { readOnlySnapshots: true }); // `metadata` is read-only: the post-fix broker (rightkit#1301 F1)
  h.fx.setBehavior({ stdout: '{"target_directory":"/not/the/receipts"}\n' });
  const r = h.cargo(["metadata", "--format-version", "1", "--no-deps", "--manifest-path", path.join(h.native, "Cargo.toml")], { cwd: h.fx.snapshot });
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(h.fx.calls().map((c) => [c.argv, c.cwd]), [[["metadata", "--format-version", "1", "--no-deps"], path.join(h.fx.checkout, "native")]]);
  assert.equal(r.stdout, '{"target_directory":"/not/the/receipts"}\n', "cargo's stdout is captured for a caller that reads it");
  assert.equal(r.outputDir, h.fx.outputDir, "and the output directory is still the receipt's, whatever cargo printed");
  const eq = h.cargo([`--manifest-path=${path.join(h.native, "Cargo.toml")}`, "metadata", "--no-deps"], { cwd: h.fx.snapshot });
  assert.equal(eq.status, 0, eq.stderr);
  assert.deepEqual(h.fx.calls().at(-1).argv, ["metadata", "--no-deps"]);
  const inherit = h.cargo(["check"], { options: { stdio: "inherit" } });
  assert.equal(inherit.status, 0); assert.equal(inherit.stdout, "");
});

test("negative: a receipt for another argv (tampered), an unverified receipt, a failing client, a broker that cannot be asked: each is a failed call in the ledger and fails the stage", { skip: POSIX }, async (t) => {
  const h = host(t);
  for (const [name, behavior] of [["tampered argv", { tamper: "requestOtherArgv" }], ["another commit", { tamper: "otherCommit" }], ["a snapshot not the commit's at exit", { tamper: "treeAtExit" }], ["unsigned by the pin", { tamper: "signedByOtherKey" }], ["status fails", { statusFail: true }], ["the job is still live", { statusLive: true }]]) {
    h.fx.setBehavior(behavior);
    const before = h.entries().entries.length;
    const r = h.cargo(["build"]);
    assert.notEqual(r.status, 0, name);
    assert.equal(r.outputDir, undefined, `${name}: no output directory is taken from an unverified call`);
    assert.match(r.stderr, /RK_CARGO_BROKER_UNVERIFIED/, name);
    assert.equal(h.entries().entries.length, before + 1, name);
    assert.equal(h.entries().entries.at(-1).status, "unverified", name);
  }
  h.fx.setBehavior({ exit: 3 });
  assert.equal(h.cargo(["build"]).status, 3, "the client's own failure status is the call's (a cargo that failed is the child's to report: its receipt still verifies)");
  h.fx.setBehavior({ refuse: true });
  assert.notEqual(h.cargo(["build"]).status, 0, "a broker that refuses the carrier");
  const { problems } = h.entries();
  assert.equal(problems.length, 7, problems.join("\n"));
  assert.equal((await h.installed.problems(h.fx.sb)).length, 7, "the stage's problems name every one");
  assert.deepEqual(fs.readdirSync(h.requests), [], "no request file is left behind, whatever the outcome");
});

test("negative: a replayed nonce or request id (two ledgered calls claiming one carrier) is a replay the gate refuses", { skip: POSIX }, async (t) => {
  const h = host(t);
  assert.equal(h.cargo(["build"]).status, 0);
  const [first] = h.entries().entries;
  appendLedger(h.installed.ledgerDir, undefined, { ...first, id: "replayed-call" });
  const problems = await h.installed.problems(h.fx.sb);
  assert.equal(problems.length, 1, problems.join("\n"));
  assert.match(problems[0], /replay: a carrier or request was used twice/);
  // The fixture broker itself accepts each nonce once.
  const state = JSON.parse(fs.readFileSync(path.join(h.fx.state, "calls.jsonl"), "utf8").trim().split("\n")[0]);
  assert.equal(state.argv[0], "build");
});

test("negative: --manifest-path without a value, --target-dir, --config, -Z and -C are refused and ledgered; the broker is never asked", { skip: POSIX }, (t) => {
  const h = host(t);
  const refused = [
    [["build", "--target-dir", "/x"], /RK_CARGO_REDIRECT/], [["build", "--target-dir=/x"], /RK_CARGO_REDIRECT/], [["build", "--config", "build.target-dir='x'"], /RK_CARGO_REDIRECT/],
    [["--config=a=b", "build"], /RK_CARGO_REDIRECT/], [["build", "-Zunstable-options"], /RK_CARGO_REDIRECT/], [["build", "-C", "x"], /RK_CARGO_REDIRECT/], [["build", "--manifest-path"], /RK_CARGO_BROKER_MANIFEST_PATH/],
  ];
  for (const [args, code] of refused) { const r = h.cargo(args); assert.equal(r.status, 101, args.join(" ")); assert.match(r.stderr, code, args.join(" ")); }
  assert.deepEqual(h.fx.calls(), [], "nothing reached the broker");
  const { entries, problems } = h.entries();
  assert.equal(entries.length, refused.length); assert.ok(entries.every((e) => e.status === "refused"));
  assert.equal(problems.length, refused.length, "each refusal fails the stage through the ledger");
  // After `--` the arguments are the program's own, not cargo's.
  assert.equal(h.cargo(["run", "--", "--target-dir", "x"]).status, 0);
  assert.deepEqual(h.fx.calls().at(-1).argv, ["run", "--locked", "--", "--target-dir", "x"], "a run is sent locked; after -- the arguments are the program's");
});

test("an output directory of the caller's (CARGO_TARGET_DIR or CARGO_BUILD_TARGET_DIR, any case) is dropped: the broker pins it, and the receipt's directory is verified whatever the environment held", { skip: POSIX }, async (t) => {
  const h = host(t);
  for (const name of ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "Cargo_Target_Dir"]) {
    const r = h.cargo(["build"], { over: { [name]: "/some/target" } });
    assert.equal(r.status, 0, `${name}: ${r.stderr}`);
    assert.deepEqual(h.fx.calls().at(-1).env, { CARGO_TARGET_DIR: null, CARGO_BUILD_TARGET_DIR: null }, `${name} never reaches the broker`);
    assert.equal(r.outputDir, h.fx.outputDir);
  }
  assert.deepEqual(await h.installed.problems(h.fx.sb), []);
  // A broker that answers another directory (the snapshot's, the checkout's, one outside the managed ws root) is unverified, not trusted.
  for (const tamper of ["outputInsideSnapshot", "outputInsideCheckout", "outputOutsideWs", "outputNotTarget", "outputRelative"]) {
    h.fx.setBehavior({ tamper });
    const r = h.cargo(["build"]);
    assert.notEqual(r.status, 0, tamper); assert.equal(r.outputDir, undefined, tamper); assert.match(r.stderr, /RK_CARGO_BROKER_UNVERIFIED/, tamper);
  }
});

test("negative: a carrier or per-call carrier variable already in the environment is refused, in either spelling; the stale carrier never reaches the client", { skip: POSIX }, (t) => {
  const h = host(t);
  const stale = JSON.stringify({ path: path.join(h.requests, "stale.json"), sha256: "a".repeat(64) });
  const cases = { RIGHTKIT_SNAPSHOT_REQUEST: stale, RUSTRIGHT_SNAPSHOT_REQUEST: stale, RIGHTKIT_ATTEST_NONCE: "b".repeat(64), RUSTRIGHT_ATTEST_NONCE: "b".repeat(64), RUSTRIGHT_REQUEST_ID_FILE: "/x", RUSTRIGHT_PARENT_REQUEST_ID: "11111111-1111-4111-8111-111111111111", RIGHTKIT_PARENT_REQUEST_ID: "p", RUSTRIGHT_PARENT_CAPABILITY_TOKEN: "t", RUSTRIGHT_CHECK_ADMISSION: "1", RIGHTKIT_CHECK_ADMISSION: " 1 " };
  for (const [name, value] of Object.entries(cases)) {
    const r = h.cargo(["build"], { over: { [name]: value } });
    assert.equal(r.status, 101, name); assert.match(r.stderr, /RK_CARGO_BROKER_INHERITED_ENV/, name); assert.ok(r.stderr.includes(name), name);
  }
  assert.deepEqual(h.fx.calls(), [], "no client ran");
  // An admission-check variable that is not `1` is no carrier; the unrelated environment passes.
  assert.equal(h.cargo(["build"], { over: { RUSTRIGHT_CHECK_ADMISSION: "0", RUSTRIGHT_BUILD_PRIORITY: "low" } }).status, 0);
  assert.deepEqual(inheritedCarrierEnv({ RUSTRIGHT_SNAPSHOT_REQUEST: "x", PATH: "/bin", RUSTRIGHT_CALLING_GATE: "g" }), ["RUSTRIGHT_SNAPSHOT_REQUEST"]);
  // The gate's own carrier of this call wins nothing from the stale one: the real client reads the current name first, so the stale name would have been used.
});

test("negative: a request directory that is a link, and a request file that cannot be made, refuse the call before the broker is asked; a stale request file is not ours and stays, ours never does", { skip: POSIX }, (t) => {
  const h = host(t);
  // The reader accepts only a real directory: a link in its place is refused by the gate before anything is written.
  const elsewhere = path.join(h.fx.root, "elsewhere"); fs.mkdirSync(elsewhere);
  fs.rmSync(h.requests, { recursive: true, force: true }); fs.symlinkSync(elsewhere, h.requests);
  const r = h.cargo(["build"]);
  assert.equal(r.status, 101); assert.match(r.stderr, /RK_CARGO_BROKER_CARRIER/);
  assert.deepEqual(h.fx.calls(), []); assert.deepEqual(fs.readdirSync(elsewhere), [], "nothing was written through the link");
  fs.rmSync(h.requests); fs.mkdirSync(h.requests);
  // A stale file of an earlier run: the gate never reads or removes it; the call's own request file is removed.
  fs.writeFileSync(path.join(h.requests, "stale.json"), "{}");
  assert.equal(h.cargo(["build"]).status, 0);
  assert.deepEqual(fs.readdirSync(h.requests), ["stale.json"]);
  // The fixture client enforces the reader's rules: a descriptor to a link, to a directory, a stale carrier replayed, an oversize descriptor.
  const client = h.fx.client;
  const run = (env, args = ["build"], cwd = path.join(h.fx.checkout, "crates", "a")) => spawnSync(client, ["cargo", ...args], { cwd, env: { ...process.env, RUSTRIGHT_CALLING_GATE: GATE, ...env }, encoding: "utf8" });
  const link = path.join(h.requests, "link.json"); fs.symlinkSync(path.join(h.requests, "stale.json"), link);
  const sha = "0".repeat(64);
  for (const [name, descriptor] of [["a link", { path: link, sha256: sha }], ["a directory", { path: h.requests, sha256: sha }], ["unknown descriptor fields", { path: link, sha256: sha, extra: 1 }], ["a path outside snapshot-requests", { path: path.join(h.fx.root, "x.json"), sha256: sha }]]) {
    const r2 = run({ RUSTRIGHT_SNAPSHOT_REQUEST: JSON.stringify(descriptor), RUSTRIGHT_ATTEST_NONCE: "c".repeat(64) });
    assert.equal(r2.status, 2, name); assert.match(r2.stderr, /invalid snapshot request carrier/, name);
  }
  assert.equal(run({ RUSTRIGHT_SNAPSHOT_REQUEST: JSON.stringify({ path: link, sha256: sha, pad: "x".repeat(5000) }) }).status, 2, "an oversize descriptor");
  assert.equal(run({ RUSTRIGHT_SNAPSHOT_REQUEST: JSON.stringify({ path: link, sha256: sha }) }, ["build", "--manifest-path", "x"]).status, 2, "an attested call with --manifest-path");
});

test("the seam: a call outside every workspace the stage resolved is refused (the commit's workspaces only); an unreadable carrier map is refused, never run uncarried", { skip: POSIX }, (t) => {
  const h = host(t);
  const stray = path.join(h.fx.root, "stray"); fs.mkdirSync(stray); fs.writeFileSync(path.join(stray, "Cargo.toml"), '[package]\nname = "s"\nversion = "0.1.0"\n');
  let r = h.cargo(["build"], { cwd: stray });
  assert.equal(r.status, 101); assert.match(r.stderr, /RK_CARGO_UNBOUND_WORKSPACE/);
  r = h.cargo(["build"], { cwd: h.fx.root });
  assert.equal(r.status, 101); assert.match(r.stderr, /RK_CARGO_UNBOUND_WORKSPACE/);
  assert.deepEqual(h.fx.calls(), []);
  // No gate named: the call never forges one.
  const noGate = h.cargo(["build"], { over: { RUSTRIGHT_CALLING_GATE: "" } });
  assert.equal(noGate.status, 101); assert.match(noGate.stderr, /RK_CARGO_BROKER_NO_GATE/);
  // A carrier map that cannot be read, or is not a broker map, refuses the call: it is never run uncarried.
  let launched = 0;
  const spawn = () => { launched += 1; return { status: 0, stdout: "", stderr: "" }; };
  const base = { HOME: process.env.HOME, PATH: process.env.PATH };
  for (const env of [{ ...base, [CARRIED_CARGO_MAP_ENV]: path.join(h.fx.root, "no-such-map.json") }, { ...base, [CARRIED_CARGO_MAP_ENV]: path.join(h.fx.state, "fixture.json") }]) {
    const out = runCargoSync(["build"], { cwd: h.fx.snapshot, env }, { ...MAC, spawn });
    assert.equal(out.status, 101); assert.equal(out.error?.code, CARRIER_MISSING_CODE); assert.match(out.stderr, /RK_CARGO_CARRIER_MISSING/); assert.match(out.error.message, /^RK_CARGO_CARRIER_MISSING: /);
  }
  assert.equal(launched, 0);
  return runCargoAsync(["build"], { cwd: h.fx.snapshot, env: { ...base, [CARRIED_CARGO_MAP_ENV]: path.join(h.fx.root, "no-such-map.json") } }, { ...MAC, spawn }).then((a) => { assert.equal(a.status, 101); assert.equal(launched, 0); });
});

test("the seam never applies on Linux (no broker there, whatever the environment names), and a call with no carrier map in its environment keeps the managed client route (refusing a carrier-less compile from a snapshot stage is RustRight's side)", { skip: POSIX }, (t) => {
  const h = host(t);
  const seen = [];
  const spawn = (command, args) => { seen.push([command, ...args]); return { status: 0, stdout: "", stderr: "" }; };
  runCargoSync(["build"], { cwd: h.a, env: { ...h.env, PATH: "/usr/bin" } }, { platform: "linux", exists: () => false, spawn });
  assert.deepEqual(seen, [["cargo", "build"]], "Linux runs the toolchain's cargo and ignores the map");
  assert.deepEqual(h.fx.calls(), []);
  assert.equal(carriedCargoMapOf({}), null);
  assert.equal(carriedCargoMapOf({ [CARRIED_CARGO_MAP_ENV]: h.installed.env[CARRIED_CARGO_MAP_ENV] }).map.mode, "broker");
});

test("the third-party wrapper (the Tauri CLI's and cargo-about's cargo): first on PATH and in CARGO, refuses --config, --target-dir and --manifest-path, ledgers every call, and the stage's problems name the refusals", { skip: POSIX }, async (t) => {
  const h = host(t);
  assert.equal(h.installed.mode, "shim");
  assert.equal(h.installed.env.CARGO, h.installed.shim);
  assert.ok(h.installed.env.PATH.startsWith(path.dirname(h.installed.shim)), "the wrapper is first on PATH");
  const wrap = (args, cwd = h.a) => spawnSync(h.installed.shim, args, { cwd, env: { ...process.env, ...h.env }, encoding: "utf8" });
  const ok = wrap(["build", "--locked"]);
  assert.equal(ok.status, 0, ok.stderr);
  for (const [args, code] of [[["build", "--config", "build.target-dir='x'"], "RK_CARGO_REDIRECT"], [["build", "--target-dir", "x"], "RK_CARGO_REDIRECT"], [["build", "--manifest-path", path.join(h.a, "Cargo.toml")], "RK_CARGO_BROKER_MANIFEST_PATH"]]) {
    const r = wrap(args);
    assert.equal(r.status, 101, args.join(" ")); assert.match(r.stderr, new RegExp(code), args.join(" "));
  }
  assert.deepEqual(h.fx.calls().map((c) => c.argv), [["build", "--locked"]], "only the clean call reached the broker");
  const problems = await h.installed.problems(h.fx.sb);
  assert.equal(problems.length, 3, problems.join("\n"));
  assert.ok(problems.every((p) => /was refused/.test(p)));
  // Both halves share one ledger and one map: the seam's own call and a third party's call are told apart only by their ids.
  assert.equal(h.cargo(["check"]).status, 0);
  assert.equal(h.entries().entries.filter((e) => e.status === "verified").length, 2);
});

test("installCarrier on Windows writes the map and the ledger only: no .cmd, .bat or PATHEXT shim, nothing on PATH, and the seam works from the map", { skip: POSIX }, async (t) => {
  const fx = makeFakeBroker(t);
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout }, section: brokerSection(fx.sb), platform: "win32", env: {} });
  t.after(() => installed.cleanup());
  assert.equal(installed.mode, "map"); assert.equal(installed.shim, null);
  assert.deepEqual(Object.keys(installed.env), [CARRIED_CARGO_MAP_ENV], "only the map; no PATH, CARGO or wrapper variable");
  assert.deepEqual(fs.readdirSync(installed.dir).sort(), ["cargo-target-map.json", "ledger"], "no wrapper of any kind is written");
  const r = runCargoSync(["build"], { cwd: path.join(fx.snapshot, "crates", "a"), env: { RUSTRIGHT_CALLING_GATE: GATE, ...installed.env }, encoding: "utf8" }, { ...MAC });
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(await installed.problems(fx.sb), []);
  installed.cleanup();
  assert.equal(fs.existsSync(installed.dir), false);
});

test("the consumers of the seam take the output directory from a receipt, and ask Cargo (a read-only snapshot request) only when no receipt names it", { skip: POSIX || (process.platform !== "darwin" && "the seam only applies on a broker host (the process platform decides here)") }, async (t) => {
  const h = host(t);
  const saved = { map: process.env[CARRIED_CARGO_MAP_ENV], gate: process.env.RUSTRIGHT_CALLING_GATE };
  process.env[CARRIED_CARGO_MAP_ENV] = h.installed.env[CARRIED_CARGO_MAP_ENV]; process.env.RUSTRIGHT_CALLING_GATE = GATE;
  t.after(() => { for (const [k, v] of [[CARRIED_CARGO_MAP_ENV, saved.map], ["RUSTRIGHT_CALLING_GATE", saved.gate]]) { if (v === undefined) delete process.env[k]; else process.env[k] = v; } });
  const { resolveTargetRoot } = await import("./cargo-target.mjs");
  const manifest = path.join(h.native, "Cargo.toml");
  // No receipt yet: the fallback is a carried read-only `metadata`, which the broker refuses today (a fixture of command_policy.rs).
  assert.throws(() => resolveTargetRoot(manifest), /metadata failed/);
  assert.match(h.entries().problems.join("\n"), /unverified/);
  // A build of that workspace is a compile: its receipt names the directory, and nothing asks Cargo again.
  const { defaultRun } = await import("./stage-sidecar.mjs");
  h.fx.setBehavior({ stdout: '{"reason":"compiler-artifact"}\n' });
  const built = defaultRun("rustright", ["cargo", "build", "--locked", "--manifest-path", manifest, "-p", "b", "--message-format=json-render-diagnostics"], { cwd: h.fx.snapshot, env: { ...process.env } });
  assert.equal(built.status, 0, built.stderr);
  assert.equal(built.stdout, '{"reason":"compiler-artifact"}\n'); assert.equal(built.outputDir, h.fx.outputDir);
  assert.equal(h.fx.calls().at(-1).cwd, path.join(h.fx.checkout, "native")); assert.ok(!h.fx.calls().at(-1).argv.includes("--manifest-path"));
  const before = h.fx.calls().length;
  assert.equal(resolveTargetRoot(manifest), h.fx.outputDir, "from the receipt of the build, not a metadata call");
  assert.equal(h.fx.calls().length, before, "no read-only request was made");
  // The bundle root of a development build is under that same directory.
  const { resolveTauriBundleDirectory } = await import("./development.mjs");
  assert.ok(typeof resolveTauriBundleDirectory === "function");
  // A workspace no receipt names still has no answer from receipts (the other workspace's directory is not borrowed).
  const { carriedOutputDir } = await import("./carried-cargo.mjs");
  assert.equal(carriedOutputDir(process.env, path.join(h.a, "Cargo.toml")), null);
  assert.equal(carriedOutputDir({}, manifest), null);
});

test("an earlier stage's receipts seed the next stage's carrier (knownOutputDirs): a child that has made no call yet still needs no read-only request", { skip: POSIX }, async (t) => {
  const fx = makeFakeBroker(t);
  const checkoutDirs = { ".": fx.checkout, native: path.join(fx.checkout, "native") };
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs, section: brokerSection(fx.sb), known: { [path.join(fx.checkout, "native")]: fx.outputDir }, platform: "darwin", env: {} });
  t.after(() => installed.cleanup());
  const { carriedOutputDir } = await import("./carried-cargo.mjs");
  const env = { ...installed.env };
  assert.equal(carriedOutputDir(env, path.join(fx.snapshot, "native", "Cargo.toml")), fx.outputDir);
  assert.equal(carriedOutputDir(env, path.join(fx.snapshot, "crates", "a", "Cargo.toml")), null);
  assert.deepEqual(fx.calls(), []);
});

test("one spelling per variable: the gate sets only RUSTRIGHT_* for the carrier, never the legacy RIGHTKIT_* beside it; an inherited one in either spelling is refused", { skip: POSIX }, (t) => {
  const h = host(t);
  assert.equal(h.cargo(["build"]).status, 0);
  const { SNAPSHOT_REQUEST_ENV, ATTEST_NONCE_ENV, REQUEST_ID_FILE_ENV } = h.constants;
  assert.deepEqual([SNAPSHOT_REQUEST_ENV, ATTEST_NONCE_ENV, REQUEST_ID_FILE_ENV], ["RUSTRIGHT_SNAPSHOT_REQUEST", "RUSTRIGHT_ATTEST_NONCE", "RUSTRIGHT_REQUEST_ID_FILE"]);
  const seen = JSON.parse(fs.readFileSync(path.join(h.fx.state, "calls.jsonl"), "utf8").trim().split("\n")[0]);
  assert.deepEqual(seen.carrierEnvNames, ["RUSTRIGHT_ATTEST_NONCE", "RUSTRIGHT_CALLING_GATE", "RUSTRIGHT_REQUEST_ID_FILE", "RUSTRIGHT_SNAPSHOT_REQUEST"], "exactly the RUSTRIGHT_ names, none of the legacy spelling");
  assert.equal(h.cargo(["build"], { over: { RIGHTKIT_CALLING_GATE: GATE, RUSTRIGHT_CALLING_GATE: "" } }).status, 101, "the legacy gate name alone does not name a gate");
});

test("runCarriedCargo records its own refusals in the stage's ledger under the stage's token", { skip: POSIX }, (t) => {
  const h = host(t);
  const map = carriedCargoMapOf(h.env).map;
  const token = "0123456789abcdef";
  const r = runCarriedCargo(["build", "--target-dir", "x"], { cwd: h.a, env: { ...h.env, [SCAN_TOKEN_ENV]: token } }, { map, err: () => {} });
  assert.equal(r.status, 101); assert.equal(r.refusal.code, "RK_CARGO_REDIRECT");
  assert.equal(readLedger(h.installed.ledgerDir, token).entries.length, 1);
  assert.equal(h.entries().entries.length, 0, "filed under the token it ran with");
});
