// rightkit#1301 F1: what the broker does with each cargo subcommand a snapshot stage carries. The table in
// test-support/cargo-classification.mjs is a fixture of rustright-policy command_policy.rs `cargo_capability` (the classification) and
// rustright-broker job_service/service.rs ("a snapshot request must compile"). Every subcommand the gate, right-release's seam and the
// wrapper carry is in it; a read-only one is marked as needing RustRight's read-only snapshot acceptance, and the fixture broker refuses
// it the way the real one does today unless a flag models the post-fix broker.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { WRAPPER_COMMANDS } from "./cargo-target-shim.mjs";
import { installCarrier } from "./carried-cargo.mjs";
import { runCargoSync } from "./managed-cargo.mjs";
import { brokerSection } from "./snapshot-broker.mjs";
import { cargoSubcommandOf, lockedArgv } from "./snapshot-request.mjs";
import { CARGO_CLASSIFICATION, READ_ONLY_NEEDS, acceptedAfterFix, classifyCargo } from "./test-support/cargo-classification.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const POSIX = process.platform === "win32" ? "the fixture wrapper is POSIX only" : false;
const subcommandOf = (argv) => (["--version", "-V", "-vV"].includes(argv[0]) ? argv[0] : cargoSubcommandOf(argv));

test("the table is the broker's classification: only BUILD capabilities compile; every read-only row names what it needs", () => {
  for (const row of CARGO_CLASSIFICATION) {
    assert.equal(classifyCargo(row.argv), row.class, `cargo ${row.argv.join(" ")}`);
    if (row.class === "read-only") assert.equal(row.needs, READ_ONLY_NEEDS, `cargo ${row.argv.join(" ")}`);
    else assert.equal(row.needs, undefined, `cargo ${row.argv.join(" ")}`);
  }
  assert.equal(READ_ONLY_NEEDS, "requires rustright read-only snapshot acceptance (rightkit#1301 F1)");
  const compiles = CARGO_CLASSIFICATION.filter((r) => r.class === "compile").map((r) => subcommandOf(r.argv)).sort();
  assert.deepEqual(compiles, ["bench", "build", "check", "clippy", "doc", "package", "run", "rustc", "rustdoc", "test"]);
  // The same command is one class or another by its flags (command_policy.rs): metadata without --no-deps writes the lock, fmt without --check rewrites files.
  assert.equal(classifyCargo(["metadata", "--no-deps"]), "read-only");
  assert.equal(classifyCargo(["metadata", "--locked"]), "read-only");
  assert.equal(classifyCargo(["metadata"]), "write");
  assert.equal(classifyCargo(["package", "-p", "a", "--list", "--locked"]), "read-only", "the observation form of package compiles nothing");
  assert.equal(classifyCargo(["+nightly", "build"]), "compile");
});

test("the table covers every subcommand the wrapper binds or passes through, and every subcommand right-release's own calls carry", () => {
  const covered = new Set(CARGO_CLASSIFICATION.map((r) => subcommandOf(r.argv)));
  // A one-letter command is a cargo alias (b, c, t, d, r): cargo expands it, and the broker's policy lists none.
  for (const command of [...WRAPPER_COMMANDS.bound, ...WRAPPER_COMMANDS.passthrough].filter((c) => c.length > 1)) assert.ok(covered.has(command), `the wrapper carries cargo ${command}: classify it in the table`);
  assert.ok(covered.has("--version"));
  // The seam's callers, by source: each subcommand is in the table with `seam` among its callers.
  const here = (f) => fs.readFileSync(new URL(f, import.meta.url), "utf8");
  const sites = [
    ["stage-sidecar.mjs", ['"build"', '"metadata"'], ["build", "metadata"]], ["cargo-target.mjs", ['"metadata"'], ["metadata"]], ["development.mjs", ['"metadata"'], ["metadata"]],
    ["cargo-license-notices.mjs", ['"about"'], ["about"]], ["deps-cargo.mjs", ['"audit"'], ["audit"]], ["cargo-contract.mjs", ['"metadata"'], ["metadata"]], ["media-sidecar-catalog.mjs", ['"metadata"'], ["metadata"]],
  ];
  for (const [file, needles, commands] of sites) {
    for (const needle of needles) assert.ok(here(file).includes(needle), `${file} no longer carries ${needle}: update the table`);
    for (const command of commands) assert.ok(CARGO_CLASSIFICATION.some((r) => subcommandOf(r.argv) === command && r.callers.includes("seam")), `${file}: ${command} must list the seam as a caller`);
  }
});

