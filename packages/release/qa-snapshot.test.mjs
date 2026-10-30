// QA on a snapshot stage (rightkit#1301, rustright#190). QA runs an already-built artifact outside the broker, so the door:
//  - runs a step that compiles nothing as a direct, gate-run step with the step's own environment, never an adapter job;
//  - turns a `run-tests` step with no listing into two parts (qa-snapshot-run.mjs): a carried test build whose listing lands outside the
//    snapshot and the checkout, then QA directly against it (the heardright native-journeys and genright e2e-fast steps);
//  - refuses what it cannot carry (`journeys run`, `--build*`), a QA directory outside the snapshot, and an input that resolves into the checkout;
//  - reads `--from-cargo-json` in flag position only.
// The broker is the fixture (test-support/fake-broker.mjs); no Cargo, no real broker.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { BUILD_LAUNCH_REFUSED, buildLaunch, qaBuildReason } from "./build-launch.mjs";
import { readLedger, UNSCOPED } from "./cargo-ledger.mjs";
import { SHIM_MAP_SCHEMA } from "./cargo-target-shim.mjs";
import { CARRIED_CARGO_MAP_ENV, installCarrier } from "./carried-cargo.mjs";
import { spawnSync } from "./background-process.mjs";
import { QA_VALUE_OPTIONS, everyValue, hasListing, isInside, parseRunTests, qaSnapshotPlan } from "./qa-snapshot.mjs";
import { brokerSection } from "./snapshot-broker.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const POSIX = process.platform === "win32" ? "the fixture client and QA runner are POSIX only" : false;
const GATE = "/gate/bin/rightkit-gate.mjs";
const HERE = path.dirname(fileURLToPath(import.meta.url));
const QA_RUN = path.join(HERE, "qa-snapshot-run.mjs");
const CLIENT = "/managed/agent-bin/rustright";

/** A snapshot stage's door host: the map names a snapshot and a primary checkout (the door's QA path rules read them); `cwd` is inside the snapshot. */
function snapshotHost(t) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-qa-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const snapshot = path.join(dir, "snapshot"); const checkout = path.join(dir, "checkout");
  fs.mkdirSync(path.join(snapshot, "app"), { recursive: true }); fs.mkdirSync(path.join(checkout, "app"), { recursive: true });
  const mapFile = path.join(dir, "carrier", "map.json");
  fs.mkdirSync(path.dirname(mapFile), { recursive: true });
  fs.writeFileSync(mapFile, JSON.stringify({ schema: SHIM_MAP_SCHEMA, mode: "broker", broker: { snapshot: { path: snapshot }, checkout }, ledgerDir: path.join(dir, "carrier", "ledger"), workspaces: {} }));
  const qa = path.join(dir, "rightkit-qa");
  const cwd = path.join(snapshot, "app");
  return { dir, qa, snapshot, checkout, cwd, mapFile, host: { platform: "darwin", cwd, exists: (p) => p === CLIENT || p === qa, which: (c) => (c === "rustright" ? [CLIENT] : []), env: { PATH: `/managed/agent-bin:${dir}`, [CARRIED_CARGO_MAP_ENV]: mapFile } } };
}

test("a QA step that compiles nothing runs outside the broker: a direct step with the caller's unscrubbed environment, no adapter job, and never the client", { skip: POSIX }, (t) => {
  const s = snapshotHost(t);
  fs.writeFileSync(s.qa, `#!/bin/sh\nprintf '%s|%s|%s' "$HEARDRIGHT_QA_APP_ROOT" "$RIGHTKIT_QA_TIER" "$*"\n`, { mode: 0o755 });
  const args = ["run-tests", "--from-cargo-json", path.join(s.dir, "carrier", "list.json"), "--app", "heardright", "--parallel", "1", "--", "--nocapture"];
  const launch = buildLaunch("rightkit-qa", args, s.host);
  assert.deepEqual(launch, { command: s.qa, args }, "the QA runner itself, by its resolved path");
  assert.notEqual(launch.command, CLIENT);
  assert.notEqual(launch.command, process.execPath);
  assert.equal(Object.hasOwn(launch, "env"), false, "no adapter environment: QA keeps the caller's own");
  assert.deepEqual(buildLaunch("rustright", ["run", "rightkit-qa", "--", ...args], s.host), launch, "the client spelling is the same direct step");
  const ran = spawnSync(launch.command, launch.args, { env: { ...process.env, HEARDRIGHT_QA_APP_ROOT: "/app", RIGHTKIT_QA_TIER: "fast,local-models" }, encoding: "utf8" });
  assert.equal(ran.status, 0);
  assert.equal(ran.stdout, `/app|fast,local-models|${args.join(" ")}`);
  for (const ok of [["journeys", "receipt", "--root", s.cwd], ["run", "--tier", "fast"], ["--all"]]) assert.deepEqual(buildLaunch("rightkit-qa", ok, s.host), { command: s.qa, args: ok }, ok.join(" "));
  // Off the snapshot route the whole run is still one broker job (the client's `run`), as before.
  assert.deepEqual(buildLaunch("rightkit-qa", ["run-tests", "--app", "a"], { ...s.host, env: { PATH: "/managed/agent-bin" } }), { command: CLIENT, args: ["run", "rightkit-qa", "--", "run-tests", "--app", "a"] });
  assert.equal(Object.hasOwn(buildLaunch("rightkit-qa", ["--all"], { ...s.host, stepEnv: { HEARDRIGHT_QA_APP_ROOT: "/app" } }), "env"), false);
});

