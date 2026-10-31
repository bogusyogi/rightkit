// The one rule for starting the RustRight client (broker-client.mjs): on win32 the agent shim is never spawned (Node's EINVAL for a
// .cmd/.bat without a shell); its target .exe is, directly, with `shell: false` and the shim's own environment; a shim of another shape
// is refused with RK_BROKER_CLIENT_UNRESOLVED and never falls back to a shell. POSIX is unchanged. Every test here is a data test of the real
// production functions with constructed inputs (a recording spawn, shim text); none is broker evidence.
import "./test-support/broker-guard.mjs";
// build-ownership.mjs first: it, managed-cargo, the target shim and snapshot-broker import one another, and this order is the one that loads.
import "./build-ownership.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { AGENT_SHIM_MARKER, BROKER_CLIENT_UNRESOLVED, brokerClientInvocation, locateBrokerClient, parseAgentShim, resolveBrokerClient, spawnBrokerClient, withoutAgentShimEnv } from "./broker-client.mjs";
import { brokerSection, carryBrokerCargo, createSnapshotBroker, receiptSync } from "./snapshot-broker.mjs";
import { isCallHandleOf } from "./snapshot-request.mjs";

const EXE = "D:\\.rustright\\runtime\\rustright.exe";
const SHIM = (exe = EXE) => `@echo off\r\nrem ${AGENT_SHIM_MARKER}\r\nsetlocal\r\nset "RUSTRIGHT_MANAGED_AGENT_SHIM="\r\nset "RIGHTKIT_MANAGED_AGENT_SHIM="\r\n"${exe}" %*\r\nexit /b %ERRORLEVEL%\r\n`;
const tmp = (t) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-bc-"))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
const shimFile = (t, text) => { const f = path.join(tmp(t), "rustright.cmd"); fs.writeFileSync(f, text); return f; };
const COMMIT = "c".repeat(40);
const TREE = "e".repeat(40);
const HANDLE = "own-7";
/** The ownership hook of a gate that holds a build ownership: asserts the call's sub-handle shape (the real rule) and records it. */
const subHandleStub = (handle) => (callHandle) => {
  assert.ok(isCallHandleOf(callHandle, handle), `a broker submit is recorded for a carried request's sub-handle <handle>#<id>, not ${JSON.stringify(callHandle ?? null)}`);
  return { handle, broker: { submitted: true, handles: [callHandle] } };
};
/** Plain directories a carried call reads (a checkout and a snapshot with one Cargo workspace, a data and a control root) and inert pins. No broker, no client. */
function hostRoots(t) {
  const root = tmp(t);
  const checkout = path.join(root, "checkout"); const dataRoot = path.join(root, "data"); const controlRoot = path.join(root, "control");
  const snapshot = path.join(dataRoot, "snapshots", "0123456789abcdef");
  for (const base of [checkout, snapshot]) { fs.mkdirSync(path.join(base, "crates", "a", "src"), { recursive: true }); fs.writeFileSync(path.join(base, "Cargo.toml"), '[workspace]\nmembers = ["crates/a"]\n'); fs.writeFileSync(path.join(base, "crates", "a", "Cargo.toml"), '[package]\nname = "a"\nversion = "0.1.0"\n'); fs.writeFileSync(path.join(base, "crates", "a", "src", "lib.rs"), "\n"); }
  fs.mkdirSync(controlRoot, { recursive: true });
  const pins = { publicKey: `04${"0".repeat(128)}`, keyId: "k".repeat(64), host: "host-1", brokerDigest: "b".repeat(64) };
  const wsRoot = path.join(dataRoot, "rightkit-build-control", "ws");
  return { root, checkout, dataRoot, controlRoot, snapshot, pins, wsRoot, commit: { sha: COMMIT, tree: TREE }, snapshotRef: { path: snapshot, expectedTree: TREE } };
}
const refused = (fn) => assert.throws(fn, (e) => e.code === BROKER_CLIENT_UNRESOLVED);

test("data: shim parsing: the installed shim yields its quoted exe (CRLF or LF); a missing marker, a different shape and an unsafe exe are refused", () => {
  assert.equal(parseAgentShim(SHIM()), EXE);
  assert.equal(parseAgentShim(SHIM().replaceAll("\r\n", "\n")), EXE);
  refused(() => parseAgentShim(SHIM().replace(`rem ${AGENT_SHIM_MARKER}\r\n`, "")));
  refused(() => parseAgentShim(SHIM().replace(AGENT_SHIM_MARKER, "rustright.build-control.agent-shims.install.v2")));
  refused(() => parseAgentShim(SHIM().replace("exit /b %ERRORLEVEL%", "exit /b %ERRORLEVEL%\r\nrem extra")));
  refused(() => parseAgentShim(SHIM().replace('"D:', 'cmd /c "D:')));
  refused(() => parseAgentShim(SHIM().replace(" %*", " %* & calc")));
  refused(() => parseAgentShim(SHIM().replace("setlocal\r\n", "")));
  refused(() => parseAgentShim(SHIM("D:\\x\\rustright.cmd")));
  refused(() => parseAgentShim(SHIM("rustright.exe")));
  refused(() => parseAgentShim(SHIM("D:\\x&y\\rustright.exe")));
  refused(() => parseAgentShim(SHIM("D:\\%TEMP%\\rustright.exe")));
  refused(() => parseAgentShim('@echo off\r\n"D:\\x\\rustright.exe" %*\r\n'));
});

