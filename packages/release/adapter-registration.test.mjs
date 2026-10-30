// GB-03, GB-04, GB-05: the pnpm argv, the request environment and the signed-artifact handoff the gate sends are judged against the adapter's
// registration BEFORE they are sent, by a port of RustRight's production parser exercised against its vector
// (test-support/vectors/adapter-argv.fixtures.json, pinned in vectors-source.json). The broker is the fixture; no real broker, no Cargo.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { ADAPTER_ENV_MANIFEST, ADAPTER_ENV_VAR, QA_HANDOFF_ENV, adapterEnvRefusal, isReservedAdapterEnvKey } from "./adapter-env.mjs";
import { spawnSync } from "./background-process.mjs";
import { BUILD_LAUNCH_REFUSED, buildLaunch } from "./build-launch.mjs";
import { CARRIED_CARGO_MAP_ENV, installCarrier } from "./carried-cargo.mjs";
import { PNPM_ADAPTER_GRAMMAR, adapterArgvRefusal, grammarFromSpec, scanAdapterArgv } from "./pnpm-adapter-grammar.mjs";
import { brokerSection } from "./snapshot-broker.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";
import { loadVector } from "./test-support/vectors.mjs";

const vector = loadVector("adapter-argv.fixtures.json");
const POSIX = process.platform === "win32" ? "the fixture client and adapter PATH are POSIX only" : false;
const HERE = path.dirname(fileURLToPath(import.meta.url));
const ADAPTER_RUN = path.join(HERE, "carried-adapter-run.mjs");
const GATE = "/gate/bin/rightkit-gate.mjs";

test("[GB-03] the door's reference registration IS RustRight's: subcommands, flags with value kinds, positionals, env, opaque launcher targets, snapshot subcommands", () => {
  const registered = grammarFromSpec(vector.adapter);
  for (const field of ["subcommands", "flags", "positionals", "env", "opaqueAfterTarget", "opaqueTargets", "snapshotSubcommands"]) assert.deepEqual(JSON.parse(JSON.stringify(PNPM_ADAPTER_GRAMMAR[field])), JSON.parse(JSON.stringify(registered[field])), field);
  assert.deepEqual([...ADAPTER_ENV_MANIFEST["rightkit-pnpm"]].sort(), [...vector.adapter.env].sort(), "the candidate env manifest is the registration's env");
});

test("[GB-03] every pnpm argv row of the vector is judged as the production adapter parser judges it, plain and snapshot-bound", (t) => {
  const scratch = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-adapter-vec-")));
  t.after(() => fs.rmSync(scratch, { recursive: true, force: true }));
  fs.mkdirSync(path.join(scratch, "rk-local-publish-a", "work"), { recursive: true });
  fs.mkdirSync(path.join(scratch, "other", "work"), { recursive: true });
  const grammar = grammarFromSpec({ ...vector.adapter, scratchRoot: scratch });
  for (const row of vector.rows) {
    const argv = row.argv.map((w) => w.replaceAll("{scratch}", scratch));
    const expected = row.result === "ok";
    const plain = adapterArgvRefusal(argv, grammar);
    assert.equal(plain === null, expected, `${row.name}: ${argv.join(" ")}: ${plain}`);
    const snapshot = adapterArgvRefusal(argv, grammar, { snapshot: true });
    assert.equal(snapshot === null, row.snapshot ? row.snapshot === "ok" : expected, `${row.name} (snapshot-bound): ${snapshot}`);
  }
});

test("[GB-03] the command token is the registration's, never the first word that is not a flag: no leading option is skipped, and nothing is rewritten", () => {
  assert.equal(scanAdapterArgv(["--dir", "hub", "run", "build"]).command, "run");
  assert.equal(scanAdapterArgv(["--filter", "app", "exec", "tauri", "build"]).target, "tauri");
  // A leading option the registration does not know used to be skipped to find `run`; it is refused with the registration named.
  for (const argv of [["--prefix", "hub", "run", "x"], ["--unknown", "run", "build"], ["--if-present", "run", "build"]]) assert.match(adapterArgvRefusal(argv), /is not registered/, argv.join(" "));
  // run passes its script's own flags opaquely, with or without the separator: both are admitted and neither is rewritten.
  const door = (args) => buildLaunch("pnpm", args, { env: { PATH: "/managed/agent-bin" }, platform: "darwin", exists: (p) => p === "/managed/agent-bin/rustright", which: () => ["/managed/agent-bin/rustright"], readFile: () => "" });
  assert.deepEqual(door(["run", "x", "--flag"]).args, ["run", "rightkit-pnpm", "--", "run", "x", "--flag"]);
  assert.deepEqual(door(["run", "x", "--", "--flag"]).args, ["run", "rightkit-pnpm", "--", "run", "x", "--", "--flag"]);
  assert.deepEqual(door(["run", "--if-present", "x"]).args, ["run", "rightkit-pnpm", "--", "run", "--if-present", "x"]);
  assert.throws(() => door(["--if-present", "run", "x"]), (e) => e.code === BUILD_LAUNCH_REFUSED && /not sent/.test(e.message));
  assert.throws(() => door(["exec", "node", "-e", "process.exit(0)"]), (e) => e.code === BUILD_LAUNCH_REFUSED && /not registered for exec/.test(e.message));
});

