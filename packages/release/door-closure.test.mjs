// rightkit#1301, one model (rustright#190): every compile of a snapshot stage on a Mac or Dell broker host is a carried request or nested under a
// carried, snapshot-bound adapter job. This file pins the steps that used to reach the broker client uncarried (a swift step, a non-Cargo
// `rustright` step, a stage-sidecar command or cmake step) and the door's own admission rules (the pnpm exec target allowlist, run/exec/pack
// only in a snapshot call), against the fixture broker (test-support/fake-broker.mjs). No real broker, no Cargo, no Swift.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { BUILD_LAUNCH_REFUSED, EXEC_TARGETS, buildLaunch } from "./build-launch.mjs";
import { readLedger, UNSCOPED } from "./cargo-ledger.mjs";
import { installCarrier } from "./carried-cargo.mjs";
import { buildAndEmbedNativeHelpers } from "./native-helpers.mjs";
import { brokerSection } from "./snapshot-broker.mjs";
import { runStep } from "./stage-sidecar.mjs";
import { spawnSync } from "./background-process.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const POSIX = process.platform === "win32" ? "the fixture client and adapter PATH are POSIX only" : false;
const MAC = process.platform === "darwin" ? false : "the broker route is the Mac and Windows hosts' (resolveExecutor); this host runs direct";
const GATE = "/gate/bin/rightkit-gate.mjs";
const HERE = path.dirname(fileURLToPath(import.meta.url));
const ADAPTER_RUN = path.join(HERE, "carried-adapter-run.mjs");

/** A broker host with a carrier: `env` is a snapshot stage's (the map in it, PATH holding the fixture client), `door` a buildLaunch from the snapshot. */
function host(t) {
  const fx = makeFakeBroker(t, { readOnlySnapshots: true });
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout }, section: brokerSection(fx.sb), platform: process.platform, env: {} });
  t.after(() => installed.cleanup());
  const env = { PATH: fx.root, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE, ...installed.env };
  const door = (command, args, over = {}) => buildLaunch(command, args, { env, platform: "darwin", exists: (p) => p === path.join(fx.root, "rustright"), cwd: fx.snapshot, ...over });
  const entries = () => readLedger(installed.ledgerDir, UNSCOPED);
  return { fx, installed, env, door, entries };
}
const refused = (pattern) => (error) => error.code === BUILD_LAUNCH_REFUSED && pattern.test(error.message);

test("a swift step of a snapshot stage is a carried rightkit-swift adapter job, not an uncarried client call; without a carrier it is the client's, as before", { skip: POSIX }, (t) => {
  const h = host(t);
  const args = ["build", "-c", "release", "--package-path", "macos-native/Pkg", "--product", "tool", "--arch", "arm64", "--arch", "x86_64"];
  const launch = h.door("swift", args);
  assert.deepEqual(launch, { command: process.execPath, args: [ADAPTER_RUN, "rightkit-swift", "--", ...args] });
  // Run it: one signed, ledgered adapter request in the snapshot's checkout counterpart, verified by the gate's own checks.
  const ran = spawnSync(launch.command, launch.args, { cwd: h.fx.snapshot, env: h.env, encoding: "utf8" });
  assert.equal(ran.status, 0, ran.stderr);
  const jobs = h.fx.adapterCalls();
  assert.deepEqual(jobs.map((j) => [j.adapter, j.argv, j.cwd]), [["rightkit-swift", args, h.fx.checkout]]);
  const { entries, problems } = h.entries();
  assert.deepEqual(problems, []);
  assert.deepEqual(entries.map((e) => [e.status, e.shim, e.adapter]), [["verified", "adapter", "rightkit-swift"]]);
  // Swift's own grammar in a snapshot call: `build` only, one relative --package-path (the fixture models check_swift_package).
  for (const bad of [["package", "resolve"], ["build", "--package-path", "/elsewhere/pkg"], ["build", "--product", "x"]]) {
    const denied = spawnSync(process.execPath, [ADAPTER_RUN, "rightkit-swift", "--", ...bad], { cwd: h.fx.snapshot, env: h.env, encoding: "utf8" });
    assert.notEqual(denied.status, 0, bad.join(" "));
  }
  // No carrier map: the client's `run rightkit-swift`, unchanged.
  const client = path.join(h.fx.root, "rustright");
  const { RIGHTKIT_CARRIED_CARGO_MAP: _, ...bare } = h.env;
  assert.deepEqual(h.door("swift", args, { env: bare }), { command: client, args: ["run", "rightkit-swift", "--", ...args] });
});

