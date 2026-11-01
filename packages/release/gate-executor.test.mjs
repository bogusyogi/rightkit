// resolveExecutor's rows: the broker-exception forceDirect rows (Mac and Dell) and the Linux rows (rightkit#1317: "Linux needs no broker, but a
// broker is allowed on Linux"). A real client file and a real unix socket on disk are what a declared Linux route is judged on; `which` and
// `readFile` are the inputs resolveExecutor has always taken.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { brokerManagedHost } from "./cargo-contract.mjs";
import { EXECUTOR_KINDS, LINUX_BROKER_ROUTE, resolveExecutor } from "./gate-executor.mjs";
import { LINUX_BROKER_CLIENT_ENV, LINUX_BROKER_CLIENT_SHA256_ENV, LINUX_BROKER_SOCKET_ENV } from "./managed-cargo.mjs";

/** Unix socket paths are limited to 103 bytes on macOS and a hosted runner's hermetic temp root leaves no room: a socket's directory falls back to /tmp when the socket's path would not fit under the temp directory. */
const socketBase = (prefix, file) => (process.platform === "win32" || Buffer.byteLength(path.join(os.tmpdir(), `${prefix}XXXXXX`, file)) < 100 ? os.tmpdir() : "/tmp");
const base = { env: { PATH: "/usr/bin" }, exists: () => false, readFile: () => "" };
const withRustright = (cmd) => (cmd === "rustright" ? ["/usr/local/bin/rustright"] : []);

test("Mac and Dell take the broker; Linux takes direct", () => {
  for (const platform of ["darwin", "win32"]) assert.equal(resolveExecutor({ ...base, platform, which: withRustright }).kind, "broker", platform);
  assert.equal(resolveExecutor({ ...base, platform: "linux", which: withRustright }).kind, "direct");
});

test("forceDirect (a recorded broker exception) takes the direct route on Mac and Dell, whatever the broker offers", () => {
  for (const platform of ["darwin", "win32"]) {
    const x = resolveExecutor({ ...base, platform, which: withRustright, forceDirect: true });
    assert.equal(x.kind, "direct", platform);
    assert.deepEqual(x.cargo, ["cargo"]);
  }
  // A broker-managed Mac whose broker is unreachable refuses without the exception and still builds direct with it.
  const managed = { ...base, env: { PATH: "/usr/bin", RUSTRIGHT_BUILD_BROKER_SOCKET: "/s" }, platform: "darwin", which: () => [] };
  assert.equal(resolveExecutor(managed).kind, "refuse");
  assert.equal(resolveExecutor({ ...managed, forceDirect: true }).kind, "direct");
});

test("forceDirect takes RustRight shim directories off PATH", () => {
  const shim = "/Users/a/Library/rightkit/build-control/agent-bin";
  const exists = (p) => p === `${shim}/cargo` || p === "/opt/cargo/bin/cargo";
  const x = resolveExecutor({ ...base, env: { PATH: `${shim}:/opt/cargo/bin` }, exists, platform: "darwin", which: withRustright, forceDirect: true });
  assert.equal(x.kind, "direct");
  assert.equal(x.env.PATH, "/opt/cargo/bin");
});

test("forceDirect never takes the Linux broker route, even when the declaration is present", () => {
  const env = { PATH: "/usr/bin", [LINUX_BROKER_CLIENT_ENV]: "/nope/rustright", [LINUX_BROKER_SOCKET_ENV]: "/nope/b.sock" };
  const x = resolveExecutor({ ...base, env, platform: "darwin", which: withRustright, forceDirect: true });
  assert.equal(x.kind, "direct"); assert.equal(x.route, undefined);
});

/** Windows cannot hold the declared Linux broker route's endpoint: a unix socket file on a POSIX path (the route is Linux's only, and its judging reads POSIX paths). */
const NO_UNIX_SOCKET = process.platform === "win32" && "a declared Linux broker names a unix socket file on a POSIX path, which Windows cannot hold";
async function installed(t) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(socketBase("rk-exec-linux-", "broker.sock"), "rk-exec-linux-")));
  const server = net.createServer();
  const socket = path.join(dir, "broker.sock");
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(socket, resolve); });
  t.after(() => { server.close(); fs.rmSync(dir, { recursive: true, force: true }); });
  const client = path.join(dir, "rustright");
  fs.writeFileSync(client, "#!/bin/sh\nexit 0\n"); fs.chmodSync(client, 0o755);
  return { dir, client, socket, sha: createHash("sha256").update("#!/bin/sh\nexit 0\n").digest("hex") };
}
const resolve = (env, platform = "linux", over = {}) => resolveExecutor({ env, platform, exists: () => false, readFile: () => "", which: () => [], ...over });

