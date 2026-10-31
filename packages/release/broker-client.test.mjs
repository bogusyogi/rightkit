// The one rule for starting the RustRight client (broker-client.mjs): on win32 the agent shim is never spawned (Node's EINVAL for a
// .cmd/.bat without a shell); its target .exe is, directly, with `shell: false` and the shim's own environment; a shim of another shape
// is refused with RK_BROKER_CLIENT_UNRESOLVED and never falls back to a shell. POSIX is unchanged. No real broker.
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
import { makeFakeBroker, subHandleStub } from "./test-support/fake-broker.mjs";

const EXE = "D:\\.rustright\\runtime\\rustright.exe";
const SHIM = (exe = EXE) => `@echo off\r\nrem ${AGENT_SHIM_MARKER}\r\nsetlocal\r\nset "RUSTRIGHT_MANAGED_AGENT_SHIM="\r\nset "RIGHTKIT_MANAGED_AGENT_SHIM="\r\n"${exe}" %*\r\nexit /b %ERRORLEVEL%\r\n`;
const tmp = (t) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-bc-"))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
const shimFile = (t, text) => { const f = path.join(tmp(t), "rustright.cmd"); fs.writeFileSync(f, text); return f; };
const refused = (fn) => assert.throws(fn, (e) => e.code === BROKER_CLIENT_UNRESOLVED);