test("[GB-04] every env row of the vector is judged as the production check_environment judges it: reserved, relative handoff, unregistered keys named with the registered set", () => {
  for (const row of vector.envRows) {
    const refusal = adapterEnvRefusal(row.env, vector.adapter.env);
    assert.equal(refusal === null, row.result === "ok", `${row.name}: ${refusal?.message}`);
    for (const word of row.mentions ?? []) assert.ok(refusal?.message.includes(word), `${row.name} mentions ${word}: ${refusal?.message}`);
  }
  assert.equal(isReservedAdapterEnvKey("RIGHTKIT_QA_APP_BINARY"), false, "exactly the two handoff names are exempt from the RIGHTKIT_ prefix");
  assert.equal(isReservedAdapterEnvKey("RIGHTKIT_QA_TIER"), true);
});

test("[GB-04] an unregistered key refuses at the door before any process, naming every key and the registered set; nothing is dropped", () => {
  const door = (stepEnv, adapterProgram = "pnpm", args = ["run", "x"], host = {}) => buildLaunch(adapterProgram, args, { env: { PATH: "/managed/agent-bin" }, platform: "darwin", exists: (p) => p === "/managed/agent-bin/rustright", which: () => ["/managed/agent-bin/rustright"], readFile: () => "", cwd: "/app", stepEnv, ...host });
  assert.equal(door({ RK_ISOLATED_CHECKOUT: "1" }).env.RK_ISOLATED_CHECKOUT, "1");
  assert.throws(() => door({ RK_OTHER: "1", RK_ZED: "2" }), (e) => e.code === BUILD_LAUNCH_REFUSED && e.reason === "RK_ADAPTER_ENV_UNREGISTERED" && ["RK_OTHER", "RK_ZED", "RK_ISOLATED_CHECKOUT"].every((w) => e.message.includes(w)));
  assert.throws(() => door({ RK_ISOLATED_CHECKOUT: "1", RK_OTHER: "1" }), (e) => e.message.includes("RK_OTHER"), "a registered key beside an unregistered one still refuses");
  assert.throws(() => door({ RK_ISOLATED_CHECKOUT: "1" }, "swift", ["build"]), (e) => e.reason === "RK_ADAPTER_ENV_UNREGISTERED", "swift registers no key");
  // The live registration wins over the manifest: a host whose registration lists no handoff names refuses the handoff before launch.
  const live = { readFile: () => JSON.stringify({ adapters: { "rightkit-pnpm": { subcommands: ["run"], flags: [], env: ["RK_ISOLATED_CHECKOUT"], positionals: "relpath", opaqueAfterTarget: ["run"] } } }) };
  assert.throws(() => door({ [QA_HANDOFF_ENV[0]]: "/signed/app" }, "pnpm", ["run", "x"], live), (e) => e.reason === "RK_ADAPTER_ENV_UNREGISTERED" && e.message.includes(QA_HANDOFF_ENV[0]));
  // Reserved names stay refused whatever a registration says.
  assert.throws(() => door({ CARGO_HOME: "/x" }), (e) => e.reason === "RK_ADAPTER_ENV_RESERVED");
});

// ---- GB-05: the signed-artifact handoff across the adapter and Cargo boundaries ----

function host(t, behavior = {}) {
  const fx = makeFakeBroker(t, { readOnlySnapshots: true });
  fx.setBehavior({ registeredEnv: { "rightkit-pnpm": [...vector.adapter.env], "rightkit-node": [...vector.adapter.env] }, ...behavior });
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout }, section: brokerSection(fx.sb), platform: process.platform, env: {} });
  t.after(() => installed.cleanup());
  const signed = { [QA_HANDOFF_ENV[0]]: path.join(fx.root, "signed", "app"), [QA_HANDOFF_ENV[1]]: path.join(fx.root, "signed", "evidence.json") };
  const env = { PATH: fx.root, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE, ...fx.ownershipEnv, ...installed.env };
  const door = (command, args, over = {}) => buildLaunch(command, args, { env, platform: "darwin", exists: (p) => p === path.join(fx.root, "rustright"), cwd: fx.snapshot, ...over });
  const run = (launch, runEnv) => spawnSync(launch.command, launch.args, { cwd: fx.snapshot, env: { ...runEnv, ...(launch.env ?? {}) }, encoding: "utf8" });
  return { fx, env, signed, door, run, installed };
}