test("unconfigured Linux is direct whatever is on PATH, rustright included (no automatic policy expansion)", () => {
  const withClient = { which: (cmd) => (cmd === "rustright" ? ["/usr/local/bin/rustright"] : []) };
  for (const env of [{ PATH: "/usr/bin" }, { PATH: "/usr/local/bin:/usr/bin", RUSTRIGHT_BUILD_BROKER_SOCKET: "/run/r.sock" }, { PATH: "/h/.rightkit-managed/agent-bin:/usr/bin" }]) {
    const r = resolve(env, "linux", withClient);
    assert.equal(r.kind, "direct"); assert.deepEqual(r.cargo, ["cargo"]); assert.equal(r.route, undefined);
  }
});

test("a declared Linux broker resolves to one typed broker choice carrying the validated client and endpoint, not a PATH lookup", { skip: NO_UNIX_SOCKET }, async (t) => {
  const host = await installed(t);
  // Even with another `rustright` first on PATH, the declared client is the one chosen.
  const r = resolve({ PATH: "/usr/bin", [LINUX_BROKER_CLIENT_ENV]: host.client, [LINUX_BROKER_SOCKET_ENV]: host.socket, [LINUX_BROKER_CLIENT_SHA256_ENV]: host.sha }, "linux", { which: () => ["/usr/bin/rustright"] });
  assert.equal(r.kind, "broker"); assert.equal(r.route, LINUX_BROKER_ROUTE);
  assert.deepEqual(r.cargo, [host.client, "cargo"]);
  assert.equal(r.client, host.client); assert.equal(r.socket, host.socket);
  assert.deepEqual(r.env, { RUSTRIGHT_BUILD_BROKER_SOCKET: host.socket });
  assert.match(r.clientSha256, /^[0-9a-f]{64}$/);
  assert.ok(EXECUTOR_KINDS.includes(r.kind));
});

test("a declared Linux broker that is stopped, replaced, unpinned or mis-pinned refuses; it never resolves to direct", { skip: NO_UNIX_SOCKET }, async (t) => {
  const host = await installed(t);
  const declared = { PATH: "/usr/bin", [LINUX_BROKER_CLIENT_ENV]: host.client, [LINUX_BROKER_SOCKET_ENV]: host.socket, [LINUX_BROKER_CLIENT_SHA256_ENV]: host.sha };
  const stopped = resolve({ ...declared, [LINUX_BROKER_SOCKET_ENV]: path.join(host.dir, "gone.sock") });
  assert.equal(stopped.kind, "refuse"); assert.equal(stopped.cargo, null); assert.match(stopped.problem, /RK_LINUX_BROKER_UNAVAILABLE.*never runs direct/);
  const { [LINUX_BROKER_CLIENT_SHA256_ENV]: _pin, ...unpinned } = declared; void _pin;
  const noPin = resolve(unpinned, "linux", { which: () => ["/usr/bin/rustright"] });
  assert.equal(noPin.kind, "refuse"); assert.equal(noPin.cargo, null); assert.match(noPin.problem, /RK_LINUX_BROKER_UNAVAILABLE.*CLIENT_SHA256 is not set/, "the digest pin is required");
  const mispinned = resolve({ ...declared, [LINUX_BROKER_CLIENT_SHA256_ENV]: "0".repeat(64) });
  assert.equal(mispinned.kind, "refuse"); assert.match(mispinned.problem, /not the pinned/);
  const missing = resolve({ PATH: "/usr/bin", [LINUX_BROKER_SOCKET_ENV]: host.socket }, "linux", { which: () => ["/usr/bin/rustright"] });
  assert.equal(missing.kind, "refuse", "a half declaration refuses even with rustright on PATH");
});

test("the declaration changes nothing on Mac and Windows", () => {
  const env = { PATH: "/usr/bin", [LINUX_BROKER_CLIENT_ENV]: "/nope/rustright", [LINUX_BROKER_SOCKET_ENV]: "/nope/b.sock" };
  assert.deepEqual(resolve(env, "darwin", { which: (cmd) => (cmd === "rustright" ? ["/Volumes/D/.rightkit-managed/agent-bin/rustright"] : []) }), { kind: "broker", cargo: ["rustright", "cargo"] });
  assert.equal(resolve({ PATH: "C:\\Windows", ...env }, "win32").kind, "direct");
});