test("data: win32: a .cmd client is spawned as the shim's .exe with the argv array, shell false and the shim variables cleared; the caller's options are otherwise kept", (t) => {
  const client = shimFile(t, SHIM());
  const calls = [];
  const spawn = (file, args, options) => { calls.push({ file, args, options }); return { status: 0 }; };
  const args = ["cargo", "test", "--locked", '{"a":"b c"}', "x & y"];
  const env = { PATH: "C:\\Windows", Rustright_Managed_Agent_Shim: "1", RIGHTKIT_MANAGED_AGENT_SHIM: "1", KEEP: "1" };
  const r = spawnBrokerClient(spawn, client, args, { cwd: "C:\\w", env, stdio: "inherit" }, { platform: "win32" });
  assert.equal(r.status, 0);
  assert.deepEqual(calls, [{ file: EXE, args, options: { cwd: "C:\\w", env: { PATH: "C:\\Windows", KEEP: "1" }, stdio: "inherit", shell: false } }]);
  assert.equal(env.RIGHTKIT_MANAGED_AGENT_SHIM, "1", "the caller's environment object is not mutated");
  assert.deepEqual(withoutAgentShimEnv({ a: "1", rustright_managed_agent_shim: "x" }), { a: "1" });
  // Without an env in the options the process environment is the base, minus the shim variables.
  spawnBrokerClient(spawn, client, ["status", "1"], {}, { platform: "win32" });
  assert.equal(calls.at(-1).options.env.RUSTRIGHT_MANAGED_AGENT_SHIM, undefined);
  assert.equal(calls.at(-1).options.shell, false);
});

test("data: win32: a client that is not a shim of the installed shape is refused and nothing is spawned (never a shell)", (t) => {
  let spawned = 0;
  const spawn = () => { spawned += 1; return { status: 0 }; };
  for (const text of [SHIM().replace(AGENT_SHIM_MARKER, "other"), '@echo off\r\n"D:\\x\\rustright.exe" %*\r\n', SHIM().replace("%*", "%* extra")]) {
    const r = spawnBrokerClient(spawn, shimFile(t, text), ["cargo"], { env: {} }, { platform: "win32" });
    assert.equal(r.error.code, BROKER_CLIENT_UNRESOLVED);
  }
  assert.equal(spawnBrokerClient(spawn, path.join(tmp(t), "missing.cmd"), ["cargo"], {}, { platform: "win32" }).error.code, BROKER_CLIENT_UNRESOLVED);
  assert.equal(spawned, 0);
  assert.equal(resolveBrokerClient("D:\\a\\rustright.exe", { platform: "win32" }).shim, false, "an .exe is spawned as given");
});

test("data: POSIX is unchanged: the client is spawned as given with the caller's options and environment, even one named like a shim", () => {
  const calls = [];
  const spawn = (file, args, options) => { calls.push({ file, args, options }); return { status: 0 }; };
  const options = { cwd: "/w", env: { RUSTRIGHT_MANAGED_AGENT_SHIM: "1" }, stdio: "inherit" };
  for (const platform of ["darwin", "linux"]) {
    spawnBrokerClient(spawn, "/usr/local/bin/rustright", ["cargo", "build"], options, { platform });
    spawnBrokerClient(spawn, "/opt/rustright.cmd", ["cargo"], options, { platform });
  }
  assert.deepEqual(calls.map((c) => c.file), ["/usr/local/bin/rustright", "/opt/rustright.cmd", "/usr/local/bin/rustright", "/opt/rustright.cmd"]);
  for (const c of calls) { assert.equal(c.options, options, "the options object is passed through untouched"); assert.equal("shell" in c.options, false); }
});

test("data: the carried map carries the exe: a win32 broker built with the .cmd shim hands brokerSection (the map) the resolved .exe, and a refused shim builds no broker", (t) => {
  const fx = hostRoots(t);
  const own = { handle: "h", markBrokerSubmit: subHandleStub("h") };
  const make = (client) => createSnapshotBroker({ env: { RIGHTKIT_DATA_ROOT: fx.dataRoot, RIGHTKIT_CONTROL_ROOT: fx.controlRoot }, platform: "win32", checkout: fx.checkout, commit: fx.commit, snapshot: fx.snapshotRef, ownership: own, client, gateFile: path.join(fx.root, "x", "rightkit-gate.mjs"), loadPins: () => ({ ...fx.pins, source: "enrolled" }) });
  const sb = make(shimFile(t, SHIM()));
  assert.equal(sb.client, EXE);
  assert.equal(brokerSection(sb).client, EXE, "the map section the children read names the exe, never the .cmd");
  assert.equal(brokerSection(sb).platform, "win32");
  assert.throws(() => make(shimFile(t, SHIM().replace(AGENT_SHIM_MARKER, "nope"))), (e) => e.code === BROKER_CLIENT_UNRESOLVED);
  assert.throws(() => make("rustright"), /by absolute path/);
});