test("a QA step that would compile and cannot be carried is refused, naming the build it needs as its own carried step", { skip: POSIX }, (t) => {
  const s = snapshotHost(t);
  const compiles = [
    ["journeys", "run", "--root", s.cwd],
    ["run", "--tier", "fast", "--build"],
    ["run", "--build-ui"],
    ["run-tests", "--from-cargo-json", path.join(s.dir, "l.json"), "--build-with", "cargo"],
    ["run-tests", "--app", "x", "--build-with=rustright"],
    ["cargo", "build"],
    ["run", "pnpm", "build"],
  ];
  for (const args of compiles) {
    assert.ok(qaBuildReason(args, { carried: true }), args.join(" "));
    assert.throws(() => buildLaunch("rightkit-qa", args, s.host), (e) => e.code === BUILD_LAUNCH_REFUSED && /rightkit-qa .*compiles nothing/s.test(e.message) && /its own carried build step/.test(e.message), args.join(" "));
    assert.throws(() => buildLaunch("rustright", ["run", "rightkit-qa", "--", ...args], s.host), (e) => e.code === BUILD_LAUNCH_REFUSED, `via the client spelling: ${args.join(" ")}`);
  }
  assert.throws(() => buildLaunch("rightkit-qa", ["journeys", "run"], s.host), /no prebuilt mode/);
  assert.ok(qaBuildReason(["run-tests", "--app", "x"]), "without the gate's carried build, a run-tests step with no listing compiles");
  assert.equal(qaBuildReason(["run-tests", "--from-cargo-json", "/l.json", "--", "cargo"]), null, "arguments after -- go to the test binaries");
  assert.equal(qaBuildReason(["run-tests", "--from-cargo-json=/l.json", "--app", "x"]), null);
});

test("--from-cargo-json counts only in flag position: a value that spells it belongs to the option before it, and QA_VALUE_OPTIONS is complete", () => {
  for (const opt of ["--app", "--env", "--cwd", "--root", "--run-id", "--managed-root", "--receipt", "--unset-env", "--qa-config", "--total-timeout-s", "--binary-timeout-s", "--compile-timeout-s"]) {
    assert.equal(hasListing(["run-tests", opt, "--from-cargo-json", "--parallel", "1"]), false, `${opt} consumes --from-cargo-json as its value`);
    assert.ok(qaBuildReason(["run-tests", opt, "--from-cargo-json"]), `${opt} --from-cargo-json is still a build without a real listing`);
    assert.ok(QA_VALUE_OPTIONS.has(opt), `${opt} is a value option`);
  }
  // The generic scan (other commands) agrees.
  assert.equal(hasListing(["other", "--managed-root", "--from-cargo-json"]), false);
  assert.equal(hasListing(["other", "--from-cargo-json", "l.json"]), true);
  assert.equal(hasListing(["run-tests", "--from-cargo-json", "l.json"]), true);
  assert.equal(hasListing(["run-tests", "--app", "x", "--from-cargo-json=l.json"]), true);
  assert.equal(hasListing(["run-tests", "--app", "x", "--", "--from-cargo-json", "l.json"]), false, "after -- it is a test binary's argument");
  // The parser is the CLI's own: unknown flags (cargo's) do not consume a value, so a listing flag after one is a flag.
  assert.equal(hasListing(["run-tests", "--target", "--from-cargo-json", "l.json"]), true);
  assert.deepEqual(parseRunTests(["--app", "x", "-p", "c", "--features", "f", "--parallel=1", "--", "a"]), { own: ["--app", "x", "--parallel=1"], cargo: ["-p", "c", "--features", "f"], testArgs: ["a"], flags: new Map([["--app", ["x"]], ["--parallel", ["1"]]]) });
  assert.throws(() => parseRunTests(["--app"]), /needs a value/);
});