test("a shared Swift package outside the commit snapshot is refused with NATIVE_HELPERS_SNAPSHOT_SOURCE, whatever names it; a package inside the snapshot is built as a carried swift job from the snapshot", { skip: MAC }, (t) => {
  const h = host(t);
  const app = path.join(h.fx.snapshot, "app");
  fs.mkdirSync(path.join(app, "dist", "Demo.app", "Contents"), { recursive: true });
  fs.writeFileSync(path.join(app, "dist", "Demo.app", "Contents", "Info.plist"), "<plist/>");
  // The editable RightKit checkout (RIGHTKIT_SWIFT_ROOT, or the installed package root by default) holds the shared package: outside the snapshot.
  const editable = path.join(h.fx.root, "editable-rightkit");
  fs.mkdirSync(path.join(editable, "swift", "RightKitCloudKitHelper"), { recursive: true });
  fs.writeFileSync(path.join(editable, "swift", "RightKitCloudKitHelper", "Package.swift"), "// package\n");
  const cloud = { name: "cloud", sharedPackage: "cloudkit", product: "rightkit-cloudkit-helper", kind: "app", embed: "Contents/Library/LoginItems/Cloud.app", universal: true, executable: "Cloud", info: { bundleId: "com.example.next", name: "Cloud" } };
  const calls = [];
  const spawn = (command, args) => { calls.push({ command, args }); return { status: 0, stdout: "", stderr: "" }; };
  for (const via of [{ sharedRoot: editable }, { env: { ...h.env, RIGHTKIT_SWIFT_ROOT: editable } }]) {
    assert.throws(
      () => buildAndEmbedNativeHelpers({ appRoot: app, app: path.join(app, "dist", "Demo.app"), helpers: [cloud], version: "1.0.0", env: h.env, spawn, ...via }),
      (e) => e.code === "NATIVE_HELPERS_SNAPSHOT_SOURCE" && /outside this stage's commit snapshot/.test(e.message) && /sharedPackage/.test(e.message) && /declare it with `package`/.test(e.message),
    );
  }
  assert.deepEqual(calls, [], "nothing ran: not a swift job, not a client call");
  // The app's own package, committed into the snapshot, builds: the first call is the carried adapter job, from the snapshot root.
  fs.mkdirSync(path.join(app, "macos-native", "Pkg"), { recursive: true });
  fs.writeFileSync(path.join(app, "macos-native", "Pkg", "Package.swift"), "// package\n");
  const own = { ...cloud, sharedPackage: undefined, package: "macos-native/Pkg" };
  delete own.sharedPackage;
  assert.throws(() => buildAndEmbedNativeHelpers({ appRoot: app, app: path.join(app, "dist", "Demo.app"), helpers: [own], version: "1.0.0", forceArch: "arm64", env: h.env, spawn }), (e) => e.code === "NATIVE_HELPERS_STEP_FAILED" && /no absolute path/.test(e.message), "the stub spawn prints no bin path: the build was attempted, so the package passed the snapshot check");
  assert.equal(calls[0].command, process.execPath);
  assert.deepEqual(calls[0].args.slice(0, 3), [ADAPTER_RUN, "rightkit-swift", "--"]);
  assert.ok(calls[0].args.includes(path.join("app", "macos-native", "Pkg")), "the package path is relative to the snapshot root, which is the job's directory");
});

