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
