import assert from "node:assert/strict";
import test from "node:test";
import { brokerRequestIds, checkTools, countDocWarnings, crateRows, parseArgs } from "./rk-preflight.mjs";

test("argv: registry and one unit are required; budget must be positive", () => {
  assert.throws(() => parseArgs(["--unit", "rightkit-daemon"]), /--registry/);
  assert.throws(() => parseArgs(["--registry", "crates", "--unit", "a b"]), /exactly one/);
  assert.throws(() => parseArgs(["--registry", "crates", "--unit", "x", "--budget-min", "0"]), /positive/);
  assert.throws(() => parseArgs(["--registry", "crates", "--unit", "x", "--bogus"]), /unknown argument/);
  const opts = parseArgs(["--registry", "crates", "--unit", "rightkit-daemon", "--budget-min", "9", "--skip-tests"]);
  assert.equal(opts.budgetMin, 9);
  assert.equal(opts.skipTests, true);
});

test("crate rows mirror the hosted gate's rows for the unit, cheapest first, with doc warnings denied", () => {
  const config = { rust: { "rightkit-daemon": { featureSets: [[], ["--features", "remote"]], alsoSerial: true, timeoutMinutes: 30 } } };
  const rows = crateRows("rightkit-daemon", config);
  assert.deepEqual(rows.map((r) => r.kind), ["clippy", "clippy", "check", "doc", "doc", "doc", "test", "test", "test", "test"]);
  assert.deepEqual(rows[0].args, ["clippy", "-p", "rightkit-daemon", "--no-deps", "--all-targets", "--", "-D", "warnings"]);
  assert.deepEqual(rows[2].args, ["check", "-p", "rightkit-daemon", "--all-targets", "--all-features"], "checkFeatureSets default to all-features");
  assert.ok(rows.filter((r) => r.kind === "doc").every((r) => r.denyWarnings));
  assert.deepEqual(rows.filter((r) => r.kind === "doc").map((r) => r.args.slice(4)), [[], ["--features", "remote"], ["--all-features"]], "doc rows are the union of feature and check sets");
  assert.ok(rows.filter((r) => r.kind === "test").some((r) => r.args.includes("--test-threads=1")), "alsoSerial adds the serial rerun");
  assert.ok(rows.every((r) => r.timeoutMinutes === 30));
  const allFeatures = crateRows("x", { rust: { x: { featureSets: [["--all-features"]] } } });
  assert.equal(allFeatures.filter((r) => r.kind === "check").length, 0, "an all-features test row needs no separate check row");
  const bare = crateRows("y", {});
  assert.deepEqual(bare.map((r) => r.id), ["clippy", "check[--all-features]", "doc", "doc[--all-features]", "test"]);
});

test("doc warnings count every warning line except cargo's summary; broker request ids are collected", () => {
  const out = ["warning: unresolved link to `Foo`", " --> src/lib.rs:3:5", "warning: `rightkit-daemon` (lib doc) generated 1 warning", "    Finished"].join("\n");
  assert.equal(countDocWarnings(out), 1);
  assert.equal(countDocWarnings("    Finished dev\n"), 0);
  assert.deepEqual(brokerRequestIds("rustright: request 6891efd1-6e8b-4ef4-9600-613527d4515e\nstuff\nrustright: request 4bb7896e-838a-483b-9534-9dc3f4dda639\n"), ["6891efd1-6e8b-4ef4-9600-613527d4515e", "4bb7896e-838a-483b-9534-9dc3f4dda639"]);
});

test("tool parity refuses a missing or wrong-digest shellcheck and an unpinned actionlint version", () => {
  const pins = { actionlint: "1.7.12", rustfmt: { toolchain: "1.98.0", version: "rustfmt 1.9.0-stable (88d9e12ae1 2026-08-18)", darwin: { path: "/tc/1.98.0/bin/rustfmt" } }, shellcheck: { darwin: { path: "/tools/shellcheck", sha256: "aa" } } };
  const spawnFn = (cmd, args) => ({ status: 0, stdout: cmd === "actionlint" ? "1.7.12\n" : cmd.endsWith("rustfmt") ? "rustfmt 1.9.0-stable (88d9e12ae1 2026-08-18)\n" : "ShellCheck - shell script analysis tool\nversion: 0.10.0\n" });
  const base = { pins, spawnFn, exists: () => true, digest: () => "aa", platform: "darwin" };
  const good = checkTools(base);
  assert.deepEqual(good.problems, []);
  assert.equal(good.observed.shellcheck.sha256, "aa");
  assert.equal(good.observed.rustfmt.path, "/tc/1.98.0/bin/rustfmt", "the pinned toolchain's rustfmt, never PATH's");
  const oldFmt = checkTools({ ...base, spawnFn: (cmd, args) => (cmd.endsWith("rustfmt") ? { status: 0, stdout: "rustfmt 1.8.0-stable (4a4ef493e3 2026-03-02)\n" } : spawnFn(cmd, args)) });
  assert.match(oldFmt.problems.join("\n"), /rustfmt at \/tc\/1.98.0\/bin\/rustfmt is "rustfmt 1.8.0-stable .*", pinned "rustfmt 1.9.0-stable/);
  const noFmt = checkTools({ ...base, exists: (p) => p !== "/tc/1.98.0/bin/rustfmt" });
  assert.match(noFmt.problems.join("\n"), /pinned rustfmt is missing/);
  const missing = checkTools({ ...base, exists: () => false });
  assert.match(missing.problems.join("\n"), /pinned shellcheck is missing/);
  const unpinned = checkTools({ ...base, platform: "win32" });
  assert.match(unpinned.problems.join("\n"), /no win32 path pinned/);
  const wrong = checkTools({ ...base, digest: () => "bb" });
  assert.match(wrong.problems.join("\n"), /has sha256 bb, pinned aa/);
  const oldLint = checkTools({ ...base, spawnFn: (cmd, args) => (cmd === "actionlint" ? { status: 0, stdout: "1.6.0\n" } : spawnFn(cmd, args)) });
  assert.match(oldLint.problems.join("\n"), /actionlint 1.6.0 is not the pinned 1.7.12/);
  const noLint = checkTools({ ...base, spawnFn: (cmd, args) => (cmd === "actionlint" ? { error: new Error("ENOENT") } : spawnFn(cmd, args)) });
  assert.match(noLint.problems.join("\n"), /actionlint is not on PATH/);
});
