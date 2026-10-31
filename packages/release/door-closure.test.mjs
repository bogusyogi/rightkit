// rightkit#1301, one model (rustright#190): every compile of a snapshot stage on a Mac or Dell broker host is a carried request or nested under a
// carried, snapshot-bound adapter job. This file pins the steps that used to reach the broker client uncarried (a swift step, a non-Cargo
// `rustright` step, a stage-sidecar command or cmake step) and the door's own admission rules (the pnpm exec target allowlist, run/exec/pack
// only in a snapshot call). The host is an isolated instance of the REAL installed RustRight broker (test-support/real-broker.mjs); the door's planning
// (buildLaunch, runStep, the native-helpers seam) needs no broker answer, and a step that is actually run reaches the real broker. No fake broker, no Cargo, no Swift.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { BUILD_LAUNCH_REFUSED, EXEC_TARGETS, buildLaunch } from "./build-launch.mjs";
import { PNPM_ADAPTER_GRAMMAR } from "./pnpm-adapter-grammar.mjs";
import { readLedger, UNSCOPED } from "./cargo-ledger.mjs";
import { installCarrier } from "./carried-cargo.mjs";
import { buildAndEmbedNativeHelpers } from "./native-helpers.mjs";
import { brokerSection } from "./snapshot-broker.mjs";
import { runStep } from "./stage-sidecar.mjs";
import { spawnSync } from "./background-process.mjs";
import { startRealBroker } from "./test-support/real-broker.mjs";
import { OWNERSHIP_ENV, OWNERSHIP_FILE_ENV, markCarriedSend } from "./build-ownership.mjs";

const POSIX = process.platform === "win32" ? "the carried adapter PATH is POSIX only" : false;
const MAC = process.platform === "darwin" ? false : "the broker route is the Mac and Windows hosts' (resolveExecutor); this host runs direct";
const GATE = "/gate/bin/rightkit-gate.mjs";
const HERE = path.dirname(fileURLToPath(import.meta.url));
const ADAPTER_RUN = path.join(HERE, "carried-adapter-run.mjs");

const COMMIT = "c".repeat(40); const TREE = "e".repeat(40); const HANDLE = "own-7";
const WORKSPACES = { "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n', "crates/a/src/lib.rs": "\n" };
/** The gate's pins for this host. They are the gate's own record; nothing in this file verifies a receipt against them. */
function hostPins() {
  const { publicKey } = crypto.generateKeyPairSync("ec", { namedCurve: "P-256" });
  const jwk = publicKey.export({ format: "jwk" });
  const sec1 = Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x, "base64url"), Buffer.from(jwk.y, "base64url")]).toString("hex");
  return { publicKey: sec1, keyId: crypto.createHash("sha256").update(Buffer.from(sec1, "hex")).digest("hex"), host: "host-1", brokerDigest: "b".repeat(64) };
}

/**
 * A broker host with a carrier on the REAL installed broker: `rb` is an isolated instance (its own socket and roots), `fx` the checkout, the gate's snapshot
 * and the build-ownership record around it, `env` a snapshot stage's (the map in it, PATH holding the real client), `door` a buildLaunch from the snapshot.
 */
async function host(t) {
  const rb = await startRealBroker({ name: "door-closure" });
  t.after(() => rb.stop());
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-door-closure-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checkout = path.join(root, "checkout"); const snapshot = path.join(root, "snapshots", "0123456789abcdef");
  for (const dir of [checkout, snapshot]) for (const [rel, body] of Object.entries(WORKSPACES)) { const f = path.join(dir, rel); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, body); }
  const ownership = path.join(root, "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(ownership), { recursive: true });
  fs.writeFileSync(ownership, JSON.stringify({ handle: HANDLE, broker: { submitted: false } }));
  const sb = {
    controlRoot: rb.controlRoot, commit: { sha: COMMIT, tree: TREE }, snapshot: { path: snapshot, expectedTree: TREE }, declaredInputs: [], handle: HANDLE, pins: hostPins(),
    markBrokerSubmit: (callHandle) => markCarriedSend({ file: ownership, handle: HANDLE, callHandle }),
    checkout, outputRoots: [path.join(rb.dataRoot, "rightkit-build-control", "ws")], platform: process.platform, client: rb.clientPath,
  };
  const fx = { root, checkout, snapshot, client: rb.clientPath, sb, ownershipEnv: { [OWNERSHIP_ENV]: HANDLE, [OWNERSHIP_FILE_ENV]: ownership } };
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(root, "carrier-")), snapshotRoot: snapshot, checkoutDirs: { ".": checkout }, section: brokerSection(sb), platform: process.platform, env: {} });
  t.after(() => installed.cleanup());
  const env = { ...rb.env, PATH: path.dirname(rb.clientPath), HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE, ...fx.ownershipEnv, ...installed.env };
  const door = (command, args, over = {}) => buildLaunch(command, args, { env, platform: "darwin", exists: (p) => p === rb.clientPath, cwd: snapshot, ...over });
  const entries = () => readLedger(installed.ledgerDir, UNSCOPED);
  return { rb, fx, installed, env, door, entries };
}
const refused = (pattern) => (error) => error.code === BUILD_LAUNCH_REFUSED && pattern.test(error.message);

