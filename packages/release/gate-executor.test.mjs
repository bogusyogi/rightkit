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
import { EXECUTOR_KINDS, LINUX_BROKER_ROUTE, resolveExecutor } from "./gate-executor.mjs";
import { LINUX_BROKER_CLIENT_ENV, LINUX_BROKER_CLIENT_SHA256_ENV, LINUX_BROKER_SOCKET_ENV } from "./managed-cargo.mjs";

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

async function installed(t) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-exec-linux-")));
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

test("a declared Linux broker resolves to one typed broker choice carrying the validated client and endpoint, not a PATH lookup", async (t) => {
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

test("a declared Linux broker that is stopped, replaced, unpinned or mis-pinned refuses; it never resolves to direct", async (t) => {
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