test("a non-Cargo `rustright` step on the snapshot route is carried when it names a registered adapter (judged by that program's door rules) and refused with its code otherwise", { skip: POSIX }, (t) => {
  const h = host(t);
  // `rustright run <adapter> -- <args>` is the adapter's own step.
  assert.deepEqual(h.door("rustright", ["run", "rightkit-pnpm", "--", "exec", "tauri", "build"]), { command: process.execPath, args: [ADAPTER_RUN, "rightkit-pnpm", "--", "exec", "tauri", "build"] });
  assert.deepEqual(h.door("rustright", ["run", "rightkit-swift", "--", "build", "--package-path", "p"]).args.slice(0, 3), [ADAPTER_RUN, "rightkit-swift", "--"]);
  assert.deepEqual(h.door("rustright", ["run", "rightkit-node", "--", "scripts/x.mjs", "--platform", "win"]).args, [ADAPTER_RUN, "rightkit-node", "--", "scripts/x.mjs", "--platform", "win"]);
  // The program's rules still apply: this is not a way around the node flag ban or the exec target allowlist.
  assert.throws(() => h.door("rustright", ["run", "rightkit-node", "--", "-e", "process.exit(0)"]), refused(/node flag -e is not allowed/));
  assert.throws(() => h.door("rustright", ["run", "rightkit-pnpm", "--", "exec", "node", "-e", "1"]), refused(/exec target "node" is not allowed/));
  // Anything else is an uncarried client call: refused, naming the step.
  for (const args of [["status", "abc"], ["run", "rightkit-unknown", "--", "x"], ["run", "rightkit-pnpm", "x"], ["doctor"], []]) {
    assert.throws(() => h.door("rustright", args), refused(new RegExp(`rustright ${args.join(" ").replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}: a snapshot stage carries every compile`)), args.join(" "));
  }
  // `rustright cargo` stays the carried Cargo seam; with no carrier the client takes every rustright command as before.
  assert.equal(h.door("rustright", ["cargo", "build", "--locked"]).args[0].endsWith("carried-cargo-run.mjs"), true);
  const { RIGHTKIT_CARRIED_CARGO_MAP: _, ...bare } = h.env;
  assert.deepEqual(h.door("rustright", ["status", "abc"], { env: bare }), { command: path.join(h.fx.root, "rustright"), args: ["status", "abc"] });
});

test("stage-sidecar's step commands (stage.cmd, a command entry's cmd, cmake) of a carried stage are carried adapter jobs, the carried Cargo seam or the door's own right-release, and any other is refused with STAGE_SIDECAR_STEP_REFUSED", { skip: MAC }, (t) => {
  const h = host(t);
  const ran = [];
  const ctx = (env) => ({ env, fs: { existsSync: fs.existsSync }, run: (command, args, options) => { ran.push({ command, args, options }); return { status: 0 }; } });
  const own = path.join(HERE, "cli", "right-release.mjs");
  const at = { cwd: h.fx.snapshot };
  runStep(ctx(h.env), "node", ["scripts/prepare-runtime.mjs", "stage-ffmpeg", "--platform", "win"], { ...at, env: h.env });
  runStep(ctx(h.env), "pnpm", ["exec", "tauri", "build"], { ...at, env: h.env });
  runStep(ctx(h.env), "rustright", ["cargo", "run", "-p", "x", "--example", "stage_media_bins"], { ...at, env: h.env });
  runStep(ctx(h.env), "right-release", ["web-build"], { ...at, env: h.env });
  runStep(ctx(h.env), "pnpm", ["exec", "right-release", "stage-sidecar", "--target", "t"], { ...at, env: h.env });
  assert.deepEqual(ran.map((r) => [r.command, r.args.slice(0, 3)]), [
    [process.execPath, [ADAPTER_RUN, "rightkit-node", "--"]],
    [process.execPath, [ADAPTER_RUN, "rightkit-pnpm", "--"]],
    ["rustright", ["cargo", "run", "-p"]],
    [process.execPath, [own, "web-build"]],
    [process.execPath, [own, "stage-sidecar", "--target"]],
  ]);
  for (const [command, args, pattern] of [
    ["cmake", ["-S", "src", "-B", "build"], /cmake -S src -B build.*no broker adapter runs this command yet/],
    ["rustright", ["status", "x"], /maps to no registered adapter/],
    ["pnpm", ["exec", "node", "-e", "1"], /exec target "node" is not allowed/],
    ["/usr/bin/make", ["all"], /no broker adapter runs this command yet/],
  ]) {
    const before = ran.length;
    assert.throws(() => runStep(ctx(h.env), command, args, { ...at, env: h.env }), (e) => e.code === "STAGE_SIDECAR_STEP_REFUSED" && pattern.test(e.message) && e.message.includes(`step \`${command} ${args.join(" ")}\``), `${command} ${args.join(" ")}`);
    assert.equal(ran.length, before, "a refused step ran nothing");
  }
  // No carrier in the environment: every step runs exactly as before, through the stage's runner.
  ran.length = 0;
  const bare = { PATH: h.env.PATH };
  runStep(ctx(bare), "cmake", ["--build", "b"], { cwd: h.fx.snapshot, env: bare });
  assert.deepEqual(ran.map((r) => [r.command, r.args]), [["cmake", ["--build", "b"]]]);
});