test("a swift step of a snapshot stage is planned as a carried rightkit-swift adapter job, not an uncarried client call, and run against the real broker it never verifies for a snapshot it does not hold; without a carrier it is the client's, as before", { skip: POSIX }, async (t) => {
  const h = await host(t);
  const args = ["build", "-c", "release", "--package-path", "macos-native/Pkg", "--product", "tool", "--arch", "arm64", "--arch", "x86_64"];
  const launch = h.door("swift", args);
  assert.deepEqual(launch, { command: process.execPath, args: [ADAPTER_RUN, "rightkit-swift", "--", ...args] });
  // Run it against the real broker: the snapshot is the gate's own directory, not one this broker holds, so the call is never a verified one.
  const ran = spawnSync(launch.command, launch.args, { cwd: h.fx.snapshot, env: h.env, encoding: "utf8" });
  assert.notEqual(ran.status, 0, "the real broker holds no such snapshot: the carried job does not succeed");
  assert.ok(h.entries().entries.every((e) => e.status !== "verified"), "nothing the real broker did not complete is a verified ledger entry");
  // Swift's own grammar in a snapshot call: `build` only, one relative --package-path (a bad step never succeeds against the real broker or at the door).
  for (const bad of [["package", "resolve"], ["build", "--package-path", "/elsewhere/pkg"], ["build", "--product", "x"]]) {
    const denied = spawnSync(process.execPath, [ADAPTER_RUN, "rightkit-swift", "--", ...bad], { cwd: h.fx.snapshot, env: h.env, encoding: "utf8" });
    assert.notEqual(denied.status, 0, bad.join(" "));
  }
  // No carrier map: the client's `run rightkit-swift`, unchanged.
  const client = h.fx.client;
  const { RIGHTKIT_CARRIED_CARGO_MAP: _, ...bare } = h.env;
  assert.deepEqual(h.door("swift", args, { env: bare }), { command: client, args: ["run", "rightkit-swift", "--", ...args] });
});

test("a Swift package outside the commit snapshot is refused with NATIVE_HELPERS_SNAPSHOT_SOURCE; sharedPackage is retired; a package inside the snapshot is built as a carried swift job from the snapshot", { skip: MAC }, async (t) => {
  const h = await host(t);
  const app = path.join(h.fx.snapshot, "app");
  fs.mkdirSync(path.join(app, "dist", "Demo.app", "Contents"), { recursive: true });
  fs.writeFileSync(path.join(app, "dist", "Demo.app", "Contents", "Info.plist"), "<plist/>");
  // A package directory that is a symlink out of the app resolves outside the snapshot.
  const editable = path.join(h.fx.root, "editable-rightkit");
  fs.mkdirSync(path.join(editable, "swift", "RightKitCloudKitHelper"), { recursive: true });
  fs.writeFileSync(path.join(editable, "swift", "RightKitCloudKitHelper", "Package.swift"), "// package\n");
  fs.symlinkSync(path.join(editable, "swift", "RightKitCloudKitHelper"), path.join(app, "linked-pkg"));
  const cloud = { name: "cloud", package: "linked-pkg", product: "rightkit-cloudkit-helper", kind: "app", embed: "Contents/Library/LoginItems/Cloud.app", universal: true, executable: "Cloud", info: { bundleId: "com.example.next", name: "Cloud" } };
  const calls = [];
  const spawn = (command, args) => { calls.push({ command, args }); return { status: 0, stdout: "", stderr: "" }; };
  assert.throws(
    () => buildAndEmbedNativeHelpers({ appRoot: app, app: path.join(app, "dist", "Demo.app"), helpers: [cloud], version: "1.0.0", env: h.env, spawn }),
    (e) => e.code === "NATIVE_HELPERS_SNAPSHOT_SOURCE" && /outside this stage's commit snapshot/.test(e.message) && /declare a package inside the app's own repository/.test(e.message),
  );
  // sharedPackage is retired (rightkit#1302): refused before anything runs, naming the replacement; RIGHTKIT_SWIFT_ROOT is no longer read.
  assert.throws(
    () => buildAndEmbedNativeHelpers({ appRoot: app, app: path.join(app, "dist", "Demo.app"), helpers: [{ ...cloud, package: undefined, sharedPackage: "cloudkit" }], version: "1.0.0", env: { ...h.env, RIGHTKIT_SWIFT_ROOT: editable }, spawn }),
    (e) => e.code === "NATIVE_HELPERS_INVALID" && /sharedPackage is retired/.test(e.message) && /package: "<the app's SwiftPM package directory>"/.test(e.message),
  );
  assert.deepEqual(calls, [], "nothing ran: not a swift job, not a client call");
  // The app's own package, committed into the snapshot, builds: the first call is the carried adapter job, from the snapshot root.
  fs.mkdirSync(path.join(app, "macos-native", "Pkg"), { recursive: true });
  fs.writeFileSync(path.join(app, "macos-native", "Pkg", "Package.swift"), "// package\n");
  const own = { ...cloud, package: "macos-native/Pkg" };
  assert.throws(() => buildAndEmbedNativeHelpers({ appRoot: app, app: path.join(app, "dist", "Demo.app"), helpers: [own], version: "1.0.0", forceArch: "arm64", env: h.env, spawn }), (e) => e.code === "NATIVE_HELPERS_STEP_FAILED" && /no absolute path/.test(e.message), "the stub spawn prints no bin path: the build was attempted, so the package passed the snapshot check");
  assert.equal(calls[0].command, process.execPath);
  assert.deepEqual(calls[0].args.slice(0, 3), [ADAPTER_RUN, "rightkit-swift", "--"]);
  assert.ok(calls[0].args.includes(path.join("app", "macos-native", "Pkg")), "the package path is relative to the snapshot root, which is the job's directory");
});

