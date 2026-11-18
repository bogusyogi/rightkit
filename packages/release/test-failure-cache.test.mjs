import { test } from "node:test";
import assert from "node:assert/strict";
import { isDeterministicCompileDiagnostics, isRustTestRowId, isTestCompileFailure } from "./deterministic-scans.mjs";

test("a test row's failure is stored only when its build did not compile, never when a test failed (NN41)", () => {
  assert.equal(isRustTestRowId("test:rust:rk:rk-core:2"), true); assert.equal(isRustTestRowId("test:batch:4ca64153"), true, "the batched group, S17 2026-11-18"); assert.equal(isRustTestRowId("clippy:batch:4ca64153"), false); assert.equal(isRustTestRowId("test:rust-serial:rk:rk-core"), true); assert.equal(isRustTestRowId("clippy:rk:rk-core"), false);
  // The S17 case: the crate does not compile.
  const compile = "error[E0308]: mismatched types\n  --> src/lib.rs:3:5\nerror: could not compile `rk-core` (lib test) due to 1 previous error\n";
  assert.equal(isDeterministicCompileDiagnostics(compile) && isTestCompileFailure(compile), true);
  // A failed test: refused by both rules (the test-row rule is a second guard: "could not compile" only).
  const failed = "running 2 tests\ntest a ... FAILED\nthread 'a' panicked at src/lib.rs:9:5\ntest result: FAILED. 1 passed; 1 failed\nerror: test failed, to rerun pass `--lib`\n";
  assert.equal(isDeterministicCompileDiagnostics(failed), false, "the compile rule already refuses a failed test");
  assert.equal(isTestCompileFailure(failed), false);
  // An environmental failure is never stored, whatever the row.
  assert.equal(isDeterministicCompileDiagnostics("error: failed to download `serde`\nerror: could not compile `x`"), false);
});