// rightkit#1367 step 2: two predicates say "the broker decides how many jobs run": the gate's `executor.kind === "broker"` (it plans and spawns the scans) and the release tools'
// `brokerManagedHost()` (right-release's heavy-work slot, cargo-guard). They are NOT the same function: the executor takes a `rustright` or the guard shim anywhere on PATH, while
// brokerManagedHost reads only the managed markers (installed client, broker socket variable, managed agent-bin on PATH, declared Linux route). Each row names both answers; the
// rows where they differ are exactly the executor-broker / managed-host-direct ones below, and the difference falls on the safe side: a release tool that does not see the broker
// keeps its host-wide heavy-work slot, never drops it.
test("broker host: the gate's executor and brokerManagedHost() agree on every managed marker and differ only where the executor trusts a PATH client or the guard shim", async (t) => {
  const shim = "/Users/a/Library/rightkit/build-control/agent-bin";
  const shimExists = (p) => p === `${shim}/cargo`;
  const guardShim = (cmd) => (cmd === "cargo" ? ["/opt/rk/cargo"] : []);
  const rows = [
    { name: "Mac with rustright on PATH and the broker socket named", platform: "darwin", env: { PATH: "/usr/bin", RUSTRIGHT_BUILD_BROKER_SOCKET: "/s" }, which: withRustright, exists: () => false, executor: "broker", managed: true },
    { name: "Mac with the managed shim first on PATH", platform: "darwin", env: { PATH: `${shim}:/usr/bin` }, which: () => [], exists: shimExists, executor: "broker", managed: true },
    { name: "Dell with rustright on PATH and the broker pipe named", platform: "win32", env: { PATH: "C:\\bin", RUSTRIGHT_BUILD_BROKER_SOCKET: "\\\\.\\pipe\\b" }, which: withRustright, exists: () => false, executor: "broker", managed: true },
    { name: "Mac with no broker at all", platform: "darwin", env: { PATH: "/usr/bin" }, which: () => [], exists: () => false, executor: "direct", managed: false },
    { name: "Linux with no declared route, rustright and a socket named", platform: "linux", env: { PATH: "/usr/bin", RUSTRIGHT_BUILD_BROKER_SOCKET: "/s" }, which: withRustright, exists: () => false, executor: "direct", managed: false },
    { name: "Mac under the recorded no-broker exception", platform: "darwin", env: { PATH: "/usr/bin", RUSTRIGHT_BUILD_BROKER_SOCKET: "/s", RK_GATE_RUN: "run-1", RIGHTKIT_GATE_NO_BROKER: "exc-1" }, which: withRustright, exists: () => false, exception: true, executor: "direct", managed: false },
    // The rows where they differ (documented, safe side):
    { name: "Mac with rustright on PATH and no managed marker (no socket variable, no installed client, no agent-bin)", platform: "darwin", env: { PATH: "/usr/bin" }, which: withRustright, exists: () => false, executor: "broker", managed: false },
    { name: "Mac with the cargo-guard shim first on PATH outside agent-bin", platform: "darwin", env: { PATH: "/opt/rk:/usr/bin" }, which: guardShim, exists: () => false, readFile: (f) => (f === "/opt/rk/cargo" ? "#!/usr/bin/env node // cargo-guard.mjs" : ""), executor: "broker", managed: false },
  ];
  for (const row of rows) {
    const kind = resolveExecutor({ env: row.env, platform: row.platform, exists: row.exists, which: row.which, readFile: row.readFile ?? (() => ""), forceDirect: Boolean(row.exception) }).kind;
    assert.equal(kind, row.executor, `${row.name}: executor`);
    assert.equal(brokerManagedHost(row.env, row.platform, row.exists), row.managed, `${row.name}: brokerManagedHost`);
  }
  // A broker-managed machine whose broker cannot be reached refuses in the gate; the release tools also call it a broker host, so it is never read as direct.
  const unreachable = { PATH: "/usr/bin", RUSTRIGHT_BUILD_BROKER_SOCKET: "/s" };
  assert.equal(resolveExecutor({ ...base, env: unreachable, platform: "darwin", which: () => [] }).kind, "refuse");
  assert.equal(brokerManagedHost(unreachable, "darwin", () => false), true);
  // The declared Linux route: a valid declaration is a broker in both; an invalid one refuses in the gate and is still a managed host (the release tools never run direct around it).
  if (!NO_UNIX_SOCKET) {
    const host = await installed(t);
    const declared = { PATH: "/usr/bin", [LINUX_BROKER_CLIENT_ENV]: host.client, [LINUX_BROKER_SOCKET_ENV]: host.socket, [LINUX_BROKER_CLIENT_SHA256_ENV]: host.sha };
    assert.equal(resolve(declared).kind, "broker");
    assert.equal(brokerManagedHost(declared, "linux", () => false), true, "declared Linux route: valid");
    const broken = { ...declared, [LINUX_BROKER_SOCKET_ENV]: path.join(host.dir, "gone.sock") };
    assert.equal(resolve(broken).kind, "refuse");
    assert.equal(brokerManagedHost(broken, "linux", () => false), true, "declared Linux route: invalid, still managed (never direct)");
  }
});