test("a non-Cargo `rustright` step on the snapshot route is carried when it names a registered adapter (judged by that program's door rules) and refused with its code otherwise", { skip: POSIX }, async (t) => {
  const h = await host(t);
  // `rustright run <adapter> -- <args>` is the adapter's own step.
  assert.deepEqual(h.door("rustright", ["run", "rightkit-pnpm", "--", "exec", "tauri", "build"]), { command: process.execPath, args: [ADAPTER_RUN, "rightkit-pnpm", "--", "exec", "tauri", "build"] });
  assert.deepEqual(h.door("rustright", ["run", "rightkit-swift", "--", "build", "--package-path", "p"]).args.slice(0, 3), [ADAPTER_RUN, "rightkit-swift", "--"]);
  assert.deepEqual(h.door("rustright", ["run", "rightkit-node", "--", "scripts/x.mjs", "--platform", "win"]).args, [ADAPTER_RUN, "rightkit-node", "--", "scripts/x.mjs", "--platform", "win"]);
  // The program's rules still apply: this is not a way around the node flag ban or the exec target allowlist.
  assert.throws(() => h.door("rustright", ["run", "rightkit-node", "--", "-e", "process.exit(0)"]), refused(/node flag -e is not allowed/));
  assert.throws(() => h.door("rustright", ["run", "rightkit-pnpm", "--", "exec", "node", "-e", "1"]), refused(/target "node" is not registered for exec \(registered targets: tauri\)/));
  // Anything else is an uncarried client call: refused, naming the step.
  for (const args of [["status", "abc"], ["run", "rightkit-unknown", "--", "x"], ["run", "rightkit-pnpm", "x"], ["doctor"], []]) {
    assert.throws(() => h.door("rustright", args), refused(new RegExp(`rustright ${args.join(" ").replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}: a snapshot stage carries every compile`)), args.join(" "));
  }
  // `rustright cargo` stays the carried Cargo seam; with no carrier the client takes every rustright command as before.
  assert.equal(h.door("rustright", ["cargo", "build", "--locked"]).args[0].endsWith("carried-cargo-run.mjs"), true);
  const { RIGHTKIT_CARRIED_CARGO_MAP: _, ...bare } = h.env;
  assert.deepEqual(h.door("rustright", ["status", "abc"], { env: bare }), { command: h.fx.client, args: ["status", "abc"] });
});