/** The app's real step shapes, as the gate's test lane hands them to the door. */
const STEPS = {
  heardrightNativeJourneys: { env: { RIGHTKIT_QA_TIER: "fast,local-models", HEARDRIGHT_QA_APP_ROOT: "/qa/app-root", HEARDRIGHT_QA_KEEP_EVIDENCE: "1" }, args: ["run-tests", "--app", "heardright", "--parallel", "1", "--manifest-path", "./qa-e2e/Cargo.toml", "--", "--nocapture"], plan: ["test", "--manifest-path", "./qa-e2e/Cargo.toml", "--no-run", "--message-format=json"], carried: ["test", "--no-run", "--message-format=json"], own: ["--app", "heardright", "--parallel", "1"], tail: ["--nocapture"] },
  genrightE2eFast: { env: {}, args: ["run-tests", "--app", "genright", "--parallel", "1", "-p", "genright-e2e", "--", "--test-threads=1"], plan: ["test", "-p", "genright-e2e", "--no-run", "--message-format=json"], carried: ["test", "-p", "genright-e2e", "--no-run", "--message-format=json"], own: ["--app", "genright", "--parallel", "1"], tail: ["--test-threads=1"] },
};

test("the plan of a run-tests step: the build argv, and the QA argv with the listing in place of the build arguments", () => {
  for (const step of Object.values(STEPS)) {
    const plan = qaSnapshotPlan(step.args);
    assert.deepEqual(plan.cargoArgs, step.plan);
    assert.deepEqual(plan.qaArgs("/L.jsonl"), ["run-tests", ...step.own, "--from-cargo-json", "/L.jsonl", "--", ...step.tail]);
  }
  assert.deepEqual(qaSnapshotPlan(["run-tests", "--app", "a", "--features", "qa", "--no-run", "--"]).cargoArgs, ["test", "--features", "qa", "--no-run", "--message-format=json"], "--no-run is not doubled");
  assert.deepEqual(qaSnapshotPlan(["run-tests", "--app", "a", "--"]).qaArgs("L"), ["run-tests", "--app", "a", "--from-cargo-json", "L", "--"], "an explicit empty -- stays: it replaces [run_tests] test_args");
  assert.deepEqual(qaSnapshotPlan(["run-tests", "--app", "a", "--message-format=json"]).cargoArgs, ["test", "--message-format=json", "--no-run"]);
  assert.match(qaSnapshotPlan(["run-tests", "--app", "a", "--message-format", "short"]).refusal, /not "short"/);
  assert.equal(qaSnapshotPlan(["run-tests", "--from-cargo-json", "l"]), null);
  assert.equal(qaSnapshotPlan(["journeys", "run"]), null);
  assert.ok(qaSnapshotPlan(["run-tests", "--app", "--from-cargo-json"]).cargoArgs, "a value that spells the listing flag is not a listing");
});

