// The optional Linux broker route (Adrian, 2026-10-31: Linux normally needs no broker, but a broker is allowed on Linux). Real files and a real unix
// socket on disk judge the declared route; nothing here is a broker, an executor or a platform spoof: `spawn` only records what managed-cargo would
// have launched, which is how every managed-cargo test observes a launch.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { brokerManagedHost } from "./cargo-contract.mjs";
import {
  LINUX_BROKER_CLIENT_ENV, LINUX_BROKER_CLIENT_SHA256_ENV, LINUX_BROKER_REFUSAL, LINUX_BROKER_SOCKET_ENV,
  directHost, linuxBrokerDeclared, managedHost, resolveLinuxBrokerRoute, runCargoAsync, runCargoSync,
} from "./managed-cargo.mjs";

const BODY = "#!/bin/sh\nexit 0\n";
const sha = (body) => createHash("sha256").update(body).digest("hex");

/** A real installed-looking tree under the temp directory: an executable client file and a listening unix socket. */
async function installed(t) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-linux-broker-")));
  const server = net.createServer();
  const socket = path.join(dir, "broker.sock");
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(socket, resolve); });
  t.after(() => { server.close(); fs.rmSync(dir, { recursive: true, force: true }); });
  const client = path.join(dir, "rustright");
  fs.writeFileSync(client, BODY); fs.chmodSync(client, 0o755);
  return { dir, client, socket, env: { PATH: "/usr/bin", [LINUX_BROKER_CLIENT_ENV]: client, [LINUX_BROKER_SOCKET_ENV]: socket, [LINUX_BROKER_CLIENT_SHA256_ENV]: sha(BODY) } };
}

test("a Linux host with no declaration stays direct, whatever is on PATH or in the older broker variables", () => {
  const env = { PATH: "/home/u/.rightkit-managed/agent-bin:/usr/bin", RUSTRIGHT_BUILD_BROKER_SOCKET: "/run/rustright.sock", RIGHTKIT_BUILD_BROKER_SOCKET: "/run/rightkit.sock" };
  assert.equal(linuxBrokerDeclared(env, "linux"), false);
  assert.deepEqual(resolveLinuxBrokerRoute({ env, platform: "linux" }), { declared: false });
  assert.equal(directHost("linux", env), true);
  assert.equal(managedHost(env, "linux"), false);
  assert.equal(brokerManagedHost(env, "linux", () => true), false);
  const calls = [];
  runCargoSync(["metadata"], { env }, { platform: "linux", exists: () => false, spawn: (c, a) => { calls.push([c, ...a]); return { status: 0, stdout: "", stderr: "" }; } });
  assert.deepEqual(calls, [["cargo", "metadata"]], "unconfigured Linux runs cargo directly");
});

test("the declaration is Linux's only: Mac and Windows ignore it and keep their own broker routing", () => {
  const env = { [LINUX_BROKER_CLIENT_ENV]: "/x/rustright", [LINUX_BROKER_SOCKET_ENV]: "/x/b.sock" };
  assert.equal(linuxBrokerDeclared(env, "darwin"), false);
  assert.equal(linuxBrokerDeclared(env, "win32"), false);
  assert.deepEqual(resolveLinuxBrokerRoute({ env, platform: "darwin" }), { declared: false });
});

test("a declared route with a real client and a real listening socket validates, and reports the client's identity", async (t) => {
  const host = await installed(t);
  const route = resolveLinuxBrokerRoute({ env: host.env, platform: "linux" });
  assert.deepEqual({ ...route }, { declared: true, ok: true, client: host.client, socket: host.socket, clientSha256: sha(BODY) });
  assert.equal(directHost("linux", host.env), false);
  assert.equal(managedHost(host.env, "linux"), true);
  assert.equal(brokerManagedHost(host.env, "linux", () => false), true);
  const pinned = resolveLinuxBrokerRoute({ env: { ...host.env, [LINUX_BROKER_CLIENT_SHA256_ENV]: sha(BODY).toUpperCase() }, platform: "linux" });
  assert.equal(pinned.ok, true, "a matching pin (either case) is accepted");
  for (const missing of [undefined, ""]) {
    const r = resolveLinuxBrokerRoute({ env: { ...host.env, [LINUX_BROKER_CLIENT_SHA256_ENV]: missing }, platform: "linux" });
    assert.deepEqual([r.declared, r.ok, r.code], [true, false, LINUX_BROKER_REFUSAL], "the pin is required");
    assert.match(r.problem, /CLIENT_SHA256 is not set/);
  }
});