/** A broker host with the stage's carrier installed, as the carried-cargo tests build it. */
function host(t, options) {
  const fx = makeFakeBroker(t, options);
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout, native: path.join(fx.checkout, "native") }, section: brokerSection(fx.sb), platform: "darwin", env: {} });
  t.after(() => installed.cleanup());
  const env = { PATH: process.env.PATH, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs", ...installed.env };
  return { fx, call: (argv) => runCargoSync(argv, { cwd: path.join(fx.snapshot, "crates", "a"), env, encoding: "utf8" }, { platform: "darwin" }) };
}

test("the fixture broker accepts a compile and refuses every other class as a snapshot request, as the real one does today", { skip: POSIX }, (t) => {
  const h = host(t);
  for (const row of CARGO_CLASSIFICATION) {
    const r = h.call(row.argv);
    if (row.class === "compile") assert.equal(r.status, 0, `cargo ${row.argv.join(" ")}: ${r.stderr}`);
    else {
      assert.notEqual(r.status, 0, `cargo ${row.argv.join(" ")} must be refused today`);
      assert.equal(r.outputDir, undefined, "an unverified call names no output directory");
      assert.match(r.stderr, /RK_CARGO_BROKER_UNVERIFIED/);
    }
  }
  assert.equal(h.fx.calls().length, CARGO_CLASSIFICATION.filter((r) => r.class === "compile").length, "the fixture broker took exactly the compile requests and refused the rest before submitting them");
});

test("with the post-fix broker flag (readOnlySnapshots) only the lock-bound allowlist is accepted: metadata --no-deps, metadata and cargo about with --frozen; every other read-only subcommand stays refused", { skip: POSIX }, (t) => {
  const h = host(t, { readOnlySnapshots: true });
  for (const row of CARGO_CLASSIFICATION.filter((r) => r.class === "read-only")) {
    const r = h.call(row.argv);
    if (acceptedAfterFix(row.argv)) { assert.equal(r.status, 0, `cargo ${row.argv.join(" ")}: ${r.stderr}`); assert.equal(r.outputDir, h.fx.outputDir, `cargo ${row.argv.join(" ")}`); }
    else assert.notEqual(r.status, 0, `cargo ${row.argv.join(" ")} is outside the allowlist`);
  }
  assert.equal(acceptedAfterFix(["metadata", "--no-deps"]), true);
  assert.equal(acceptedAfterFix(["metadata", "--frozen", "--format-version", "1"]), true);
  assert.equal(acceptedAfterFix(["metadata", "--locked"]), false, "with dependencies it must be --frozen");
  assert.equal(acceptedAfterFix(["about", "generate", "--frozen"]), true);
  assert.equal(acceptedAfterFix(["about", "generate", "--locked"]), false);
});

test("every read-only call right-release itself carries is inside the post-fix allowlist (metadata --no-deps, or with dependencies --frozen; cargo about --frozen)", () => {
  const seamRows = CARGO_CLASSIFICATION.filter((r) => r.class === "read-only" && r.callers.includes("seam"));
  assert.ok(seamRows.length >= 3);
  for (const row of seamRows) if (row.argv[0] !== "--version" && row.argv[0] !== "audit") assert.equal(acceptedAfterFix(row.argv), true, `cargo ${row.argv.join(" ")}`);
  const here = (f) => fs.readFileSync(new URL(f, import.meta.url), "utf8");
  assert.match(here("stage-sidecar.mjs"), /"metadata", "--frozen", "--format-version", "1"\]/);
  assert.match(here("media-sidecar-catalog.mjs"), /\["metadata", "--frozen", "--format-version", "1"/);
  assert.match(here("cargo-license-notices.mjs"), /"about", "generate", "--frozen", "--fail"/);
  assert.match(here("cargo-contract.mjs"), /"metadata", "--no-deps"/);
});

test("a build or run is sent locked: --locked is added after the subcommand unless --locked or --frozen is already there (before any --)", () => {
  assert.deepEqual(lockedArgv(["build", "-p", "a"]), ["build", "--locked", "-p", "a"]);
  assert.deepEqual(lockedArgv(["+nightly", "run", "--", "x"]), ["+nightly", "run", "--locked", "--", "x"]);
  assert.deepEqual(lockedArgv(["--color", "always", "test"]), ["--color", "always", "test", "--locked"]);
  assert.deepEqual(lockedArgv(["build", "--frozen"]), ["build", "--frozen"]);
  assert.deepEqual(lockedArgv(["build", "--locked"]), ["build", "--locked"]);
  assert.deepEqual(lockedArgv(["run", "--", "--locked"]), ["run", "--locked", "--", "--locked"], "after -- the flag is the program's, not Cargo's");
  for (const argv of [["metadata", "--no-deps"], ["about", "generate"], ["--version"], ["fmt", "--check"]]) assert.deepEqual(lockedArgv(argv), argv);
});