test("shim parsing: the installed shim yields its quoted exe (CRLF or LF); a missing marker, a different shape and an unsafe exe are refused", () => {
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

test("win32: a .cmd client is spawned as the shim's .exe with the argv array, shell false and the shim variables cleared; the caller's options are otherwise kept", (t) => {
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

test("win32: a client that is not a shim of the installed shape is refused and nothing is spawned (never a shell)", (t) => {
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

test("POSIX is unchanged: the client is spawned as given with the caller's options and environment, even one named like a shim", () => {
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

test("the carried map carries the exe: a win32 broker built with the .cmd shim hands brokerSection (the map) the resolved .exe, and a refused shim builds no broker", (t) => {
  const fx = makeFakeBroker(t);
  const own = { handle: "h", markBrokerSubmit: subHandleStub("h") };
  const make = (client) => createSnapshotBroker({ env: { RIGHTKIT_DATA_ROOT: fx.dataRoot, RIGHTKIT_CONTROL_ROOT: fx.controlRoot }, platform: "win32", checkout: fx.checkout, commit: fx.sb.commit, snapshot: fx.sb.snapshot, ownership: own, client, gateFile: path.join(fx.root, "x", "rightkit-gate.mjs"), loadPins: () => ({ ...fx.pins, source: "enrolled" }) });
  const sb = make(shimFile(t, SHIM()));
  assert.equal(sb.client, EXE);
  assert.equal(brokerSection(sb).client, EXE, "the map section the children read names the exe, never the .cmd");
  assert.equal(brokerSection(sb).platform, "win32");
  assert.throws(() => make(shimFile(t, SHIM().replace(AGENT_SHIM_MARKER, "nope"))), (e) => e.code === BROKER_CLIENT_UNRESOLVED);
  assert.throws(() => make("rustright"), /by absolute path/);
});

test("a carried call and a receipt read start the exe directly on win32; a carried call whose client shim is refused is unverified RK_BROKER_CLIENT_UNRESOLVED and starts nothing", (t) => {
  const fx = makeFakeBroker(t, {});
  const snapshotRoot = fs.realpathSync.native(fx.snapshot);
  const calls = [];
  const spawn = (file, args, options) => { calls.push({ file, args, options }); return { status: 0, stdout: "", stderr: "" }; };
  const env = { PATH: process.env.PATH, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs", RIGHTKIT_MANAGED_AGENT_SHIM: "1" };
  const carry = (client) => carryBrokerCargo({ words: ["build", "--locked"], cwd: path.join(snapshotRoot, "crates", "a"), env, section: { ...brokerSection(fx.sb), markSend: (h) => ({ broker: { submitted: true, handles: [h] } }), client, platform: "win32" }, workspace: snapshotRoot, checkoutWorkspace: fx.checkout, record: () => {}, err: () => {}, stdio: "capture", spawn });
  const good = carry(shimFile(t, SHIM()));
  assert.equal(good.outcome.status, "unverified", "the stub client prints no receipt");
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

test("[M6] locateBrokerClient and brokerClientInvocation: POSIX keeps the bare name; win32 finds the shim with `where` and starts its .exe with no shell; anything but `rustright` is unchanged; a foreign shim is refused", () => {
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

// ---- GATE-3 (audit N33, B7, M46, M47) ----
import { BROKER_CLIENT_QUERY_TIMEOUT_MS, BROKER_UNAVAILABLE, LEGACY_AGENT_SHIM_MARKER, OUTAGE_RECONNECT_WAIT_MS, brokerStagePreflight, spawnBrokerClientAsync, spawnClientProcess, withOutageReconnectBound } from "./broker-client.mjs";

test("[N33] a shim installed before the rename (legacy marker, the same seven lines) is read as the installed shim; any other marker or shape is still refused", () => {
  assert.equal(LEGACY_AGENT_SHIM_MARKER, "rightkit.build-control.agent-shims.install.v1");
  const legacy = SHIM().replace(AGENT_SHIM_MARKER, LEGACY_AGENT_SHIM_MARKER);
  assert.equal(parseAgentShim(legacy), EXE);
  assert.equal(parseAgentShim(legacy.replaceAll("\r\n", "\n")), EXE);
  assert.equal(resolveBrokerClient("C:\\agent-bin\\rustright.cmd", { platform: "win32", readFile: () => legacy }).file, EXE);
  assert.throws(() => parseAgentShim(SHIM().replace(AGENT_SHIM_MARKER, "rightkit.build-control.agent-shims.install.v0")), (e) => e.code === BROKER_CLIENT_UNRESOLVED);
  assert.throws(() => parseAgentShim(`${legacy}rem extra\r\n`), (e) => e.code === BROKER_CLIENT_UNRESOLVED, "a legacy marker does not loosen the exact shape");
  assert.throws(() => parseAgentShim(legacy.replace("setlocal", "setlocal enabledelayedexpansion")), (e) => e.code === BROKER_CLIENT_UNRESOLVED);
});

test("[M46] short broker queries wait at least as long as the Windows client retries a busy pipe (120 s)", () => {
  assert.ok(BROKER_CLIENT_QUERY_TIMEOUT_MS >= 130_000);
  const seen = [];
  brokerStagePreflight({ client: "/x/rustright", platform: "linux", spawn: (f, a, o) => { seen.push({ a, o }); return { status: 0 }; } });
  assert.ok(seen[0].o.timeout >= 130_000);
});

test("[M47] the per-stage preflight is one `rustright doctor` question: READY passes, a dead broker or a client that cannot run is RK_BROKER_UNAVAILABLE before any carried call; its reconnect budget is short", () => {
  const seen = [];
  const run = (status, error) => (f, a, o) => { seen.push({ f, a, o }); return error ? { error } : { status }; };
  assert.deepEqual(brokerStagePreflight({ client: "/x/rustright", platform: "linux", spawn: run(0) }), { ok: true });
  assert.deepEqual(seen[0].a, ["doctor", "--json", "--no-scan"]);
  assert.equal(seen[0].o.env.RUSTRIGHT_RECONNECT_WAIT_MS, String(OUTAGE_RECONNECT_WAIT_MS));
  const down = brokerStagePreflight({ client: "/x/rustright", platform: "linux", spawn: run(1) });
  assert.equal(down.ok, false); assert.equal(down.code, BROKER_UNAVAILABLE); assert.match(down.message, /not READY/);
  const noRun = brokerStagePreflight({ client: "/x/rustright", platform: "linux", spawn: run(null, Object.assign(new Error("spawn ETIMEDOUT"), { code: "ETIMEDOUT" })) });
  assert.equal(noRun.code, BROKER_UNAVAILABLE);
  const shim = brokerStagePreflight({ client: "C:\\x\\rustright.cmd", platform: "win32", readFile: () => "@echo off\r\n", spawn: run(0) });
  assert.equal(shim.code, BROKER_CLIENT_UNRESOLVED);
  assert.equal(withOutageReconnectBound({ RUSTRIGHT_RECONNECT_WAIT_MS: "900000", X: "1" }).RUSTRIGHT_RECONNECT_WAIT_MS, "10000");
  assert.equal(withOutageReconnectBound({ rustright_reconnect_wait_ms: "500" }).RUSTRIGHT_RECONNECT_WAIT_MS, "500");
  assert.equal(withOutageReconnectBound({ RUSTRIGHT_RECONNECT_WAIT_MS: "junk" }).RUSTRIGHT_RECONNECT_WAIT_MS, "10000");
});

test("[B7/M45] spawnClientProcess is async, kills a client that outlives its timeout (ETIMEDOUT, killed), obeys an AbortSignal, reports a spawn failure as pid 0, and resolves the shim like the sync spawn", { skip: process.platform === "win32" }, async () => {
  const node = process.execPath;
  const ok = await spawnClientProcess(node, ["-e", "process.stdout.write('hi'); process.stderr.write('lo')"], { encoding: "utf8" });
  assert.equal(ok.status, 0); assert.equal(ok.stdout, "hi"); assert.equal(ok.stderr, "lo");
  const started = Date.now();
  const slow = await spawnClientProcess(node, ["-e", "setTimeout(() => {}, 30000)"], { timeout: 200 });
  assert.ok(Date.now() - started < 5000);
  assert.equal(slow.timedOut, true); assert.equal(slow.error.code, "ETIMEDOUT"); assert.equal(slow.signal, "SIGTERM");
  const ac = new AbortController(); setTimeout(() => ac.abort(), 150);
  const aborted = await spawnClientProcess(node, ["-e", "setTimeout(() => {}, 30000)"], { signal: ac.signal });
  assert.equal(aborted.aborted, true); assert.equal(aborted.signal, "SIGTERM");
  const missing = await spawnClientProcess("/no/such/client", [], {});
  assert.equal(missing.error.code, "ENOENT"); assert.ok(!missing.pid);
  const calls = [];
  const viaShim = await spawnBrokerClientAsync(async (f, a, o) => { calls.push({ f, o }); return { status: 0 }; }, "C:\\agent-bin\\rustright.cmd", ["status", "x"], { env: { RIGHTKIT_MANAGED_AGENT_SHIM: "1" } }, { platform: "win32", readFile: () => SHIM() });
  assert.equal(viaShim.status, 0); assert.equal(calls[0].f, EXE); assert.equal(calls[0].o.shell, false); assert.equal(calls[0].o.env.RIGHTKIT_MANAGED_AGENT_SHIM, undefined);
  const refused = await spawnBrokerClientAsync(async () => { throw new Error("must not start"); }, "C:\\agent-bin\\rustright.cmd", [], {}, { platform: "win32", readFile: () => "@echo off\r\n" });
  assert.equal(refused.error.code, BROKER_CLIENT_UNRESOLVED);
});