/** A fixture broker host: the QA runner's carrier, the step's directory `native` (its own Cargo workspace, with `qa-e2e` inside), and a fake `rightkit-qa` that records how it ran. */
function qaHost(t, name) {
  const fx = makeFakeBroker(t, { readOnlySnapshots: true });
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout, native: path.join(fx.checkout, "native") }, section: brokerSection(fx.sb), platform: process.platform, env: {} });
  t.after(() => installed.cleanup());
  const step = STEPS[name];
  for (const root of [fx.snapshot, fx.checkout]) {
    fs.mkdirSync(path.join(root, "native", "qa-e2e"), { recursive: true });
    fs.writeFileSync(path.join(root, "native", "qa-e2e", "Cargo.toml"), '[package]\nname = "qa-e2e"\nversion = "0.1.0"\n');
  }
  const out = path.join(fx.root, "qa-ran.json");
  const qa = path.join(fx.root, "rightkit-qa");
  const script = [
    `#!${process.execPath}`,
    'const fs = require("node:fs");',
    "const a = process.argv.slice(2);",
    'const listing = a[a.indexOf("--from-cargo-json") + 1];',
    `fs.writeFileSync(${JSON.stringify(out)}, JSON.stringify({ args: a, cwd: process.cwd(), listing, listingText: fs.readFileSync(listing, "utf8"), env: Object.fromEntries(["RIGHTKIT_QA_TIER", "HEARDRIGHT_QA_APP_ROOT", "HEARDRIGHT_QA_KEEP_EVIDENCE"].map((k) => [k, process.env[k] ?? null])) }));`,
    "process.exit(Number(process.env.FAKE_QA_EXIT ?? 0));",
    "",
  ].join("\n");
  fs.writeFileSync(qa, script, { mode: 0o755 });
  const cwd = path.join(fx.snapshot, "native");
  const stepEnv = { ...step.env, PATH: `${fx.root}:${process.env.PATH}`, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE, RK_GATE_CHECKOUT: fx.checkout, ...installed.env };
  const door = () => buildLaunch("rightkit-qa", step.args, { platform: "darwin", cwd, exists: (p) => p === CLIENT || p === qa, which: (c) => (c === "rightkit-qa" ? [qa] : c === "rustright" ? [CLIENT] : []), env: stepEnv, stepEnv: step.env });
  // The gate's step spawn: the launch's env (none for QA) over the step's, in the step's directory.
  const run = (launch, over = {}) => spawnSync(launch.command, launch.args, { cwd, env: { ...stepEnv, ...(launch.env ?? {}), ...over }, encoding: "utf8" });
  return { fx, installed, step, qa, out, cwd, door, run, map: installed.env[CARRIED_CARGO_MAP_ENV], ran: () => JSON.parse(fs.readFileSync(out, "utf8")), entries: () => readLedger(installed.ledgerDir, UNSCOPED) };
}

for (const [label, name] of [["heardright native-journeys", "heardrightNativeJourneys"], ["genright e2e-fast", "genrightE2eFast"]]) {
  test(`${label} on the snapshot route: a carried test build, then the QA step directly against its listing, with the step's declared env`, { skip: POSIX }, async (t) => {
    const h = qaHost(t, name);
    const launch = h.door();
    assert.deepEqual(launch, { command: process.execPath, args: [QA_RUN, h.qa, "--", ...h.step.args] }, "the door hands the step to the two-part runner: not refused, not an adapter job");
    assert.equal(Object.hasOwn(launch, "env"), false, "QA gets no adapter environment");
    h.fx.setBehavior({ stdout: `${JSON.stringify({ reason: "compiler-artifact", target: { name: "t" } })}\n${JSON.stringify({ reason: "build-finished", success: true })}\n` });
    const r = h.run(launch);
    assert.equal(r.status, 0, r.stderr);
    // (a) one carried build request: the step's own build arguments (the manifest path becomes the working directory), message format json.
    const calls = h.fx.calls();
    assert.equal(calls.length, 1, "one carried broker call");
    assert.deepEqual(calls[0].argv.filter((w) => w !== "--locked"), h.step.carried);
    assert.equal(h.fx.adapterCalls().length, 0, "no adapter job");
    const { entries, problems } = h.entries();
    assert.deepEqual(problems, []);
    assert.deepEqual(entries.map((e) => e.status), ["verified"]);
    assert.deepEqual(await h.installed.problems(h.fx.sb), [], "the gate verifies the build's receipt again itself");
    // (b) QA ran directly from the step's directory with the listing in place of the build arguments; the listing is the build's stdout.
    const ran = h.ran();
    assert.deepEqual(ran.args, ["run-tests", ...h.step.own, "--from-cargo-json", ran.listing, "--", ...h.step.tail]);
    assert.equal(fs.realpathSync(ran.cwd), fs.realpathSync(h.cwd));
    assert.match(ran.listingText, /build-finished/);
    assert.equal(fs.existsSync(ran.listing), false, "the listing is unlinked once QA exits");
    // The listing lives outside the snapshot and the primary checkout, in the carrier's own directory.
    for (const tree of [h.fx.snapshot, h.fx.checkout]) assert.equal(isInside(ran.listing, tree), false, `${ran.listing} is not inside ${tree}`);
    assert.equal(isInside(ran.listing, path.dirname(JSON.parse(fs.readFileSync(h.map, "utf8")).ledgerDir)), true);
    // The direct run got the step's declared environment, unscrubbed (RIGHTKIT_QA_TIER is reserved for adapter jobs; QA is not one).
    for (const [key, value] of Object.entries(h.step.env)) assert.equal(ran.env[key], value, key);
    if (name === "genrightE2eFast") assert.deepEqual(ran.env, { RIGHTKIT_QA_TIER: null, HEARDRIGHT_QA_APP_ROOT: null, HEARDRIGHT_QA_KEEP_EVIDENCE: null });
  });
}