test("data: a carried call and a receipt read start the exe directly on win32; a carried call whose client shim is refused is unverified RK_BROKER_CLIENT_UNRESOLVED and starts nothing", (t) => {
  const fx = hostRoots(t);
  const snapshotRoot = fs.realpathSync.native(fx.snapshot);
  const base = brokerSection({ controlRoot: fx.controlRoot, commit: fx.commit, snapshot: fx.snapshotRef, handle: HANDLE, pins: fx.pins, checkout: fx.checkout, outputRoots: [fx.wsRoot], platform: "win32", client: EXE });
  const calls = [];
  const spawn = (file, args, options) => { calls.push({ file, args, options }); return { status: 0, stdout: "", stderr: "" }; };
  const env = { PATH: process.env.PATH, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs", RIGHTKIT_MANAGED_AGENT_SHIM: "1" };
  const carry = (client) => carryBrokerCargo({ words: ["build", "--locked"], cwd: path.join(snapshotRoot, "crates", "a"), env, section: { ...base, markSend: subHandleStub(HANDLE), client, platform: "win32" }, workspace: snapshotRoot, checkoutWorkspace: fx.checkout, record: () => {}, err: () => {}, stdio: "capture", spawn });
  const good = carry(shimFile(t, SHIM()));
  assert.equal(good.outcome.status, "unverified", "the recording spawn prints no receipt");
  assert.equal(calls[0].file, EXE);
  assert.deepEqual(calls[0].args.slice(0, 3), ["cargo", "build", "--locked"]);
  assert.equal(calls[0].options.shell, false);
  assert.equal(calls[0].options.env.RIGHTKIT_MANAGED_AGENT_SHIM, undefined);
  assert.ok(calls.every((c) => c.file === EXE), "the receipt read starts the exe too");
  const before = calls.length;
  const bad = carry(shimFile(t, SHIM().replace(AGENT_SHIM_MARKER, "nope")));
  assert.equal(bad.outcome.code, BROKER_CLIENT_UNRESOLVED);
  assert.equal(calls.length, before, "nothing was started");
  assert.throws(() => receiptSync({ client: shimFile(t, "@echo off\r\n"), requestId: "x", env: {}, spawn, platform: "win32" }), (e) => e.code === "receipt");
});

test("data: [M6] locateBrokerClient and brokerClientInvocation: POSIX keeps the bare name; win32 finds the shim with `where` and starts its .exe with no shell; anything but `rustright` is unchanged; a foreign shim is refused", () => {
  assert.equal(locateBrokerClient({ platform: "darwin" }), "rustright");
  assert.deepEqual(brokerClientInvocation("rustright", ["status", "x"], { cwd: "/c" }, { platform: "darwin" }), { command: "rustright", args: ["status", "x"], options: { cwd: "/c" } });
  assert.deepEqual(brokerClientInvocation("node", ["a"], { cwd: "/c" }, { platform: "win32" }), { command: "node", args: ["a"], options: { cwd: "/c" } });
  const probe = (answer) => (cmd, args) => { assert.equal(cmd, "where"); assert.deepEqual(args, ["rustright"]); return { status: 0, stdout: answer }; };
  const CMD = "C:\\x\\rustright.cmd";
  assert.equal(locateBrokerClient({ platform: "win32", probe: probe(`${CMD}\r\nC:\\y\\rustright.exe\r\n`) }), CMD);
  assert.equal(locateBrokerClient({ platform: "win32", probe: probe("C:\\y\\rustright.exe\r\n") }), "C:\\y\\rustright.exe");
  assert.equal(locateBrokerClient({ platform: "win32", probe: probe("") }), "rustright", "not found: the bare name, which the caller's spawn fails on as before");
  assert.throws(() => locateBrokerClient({ platform: "win32" }), (e) => e.code === BROKER_CLIENT_UNRESOLVED);
  const launched = brokerClientInvocation("rustright", ["run", "x"], { cwd: "/c", env: { A: "1", RUSTRIGHT_MANAGED_AGENT_SHIM: "1" } }, { platform: "win32", probe: probe(`${CMD}\r\n`), readFile: () => SHIM() });
  assert.equal(launched.command, EXE); assert.deepEqual(launched.args, ["run", "x"]);
  assert.equal(launched.options.shell, false); assert.deepEqual(launched.options.env, { A: "1" }); assert.equal(launched.options.cwd, "/c");
  refused(() => brokerClientInvocation("rustright", [], {}, { platform: "win32", probe: probe(`${CMD}\r\n`), readFile: () => "@echo off\r\nother\r\n" }));
});
