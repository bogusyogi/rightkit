import assert from "node:assert/strict";
import test from "node:test";

import { assertCleanSource } from "./source-gate.mjs";

test("a clean status passes the gate", () => {
  assert.doesNotThrow(() => assertCleanSource({ status: "", commandId: "right-release build" }));
});

test("a dirty status names the command and the offending files", () => {
  assert.throws(
    () => assertCleanSource({ status: " M src/main.rs\0?? notes.txt\0", commandId: "right-release build" }),
    (error) => {
      assert.match(error.message, /right-release build requires a clean working tree/);
      assert.match(error.message, /- src\/main\.rs/);
      assert.match(error.message, /- notes\.txt/);
      return true;
    },
  );
});

test("a missing status is refused rather than treated as clean", () => {
  assert.throws(() => assertCleanSource({ commandId: "right-release upload" }), /status is required/);
  assert.throws(() => assertCleanSource({ status: null }), /status is required/);
});

test("right-release's own untracked lock file does not dirty the tree", () => {
  assert.doesNotThrow(() => assertCleanSource({ status: "?? .right-release/locks/mac.lock.json\0" }));
  assert.doesNotThrow(() => assertCleanSource({ status: "?? apps/slice/.right-release/locks/win.lock.json\0" }));
  assert.doesNotThrow(() => assertCleanSource({ status: "?? .right-release/locks/win.lock.json\0?? .right-release/locks/mac.lock.json\0" }));
});

test("the lock exemption is exact: other vault files and real changes still fail", () => {
  assert.throws(() => assertCleanSource({ status: "?? .right-release/locks/mac.lock.json\0?? .right-release/state/x.json\0" }), /- \.right-release\/state\/x\.json/);
  assert.doesNotThrow(() => assertCleanSource({ status: "?? .right-release/locks/other.json\0" }));
  assert.throws(() => assertCleanSource({ status: "?? .right-release/locks/sub/x.json\0" }), /sub\/x\.json/);
  assert.throws(() => assertCleanSource({ status: " M .right-release/locks/mac.lock.json\0" }), /mac\.lock\.json/);
  assert.throws(() => assertCleanSource({ status: "?? .right-release/locks/mac.lock.json\0 M src/a.rs\0" }), /- src\/a\.rs/);
});

test("committed-checkout clean check ignores the lock file only", async () => {
  const { assertCommittedCheckoutClean } = await import("./committed-source.mjs");
  const runner = (status) => () => ({ status: 0, stdout: Buffer.from(status), stderr: Buffer.alloc(0) });
  assert.equal(assertCommittedCheckoutClean("/repo", { runner: runner("?? .right-release/locks/mac.lock.json\0?? .right-release/locks/win.lock.json\0") }), true);
  assert.throws(() => assertCommittedCheckoutClean("/repo", { runner: runner("?? .right-release/locks/mac.lock.json\0?? stray.txt\0") }), /changes/);
});

test("assertCleanSource keeps the first character of the first path even when the status was trimmed (#782)", () => {
  assert.throws(
    () => assertCleanSource({ status: "M core/src/agent/device.rs\0 M core/src/devicelink/mod.rs\0", commandId: "right-release build" }),
    (error) => /- core\/src\/agent\/device\.rs/.test(error.message) && !/- ore\/src/.test(error.message),
  );
});