test("[GB-05] a pnpm or node consumer after a sign step receives exactly the signed binary and evidence paths as the adapter request's environment; they are part of the signed request", { skip: POSIX }, (t) => {
  const h = host(t);
  const envAfterSign = { ...h.env, ...h.signed };
  const launch = h.door("pnpm", ["run", "qa"], { env: envAfterSign });
  assert.equal(launch.env[ADAPTER_ENV_VAR], "RIGHTKIT_QA_APP_BINARY,RIGHTKIT_QA_SIGNING_EVIDENCE");
  assert.deepEqual({ a: launch.env.RIGHTKIT_QA_APP_BINARY, b: launch.env.RIGHTKIT_QA_SIGNING_EVIDENCE }, { a: h.signed.RIGHTKIT_QA_APP_BINARY, b: h.signed.RIGHTKIT_QA_SIGNING_EVIDENCE });
  const r = h.run(launch, envAfterSign);
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(h.fx.adapterCalls()[0].forwardedEnv, h.signed, "the broker job received the exact signed paths");
  // Before a sign step there is nothing to forward, and no RUSTRIGHT_ADAPTER_ENV at all.
  assert.equal(Object.hasOwn(h.door("pnpm", ["run", "qa"]), "env"), false);
  // A changed path is a changed request: the same argv with another evidence path signs another digest (never coalesced, GB-04).
  const node = h.door("node", ["scripts/qa.mjs"], { env: envAfterSign, cwd: h.fx.snapshot });
  assert.equal(node.env[ADAPTER_ENV_VAR], "RIGHTKIT_QA_APP_BINARY,RIGHTKIT_QA_SIGNING_EVIDENCE");
  // A relative or escaping path is refused before launch; the consumer is never started with a path that is not the signed artifact's.
  for (const bad of ["app", "../signed/app", "/signed/../app"]) assert.throws(() => h.door("pnpm", ["run", "qa"], { env: { ...h.env, RIGHTKIT_QA_APP_BINARY: bad } }), (e) => e.code === BUILD_LAUNCH_REFUSED && e.reason === "RK_ADAPTER_ENV_HANDOFF_PATH", bad);
  // No signing key or credential rides along: only the two names.
  assert.deepEqual(Object.keys(launch.env).sort(), [ADAPTER_ENV_VAR, ...QA_HANDOFF_ENV].sort());
});

test("[GB-05] a Cargo suite consumer (test, bench, run, nextest) is told to forward the two handoff names; a build or a missing handoff forwards none", { skip: POSIX }, (t) => {
  const h = host(t);
  const envAfterSign = { ...h.env, ...h.signed };
  for (const args of [["test", "--locked"], ["t"], ["bench"], ["run", "-p", "x"], ["nextest", "run"]]) {
    assert.equal(h.door("cargo", args, { env: envAfterSign }).env?.RUSTRIGHT_FORWARD_ENV, "RIGHTKIT_QA_APP_BINARY,RIGHTKIT_QA_SIGNING_EVIDENCE", `cargo ${args.join(" ")}`);
  }
  for (const args of [["build", "--locked"], ["check"], ["clippy"], ["metadata", "--no-deps"]]) assert.equal(Object.hasOwn(h.door("cargo", args, { env: envAfterSign }), "env"), false, `cargo ${args.join(" ")} is no runtime command`);
  assert.equal(Object.hasOwn(h.door("cargo", ["test"]), "env"), false, "no sign step ran: nothing to forward");
  // An existing forward list is kept and extended, never replaced.
  assert.equal(h.door("cargo", ["test"], { env: { ...envAfterSign, RUSTRIGHT_FORWARD_ENV: "FFMPEG_PATH" } }).env.RUSTRIGHT_FORWARD_ENV, "FFMPEG_PATH,RIGHTKIT_QA_APP_BINARY,RIGHTKIT_QA_SIGNING_EVIDENCE");
  assert.throws(() => h.door("cargo", ["test"], { env: { ...h.env, RIGHTKIT_QA_SIGNING_EVIDENCE: "evidence.json" } }), (e) => e.code === BUILD_LAUNCH_REFUSED && /absolute path/.test(e.message));
  // On the carried route the forward list rides to the carrier's client call (the door returns the environment to merge, the values are the caller's).
  const launch = h.door("cargo", ["test", "--locked"], { env: envAfterSign });
  assert.equal(path.basename(launch.args[0]), "carried-cargo-run.mjs");
});