test("pnpm exec reaches a broker adapter only for tauri, right-release and vite; node, sh and an option in the target's place are refused at the door, with or without the snapshot route", { skip: POSIX }, (t) => {
  const h = host(t);
  const CLIENT = path.join(h.fx.root, "rustright");
  const { RIGHTKIT_CARRIED_CARGO_MAP: _, ...bare } = h.env;
  for (const target of EXEC_TARGETS) {
    assert.deepEqual(h.door("pnpm", ["exec", target, "build"]).args.slice(0, 3), [ADAPTER_RUN, "rightkit-pnpm", "--"], target);
    assert.deepEqual(h.door("pnpm", ["--dir", "hub", "exec", target, "build"], { env: bare }), { command: CLIENT, args: ["run", "rightkit-pnpm", "--", "--dir", "hub", "exec", target, "build"] }, target);
  }
  assert.deepEqual([...EXEC_TARGETS].sort(), ["right-release", "tauri", "vite"]);
  for (const args of [["exec", "node", "-e", "require('fs')"], ["exec", "sh", "-c", "id"], ["exec", "rightkit", "cargo", "build"], ["--dir", "hub", "exec", "node", "x.js"], ["exec", "--", "tauri"], ["exec", "-r", "tauri"], ["exec"]]) {
    for (const env of [h.env, bare]) assert.throws(() => h.door("pnpm", args, { env }), refused(/pnpm exec target .* is not allowed \(allowed: tauri, right-release, vite\)/), args.join(" "));
  }
  // `run`'s script name and `--filter`'s value are not exec targets.
  assert.ok(h.door("pnpm", ["--filter", "exec", "run", "node"]).args.includes("rightkit-pnpm"));
});

test("a snapshot call admits pnpm run, exec and pack only: a bare script name is refused at the door naming the step (the broker's snapshotSubcommands would refuse it too); outside the snapshot route it is the client's", { skip: POSIX }, (t) => {
  const h = host(t);
  for (const args of [["run", "build"], ["pack"], ["--dir", "packages/release", "pack", "--pack-destination", "/s/rk-local-publish-x/p"], ["-r", "--workspace-concurrency=1", "run", "test"]]) assert.equal(h.door("pnpm", args).args[1], "rightkit-pnpm", args.join(" "));
  for (const args of [["build"], ["native:assemble", "--", "--profile", "release"], ["windows:package"], ["--dir", "hub", "dev"]]) assert.throws(() => h.door("pnpm", args), refused(new RegExp(`a snapshot stage runs pnpm run, exec and pack only.*pnpm run ${args.find((a) => !a.startsWith("-") && a !== "hub").replace(/[:.]/g, ".")}`)), args.join(" "));
  const { RIGHTKIT_CARRIED_CARGO_MAP: _, ...bare } = h.env;
  assert.deepEqual(h.door("pnpm", ["build"], { env: bare }).args, ["run", "rightkit-pnpm", "--", "build"]);
});