test("every way a declared route can be wrong refuses with RK_LINUX_BROKER_UNAVAILABLE, naming the cause", async (t) => {
  const host = await installed(t);
  const refused = (env, pattern, label) => {
    const r = resolveLinuxBrokerRoute({ env: { ...host.env, ...env }, platform: "linux" });
    assert.equal(r.declared, true, label); assert.equal(r.ok, false, label); assert.equal(r.code, LINUX_BROKER_REFUSAL, label);
    assert.match(r.problem, pattern, label);
  };
  refused({ [LINUX_BROKER_CLIENT_ENV]: undefined }, /LINUX_BROKER_CLIENT is (?:not set|empty)/, "client missing");
  const half = { PATH: "/usr/bin", [LINUX_BROKER_CLIENT_ENV]: host.client };
  assert.equal(resolveLinuxBrokerRoute({ env: half, platform: "linux" }).ok, false, "a half declaration refuses");
  refused({ [LINUX_BROKER_SOCKET_ENV]: "" }, /empty/, "socket empty");
  refused({ [LINUX_BROKER_CLIENT_ENV]: "rustright" }, /absolute path/, "relative client");
  refused({ [LINUX_BROKER_CLIENT_ENV]: `${host.dir}/../${path.basename(host.dir)}/rustright` }, /no '\.\.'/, "dot-dot client");
  refused({ [LINUX_BROKER_CLIENT_ENV]: path.join(host.dir, "absent") }, /cannot be resolved/, "client absent");
  refused({ [LINUX_BROKER_CLIENT_ENV]: host.dir }, /not a regular file/, "client is a directory");
  refused({ [LINUX_BROKER_SOCKET_ENV]: path.join(host.dir, "no-such.sock") }, /broker is not running here/, "no broker endpoint");
  refused({ [LINUX_BROKER_SOCKET_ENV]: host.client }, /not a unix socket/, "endpoint is a file");
  refused({ [LINUX_BROKER_CLIENT_SHA256_ENV]: "abc" }, /not a SHA-256/, "malformed pin");
  refused({ [LINUX_BROKER_CLIENT_SHA256_ENV]: sha("another client") }, /not the pinned/, "pin mismatch");
  refused({ RUSTRIGHT_BUILD_BROKER_SOCKET: "/elsewhere.sock" }, /ambiguous/, "another endpoint named");
  assert.equal(resolveLinuxBrokerRoute({ env: { ...host.env, RUSTRIGHT_BUILD_BROKER_SOCKET: host.socket }, platform: "linux" }).ok, true, "the same endpoint named twice is not a conflict");
  fs.chmodSync(host.client, 0o644);
  refused({}, /not executable/, "client not executable");
  fs.chmodSync(host.client, 0o775);
  refused({}, /group- or world-writable/, "client writable by its group");
  fs.chmodSync(host.client, 0o755);
  fs.chmodSync(host.dir, 0o777);
  refused({}, /group- or world-writable/, "a world-writable directory holds the client");
});

test("a declared route that refuses never runs cargo direct; a validated one launches the declared client at the declared endpoint", async (t) => {
  const host = await installed(t);
  const calls = [];
  const unpinned = { ...host.env }; delete unpinned[LINUX_BROKER_CLIENT_SHA256_ENV];
  const spawn = (c, a, o) => { calls.push({ call: [c, ...a], socket: o?.env?.RUSTRIGHT_BUILD_BROKER_SOCKET }); return { status: 0, stdout: "", stderr: "" }; };
  // Valid: the client, never `cargo`, and the endpoint rides the child's environment.
  const ok = runCargoSync(["build", "-p", "a"], { env: host.env }, { platform: "linux", exists: () => false, spawn });
  assert.equal(ok.status, 0);
  assert.deepEqual(calls, [{ call: [host.client, "cargo", "build", "-p", "a"], socket: host.socket }]);
  // Invalid (the broker is stopped: its socket is gone): refused, zero launches, and the async twin refuses the same way.
  calls.length = 0;
  const gone = { ...host.env, [LINUX_BROKER_SOCKET_ENV]: path.join(host.dir, "stopped.sock") };
  const sync = runCargoSync(["build"], { env: gone }, { platform: "linux", exists: () => true, spawn });
  assert.equal(sync.status, null); assert.equal(sync.error.code, "RK_BROKER_UNAVAILABLE"); assert.match(sync.stderr, /RK_LINUX_BROKER_UNAVAILABLE/);
  const async_ = await runCargoAsync(["build"], { env: gone }, { platform: "linux", exists: () => true, spawn, spawnAsync: async (c, a) => { calls.push({ call: [c, ...a] }); return { status: 0, stdout: "", stderr: "" }; } });
  assert.equal(async_.error.code, "RK_BROKER_UNAVAILABLE");
  assert.deepEqual(calls, [], "no direct cargo, no client launch, with the declared broker unavailable");
  const unpinnedRun = runCargoSync(["build"], { env: unpinned }, { platform: "linux", exists: () => false, spawn });
  assert.equal(unpinnedRun.error.code, "RK_BROKER_UNAVAILABLE"); assert.match(unpinnedRun.stderr, /CLIENT_SHA256 is not set/);
  assert.deepEqual(calls, [], "a declaration without the client's pinned digest launches nothing");
});

test("a launched client's own unavailable answer is reported as the broker being unavailable on a declared Linux route", async (t) => {
  const host = await installed(t);
  const spawn = () => ({ status: 1, stdout: "", stderr: "RK_BROKER_UNAVAILABLE: connect refused" });
  const r = runCargoSync(["check"], { env: host.env }, { platform: "linux", exists: () => false, spawn });
  assert.equal(r.error.code, "RK_BROKER_UNAVAILABLE");
});
