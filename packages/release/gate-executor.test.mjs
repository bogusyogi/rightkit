import test from "node:test";
import assert from "node:assert/strict";
import { resolveExecutor } from "./gate-executor.mjs";

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