test("stage-sidecar's step commands (stage.cmd, a command entry's cmd, cmake) of a carried stage are carried adapter jobs, the carried Cargo seam or the door's own right-release, and any other is refused with STAGE_SIDECAR_STEP_REFUSED", { skip: MAC }, async (t) => {
  const h = await host(t);
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
    ["pnpm", ["exec", "node", "-e", "1"], /target "node" is not registered for exec/],
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

test("pnpm exec reaches a broker adapter only for a target the registration admits and the door allows (GB-03: the reference registration admits tauri only); node, sh and an option in the target's place are refused, with or without the snapshot route", { skip: POSIX }, async (t) => {
  const h = await host(t);
  const CLIENT = h.fx.client;
  const { RIGHTKIT_CARRIED_CARGO_MAP: _, ...bare } = h.env;
  assert.deepEqual([...EXEC_TARGETS].sort(), ["right-release", "tauri", "vite"], "the door's own allowlist");
  // The reference registration (RustRight's `opaqueTargets: {exec: [tauri]}`) admits tauri alone: the other two are refused at planning with the registration named.
  for (const target of ["right-release", "vite"]) assert.throws(() => h.door("pnpm", ["exec", target, "build"]), refused(new RegExp(`target "${target}" is not registered for exec \\(registered targets: tauri\\)`)), target);
  // A registration that lists all three (an operator's choice, read live) admits each, still never an arbitrary executable.
  const registered = { ...PNPM_ADAPTER_GRAMMAR, opaqueTargets: { exec: ["tauri", "right-release", "vite"] } };
  for (const target of EXEC_TARGETS) {
    assert.deepEqual(h.door("pnpm", ["exec", target, "build"], { pnpmGrammar: registered }).args.slice(0, 3), [ADAPTER_RUN, "rightkit-pnpm", "--"], target);
    assert.deepEqual(h.door("pnpm", ["--dir", "hub", "exec", target, "build"], { env: bare, pnpmGrammar: registered }), { command: CLIENT, args: ["run", "rightkit-pnpm", "--", "--dir", "hub", "exec", target, "build"] }, target);
  }
  assert.deepEqual(h.door("pnpm", ["exec", "tauri", "build"]).args.slice(0, 3), [ADAPTER_RUN, "rightkit-pnpm", "--"]);
  for (const args of [["exec", "node", "-e", "require('fs')"], ["exec", "sh", "-c", "id"], ["exec", "rightkit", "cargo", "build"], ["--dir", "hub", "exec", "node", "x.js"], ["exec", "-r", "tauri"], ["exec"], ["exec", "--", "node", "-e", "1"]]) {
    for (const env of [h.env, bare]) for (const grammar of [undefined, registered]) assert.throws(() => h.door("pnpm", args, { env, pnpmGrammar: grammar }), refused(/is not registered for exec|is not allowed \(allowed: tauri, right-release, vite\)|is not registered for rightkit-pnpm|requires a target|not allowed/), `${args.join(" ")} ${grammar ? "(wide registration)" : ""}`);
  }
  // `run`'s script name and `--filter`'s value are not exec targets.
  assert.ok(h.door("pnpm", ["--filter", "exec", "run", "node"]).args.includes("rightkit-pnpm"));
});

test("a snapshot call admits pnpm run, exec and pack only: a bare script name is refused at the door naming the step (the broker's snapshotSubcommands would refuse it too); outside the snapshot route it is the client's", { skip: POSIX }, async (t) => {
  const h = await host(t);
  for (const args of [["run", "build"], ["pack"], ["--dir", "packages/release", "pack", "--pack-destination", "/s/rk-local-publish-x/p"], ["-r", "--workspace-concurrency=1", "run", "test"]]) assert.equal(h.door("pnpm", args).args[1], "rightkit-pnpm", args.join(" "));
  // GB-03: the registration's parser refuses a bare script name (no registered first argument), and a registered command that is not snapshot-bound (install).
  for (const args of [["build"], ["native:assemble", "--", "--profile", "release"], ["windows:package"], ["--dir", "hub", "dev"]]) assert.throws(() => h.door("pnpm", args), refused(/is not registered for rightkit-pnpm|is not allowed in a snapshot-bound call/), args.join(" "));
  assert.equal(h.door("pnpm", ["--dir", "hub", "install"]).args[1], "--skip-install", "an install step of a snapshot stage is skipped before the grammar is asked (the gate installed the snapshot)");
  const { RIGHTKIT_CARRIED_CARGO_MAP: _, ...bare } = h.env;
  assert.deepEqual(h.door("pnpm", ["run", "build"], { env: bare }).args, ["run", "rightkit-pnpm", "--", "run", "build"]);
  // The broker's rightkit-pnpm grammar (M12): a bare script name is no registered first argument on any route, so the door refuses it too.
  assert.throws(() => h.door("pnpm", ["build"], { env: bare }), refused(/subcommand "build" is not registered for rightkit-pnpm/));
});