test("a failed carried build stops the step before QA runs; QA's own exit status is the step's; a build with no verified receipt never feeds QA", { skip: POSIX }, (t) => {
  const failed = qaHost(t, "genrightE2eFast");
  failed.fx.setBehavior({ exit: 101, stdout: "" });
  const r = failed.run(failed.door());
  assert.notEqual(r.status, 0);
  assert.equal(fs.existsSync(failed.out), false, "QA did not run");
  assert.match(r.stderr, /RK_QA_SNAPSHOT_BUILD/);
  const ok = qaHost(t, "genrightE2eFast");
  ok.fx.setBehavior({ stdout: "{}\n" });
  assert.equal(ok.run(ok.door(), { FAKE_QA_EXIT: "7" }).status, 7);
  assert.equal(fs.existsSync(ok.ran().listing), false, "the listing is unlinked after a failing QA exit too");
  const unverified = qaHost(t, "genrightE2eFast");
  unverified.fx.setBehavior({ tamper: "failed", stdout: "{}\n" });
  const g = unverified.run(unverified.door());
  assert.notEqual(g.status, 0);
  assert.equal(fs.existsSync(unverified.out), false);
});

test("a QA step's paths: a directory outside the snapshot, or a listing, --root, --cwd or --manifest-path that resolves into the primary checkout, is refused", { skip: POSIX }, (t) => {
  const s = snapshotHost(t);
  const inCheckout = path.join(s.checkout, "app");
  const refused = (args, host = s.host, pattern = /primary checkout/) => assert.throws(() => buildLaunch("rightkit-qa", args, host), (e) => e.code === BUILD_LAUNCH_REFUSED && pattern.test(e.message), args.join(" "));
  refused(["--all"], { ...s.host, cwd: inCheckout }, /outside the commit's snapshot/);
  refused(["--all"], { ...s.host, cwd: s.dir }, /outside the commit's snapshot/);
  refused(["--all"], { ...s.host, cwd: path.join(s.snapshot, "..", "elsewhere") }, /outside the commit's snapshot/);
  refused(["run-tests", "--from-cargo-json", path.join(inCheckout, "l.json"), "--app", "x"]);
  refused(["run-tests", `--from-cargo-json=${path.join(inCheckout, "l.json")}`, "--app", "x"]);
  refused(["run-tests", "--root", inCheckout, "--from-cargo-json", path.join(s.dir, "l.json")]);
  refused(["run-tests", "--root", "../../checkout/app", "--from-cargo-json", "l.json"]);
  refused(["run-tests", "--from-cargo-json", "l.json", "--cwd", path.join(inCheckout, "pkg")]);
  refused(["run-tests", "--from-cargo-json", "l.json", "--cwd", "../../checkout/app"]);
  refused(["run-tests", "--app", "x", "--manifest-path", path.join(inCheckout, "Cargo.toml")]);
  refused(["run-tests", "--app", "x", "--manifest-path=../../checkout/app/Cargo.toml"]);
  refused(["journeys", "receipt", "--root", inCheckout]);
  fs.symlinkSync(inCheckout, path.join(s.cwd, "link"));
  refused(["run-tests", "--from-cargo-json", "link/l.json", "--app", "x"]);
  // The same shapes inside the snapshot, `package`/`workspace` and stdin are fine.
  for (const ok of [["run-tests", "--from-cargo-json", "l.json", "--app", "x"], ["run-tests", "--from-cargo-json", "-", "--cwd", "workspace"], ["run-tests", "--from-cargo-json", "l.json", "--cwd", "package", "--root", s.cwd], ["run-tests", "--from-cargo-json", "l.json", "--cwd", "pkg"]]) assert.deepEqual(buildLaunch("rightkit-qa", ok, s.host), { command: s.qa, args: ok }, ok.join(" "));
  // A path that is another option's value, or comes after `--`, is not a named input.
  assert.ok(buildLaunch("rightkit-qa", ["run-tests", "--from-cargo-json", "l.json", "--app", inCheckout, "--", "--root", inCheckout], s.host));
  // A map that names no snapshot cannot place the step: refused rather than guessed.
  const bare = snapshotHost(t);
  fs.writeFileSync(bare.mapFile, JSON.stringify({ ...JSON.parse(fs.readFileSync(bare.mapFile, "utf8")), broker: {} }));
  assert.throws(() => buildLaunch("rightkit-qa", ["--all"], bare.host), /outside the commit's snapshot/);
});

test("a non-run-tests QA command reads the WHOLE argv (rightkit-qa.rs Args ignores `--`): a build flag anywhere is a build, in every spelling", { skip: POSIX }, (t) => {
  const s = snapshotHost(t);
  for (const args of [["run", "--", "--build"], ["run", "--", "--build-ui"], ["run", "--", "--build-with", "cargo"], ["run", "--", "--build-with=cargo"], ["run", "--build=1"], ["run", "--build-ui=1"], ["run", "--app", "--build"], ["run", "--", "pnpm", "build"]]) {
    assert.ok(qaBuildReason(args, { carried: true }), args.join(" "));
    assert.throws(() => buildLaunch("rightkit-qa", args, s.host), (e) => e.code === BUILD_LAUNCH_REFUSED && /compiles nothing/.test(e.message), args.join(" "));
  }
  // run-tests keeps its own parser: after `--` the words are the test binaries'.
  assert.equal(qaBuildReason(["run-tests", "--from-cargo-json", "l.json", "--", "--build"], { carried: true }), null);
  assert.deepEqual(buildLaunch("rightkit-qa", ["run", "--tier", "fast"], s.host), { command: s.qa, args: ["run", "--tier", "fast"] });
});

test("--root is refused with a clear code on a carried run-tests build; with a listing it is only a path check", { skip: POSIX }, (t) => {
  const s = snapshotHost(t);
  for (const args of [["run-tests", "--root", s.cwd, "--app", "x"], ["run-tests", `--root=${s.cwd}`, "--app", "x"]]) {
    assert.match(qaSnapshotPlan(args).refusal, /^RK_QA_SNAPSHOT_ROOT_UNSUPPORTED/);
    assert.throws(() => buildLaunch("rightkit-qa", args, s.host), (e) => e.code === BUILD_LAUNCH_REFUSED && /RK_QA_SNAPSHOT_ROOT_UNSUPPORTED/.test(e.message), args.join(" "));
  }
  assert.equal(qaSnapshotPlan(["run-tests", "--root", s.cwd, "--from-cargo-json", "l.json"]), null);
});

test("every value of a path flag counts for a non-run-tests command, after `--` and repeats included; --qa-config, --config, --app-root and RIGHTKIT_QA_CONFIG are checked too", { skip: POSIX }, (t) => {
  const s = snapshotHost(t);
  const bad = path.join(s.checkout, "app");
  const refused = (args, host = s.host) => assert.throws(() => buildLaunch("rightkit-qa", args, host), (e) => e.code === BUILD_LAUNCH_REFUSED && /primary checkout/.test(e.message), args.join(" "));
  assert.deepEqual(everyValue(["x", "--root", "a", "--", "--root=b", "--root"], "--root"), ["a", "b"]);
  refused(["journeys", "receipt", "--root", s.cwd, "--root", bad]);
  refused(["journeys", "receipt", "--root", s.cwd, "--", "--root", bad]);
  refused(["run", "--manifest-path", "m.toml", "--manifest-path", path.join(bad, "Cargo.toml")]);
  refused(["run", "--config", path.join(bad, "qa.toml")]);
  refused(["run", "--app-root", bad]);
  refused(["run", "--", "--app-root=" + bad]);
  refused(["run", "--qa-config", path.join(bad, "qa.toml")]);
  refused(["run-tests", "--qa-config", path.join(bad, "qa.toml"), "--from-cargo-json", "l.json"]);
  refused(["run", "--tier", "fast"], { ...s.host, stepEnv: { RIGHTKIT_QA_CONFIG: path.join(bad, "qa.toml") } });
  refused(["run", "--tier", "fast"], { ...s.host, env: { ...s.host.env, RIGHTKIT_QA_CONFIG: "../../checkout/app/qa.toml" } });
  for (const ok of [["run", "--config", "qa.toml", "--app-root", s.cwd], ["run", "--qa-config", "qa.toml"]]) assert.deepEqual(buildLaunch("rightkit-qa", ok, { ...s.host, stepEnv: { RIGHTKIT_QA_CONFIG: path.join(s.cwd, "qa.toml") } }), { command: s.qa, args: ok }, ok.join(" "));
});
