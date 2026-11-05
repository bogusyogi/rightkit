// rightkit#1512: a run-outside Rust test row keeps each test executable's own stdout/stderr for its failure log.
import test from "node:test";
import assert from "node:assert";
import { runTestBinaries } from "./deterministic-scans.mjs";

test("runTestBinaries returns each executable's own output, failing and passing", async () => {
  const binaries = [
    { package: "p", target: "a", sha256: "1", manifestDir: "/m", real: "/bin/a" },
    { package: "p", target: "b", sha256: "2", manifestDir: "/m", real: "/bin/b" },
  ];
  const spawn = async (real) => real === "/bin/a"
    ? { status: 101, stdout: "running 1 test\ntest probe::tests::x ... FAILED\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n", stderr: "thread 'x' panicked at probe.rs:179\n" }
    : { status: 0, stdout: "running 1 test\ntest y ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n", stderr: "" };
  const ran = await runTestBinaries({ scan: { id: "test:p", runOutside: { args: ["--test-threads=1"] } }, binaries, spawn, env: {}, limitMs: 10_000, ops: { isDir: () => true }, concurrency: 2 });
  assert.strictEqual(ran.status, "fail");
  assert.strictEqual(ran.outputs.length, 2);
  const a = ran.outputs.find((o) => o.label === "p::a");
  assert.strictEqual(a.exit, 101);
  assert.match(a.stderr, /panicked at probe\.rs:179/);
  assert.match(a.stdout, /probe::tests::x \.\.\. FAILED/);
  assert.ok(!("outputs" in ran.executables[0]), "outputs never enter the report rows");
});
