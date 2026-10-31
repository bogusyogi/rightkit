// rightkit#1301 F1: what the broker does with each cargo subcommand a snapshot stage carries. The table below is a fixture of rustright-policy command_policy.rs `cargo_capability` (the classification)
// and rustright-broker job_service/service.rs ("a snapshot request must compile"), judged by the gate's PRODUCTION grammar (cargo-shapes.mjs, snapshot-metadata.mjs). Every subcommand the gate,
// right-release's seam and the wrapper carry is in it.
//
// Evidence labels. "data:" titles call the real production functions with constructed argv and are NOT broker evidence. "real broker:" titles carry each row through the production carried call to an
// isolated instance of the installed RustRight broker (test-support/real-broker.mjs). There is no simulated broker in this file: the pre-fix and post-fix broker models of the old fixture are gone.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { GATE_EXTERNAL_COMMANDS, carriedShapeDecision, classifyCargoArgv } from "./cargo-shapes.mjs";
import { WRAPPER_COMMANDS } from "./cargo-target-shim.mjs";
import { acquireBuildOwnership, brokerLeaseState } from "./build-ownership.mjs";
import { brokerSection, carryBrokerCargo } from "./snapshot-broker.mjs";
import { cargoSubcommandOf, lockedArgv } from "./snapshot-request.mjs";
import { carriedScanRefusal } from "./snapshot-metadata.mjs";
import { startRealBroker } from "./test-support/real-broker.mjs";

const READ_ONLY_NEEDS = "requires rustright read-only snapshot acceptance (rightkit#1301 F1)";
const SNAPSHOT_MUST_COMPILE = "RK_CHECK_ERROR: a snapshot request must compile";

const hasFlag = (argv, flag) => argv.includes(flag);

/**
 * `compile` (admitted as a snapshot request), `read-only` (CONTROL, ANALYSIS or WORKSPACE_OBSERVE), `write` (WORKSPACE_WRITE: refused), `denied`.
 * GB-82: whether a request compiles or is refused is the production grammar (cargo-shapes.mjs, exercised against RustRight's cargo-shapes vector), not a grammar of
 * this fixture's own; only the descriptive read-only/write split below is the fixture's.
 */
function classifyCargo(argv) {
  const row = classifyCargoArgv(argv, { registered: GATE_EXTERNAL_COMMANDS });
  if (row.refused) return row.refused.code === "RK_UNKNOWN_COMMAND" ? "read-only" : "denied";
  if (row.compile) return "compile";
  const words = [...argv];
  const command = row.command;
  const locked = hasFlag(words, "--locked") || hasFlag(words, "--frozen");
  if (command === "metadata" && hasFlag(words, "--no-deps")) return "read-only";
  if (command === "metadata" || command === "tree" || command === "fetch") return locked ? "read-only" : "write";
  if (command === "fmt") return words.includes("--check") ? "read-only" : "write";
  if (["new", "init", "update", "vendor", "generate-lockfile", "add", "remove"].includes(command)) return "write";
  return "read-only"; // CONTROL, ANALYSIS and an unclassified plugin (about, audit, deny) are no BUILD capability: never a compile
}

/**
 * What the broker accepts as a snapshot request (GB-82: the vector's own decision): `compile` rows, and, unless `readOnly` is false (a broker that predates
 * RustRight's read-only snapshot acceptance), the carried read-only shapes, none of which carries cargo's `--config` or `-Z`. The files `deny` reads and the database
 * directory `audit` reads must also be absolute, exist, be of the right kind and lie OUTSIDE the live checkout (`checkout`; with none only the shape is judged).
 */
function acceptedAfterFix(argv, { checkout = null, readOnly = true } = {}) {
  const row = classifyCargoArgv(argv, { registered: GATE_EXTERNAL_COMMANDS });
  if (row.refused || row.carriedRefusal) return false;
  if (row.compile) return true;
  if (!readOnly || !row.carriedReadOnly) return false;
  if (["--version", "-V", "version", "locate-project", "read-manifest", "about"].includes(row.command)) return true;
  return carriedScanRefusal(row.normalizedArgv, { checkout }) === null;
}

/**
 * Every cargo subcommand a snapshot stage carries, with the broker's classification today. `callers` names who carries it:
 * `seam` (runCargoSync of right-release), `wrapper` (the broker-mode PATH wrapper for third-party children), `gate` (the gate's own),
 * `tauri` (the Tauri CLI, through the wrapper).
 */
const CARGO_CLASSIFICATION = Object.freeze([
  { argv: ["build", "--locked", "-p", "a"], class: "compile", callers: ["seam", "wrapper", "tauri"] },
  { argv: ["check"], class: "compile", callers: ["seam", "wrapper", "gate"] },
  { argv: ["test", "--locked"], class: "compile", callers: ["seam", "wrapper", "gate"] },
  { argv: ["bench"], class: "compile", callers: ["wrapper"] },
  { argv: ["clippy", "--locked"], class: "compile", callers: ["wrapper", "gate"] },
  { argv: ["doc"], class: "compile", callers: ["wrapper", "gate"] },
  { argv: ["rustc", "--", "-vV"], class: "compile", callers: ["wrapper"] },
  { argv: ["rustdoc"], class: "compile", callers: ["wrapper"] },
  { argv: ["run", "-p", "a"], class: "compile", callers: ["seam", "wrapper"] },
  { argv: ["package", "-p", "a"], class: "compile", callers: ["wrapper"] },
  { argv: ["metadata", "--format-version", "1", "--no-deps"], class: "read-only", callers: ["seam", "wrapper", "tauri"], needs: READ_ONLY_NEEDS },
  { argv: ["metadata", "--offline", "--format-version", "1", "--no-deps"], class: "read-only", callers: ["seam", "wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["metadata", "--frozen", "--format-version", "1"], class: "read-only", callers: ["seam", "wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["metadata", "--locked", "--format-version", "1"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["locate-project", "--workspace"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["--version"], class: "read-only", callers: ["wrapper", "seam"], needs: READ_ONLY_NEEDS },
  { argv: ["version"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["help"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["read-manifest"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["tree", "--locked"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["pkgid", "--locked"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["verify-project", "--locked"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["fetch", "--locked"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["fmt", "--check"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["audit"], class: "read-only", callers: ["seam"], needs: READ_ONLY_NEEDS },
  { argv: ["deny", "check"], class: "read-only", callers: ["wrapper"], needs: READ_ONLY_NEEDS },
  { argv: ["about", "generate", "--frozen", "--fail"], class: "read-only", callers: ["seam"], needs: READ_ONLY_NEEDS },
  { argv: ["about", "generate", "--locked", "--fail"], class: "read-only", callers: [], needs: READ_ONLY_NEEDS },
  { argv: ["fmt"], class: "write", callers: [] },
  { argv: ["metadata", "--format-version", "1"], class: "write", callers: [] },
  { argv: ["clean"], class: "denied", callers: [] },
  { argv: ["install", "x"], class: "denied", callers: [] },
]);

const POSIX = process.platform === "win32" ? "the carried client environment is POSIX only" : false;
const subcommandOf = (argv) => (["--version", "-V", "-vV"].includes(argv[0]) ? argv[0] : cargoSubcommandOf(argv));

test("data: the table is the broker's classification: only BUILD capabilities compile; every read-only row names what it needs", () => {
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
  assert.equal(classifyCargo(["+nightly", "build"]), "denied", "GB-82: RustRight refuses a +toolchain of a managed request (RK_REDIRECT_REFUSED), so it is no compile there");
});

test("data: the table covers every subcommand the wrapper binds or passes through, and every subcommand right-release's own calls carry", () => {
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

const COMMIT = { sha: "c".repeat(40), tree: "e".repeat(40) };
const WORKSPACES = { "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n', "crates/a/src/lib.rs": "\n" };
const writeTree = (root, files) => { for (const [rel, body] of Object.entries(files)) { const f = path.join(root, rel); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, body); } };
const NOW = Date.now();
const probe = () => ({ platform: "darwin", identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW - 60_000, group: 4242 } : { alive: false, startedMs: null, group: null }), bootTime: () => NOW - 86_400_000, groupSnapshot: () => ({ members: [], byPid: new Map([[1, { pid: 1, ppid: 0, startedMs: NOW - 9e7 }]]), own: new Set([process.pid]) }), tokenHolders: () => null, snapshotUsers: () => null, ancestors: () => [{ pid: 5000, startedMs: NOW - 9e6 }] });
const GATE = "/gate/bin/rightkit-gate.mjs";

test("real broker: every carried shape of the table is sent to the real broker and every other is refused by the gate before anything is marked or submitted; the real broker never answers a verified result for a snapshot it does not hold", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "cargo-classes" });
  t.after(() => rb.stop());
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-cc-"))); t.after(() => fs.rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 }));
  const checkout = path.join(root, "checkout"); const snapshot = path.join(rb.dataRoot, "snapshots", "0123456789abcdef");
  writeTree(checkout, WORKSPACES); writeTree(snapshot, WORKSPACES);
  const own = acquireBuildOwnership({ file: path.join(root, "build-ownership", "owner.json"), platform: "darwin", probe: probe(), lease: () => { throw new Error("no lease query expected"); }, env: {}, exportEnv: false, stage: "compile", snapshot });
  const marks = [];
  const section = brokerSection({
    controlRoot: rb.controlRoot, commit: COMMIT, snapshot: { path: snapshot, expectedTree: COMMIT.tree }, declaredInputs: [], handle: own.handle, pins: { host: "host-1", keyId: "0".repeat(64), publicKey: "04".padEnd(130, "0"), brokerDigest: "b".repeat(64) },
    markBrokerSubmit: (callHandle) => { marks.push(callHandle); return own.markBrokerSubmit(callHandle); }, markBrokerUnsent: own.markBrokerUnsent, checkout, outputRoots: [path.join(rb.dataRoot, "rightkit-build-control", "ws")], platform: process.platform, client: rb.clientPath,
  });
  const env = { PATH: process.env.PATH, RUSTRIGHT_CALLING_GATE: GATE, ...rb.env };
  const atBroker = () => (fs.existsSync(rb.receiptsDir) ? fs.readdirSync(rb.receiptsDir).length : 0);
  let sent = 0;
  for (const row of CARGO_CLASSIFICATION) {
    const before = marks.length;
    const r = carryBrokerCargo({ words: row.argv, cwd: path.join(snapshot, "crates", "a"), env, section, workspace: snapshot, checkoutWorkspace: checkout, record: () => {}, err: () => {}, stdio: "capture", deadlineMs: 20_000 });
    const decision = carriedShapeDecision(lockedArgv(row.argv));
    assert.notEqual(r.outcome.status, "verified", `cargo ${row.argv.join(" ")}: a snapshot the real broker does not hold is never a verified result`);
    if (decision.ok) { assert.ok(marks.length > before, `cargo ${row.argv.join(" ")} is a carried shape: it is submitted`); sent += 1; }
    else { assert.equal(r.outcome.status, "refused", `cargo ${row.argv.join(" ")}`); assert.equal(marks.length, before, `cargo ${row.argv.join(" ")} is refused before anything is marked or sent`); }
  }
  assert.ok(sent >= CARGO_CLASSIFICATION.filter((r) => r.class === "compile").length, "every compile row is carried");
  assert.ok(atBroker() <= marks.length, "the real broker holds no more requests than the gate marked");
});

test("data: with the post-fix admission only the lock-bound allowlist is accepted: metadata, fetch --locked, deny, audit --db --no-fetch and cargo about --frozen; every other read-only subcommand stays refused", () => {
  assert.equal(acceptedAfterFix(["metadata", "--no-deps"]), true);
  assert.equal(acceptedAfterFix(["metadata", "--frozen", "--format-version", "1"]), true);
  assert.equal(acceptedAfterFix(["metadata", "--locked"]), true, "with dependencies --locked is enough (online is allowed, rustright 52a9ed76)");
  assert.equal(acceptedAfterFix(["metadata", "--offline"]), false, "with dependencies it must be --locked or --frozen");
  // The scan shapes (rustright 52a9ed76): exactly these, each flag once, spaced form, files absolute and outside the live checkout.
  assert.equal(acceptedAfterFix(["fetch", "--locked"]), true);
  for (const no of [["fetch"], ["fetch", "--locked", "--offline"], ["fetch", "--locked=true"]]) assert.equal(acceptedAfterFix(no), false, no.join(" "));
  assert.equal(acceptedAfterFix(["deny", "--frozen", "--metadata-path", "/m.json", "--config", "/d.toml", "check"]), true);
  for (const no of [["deny", "check", "--hide-inclusion-graph"], ["deny", "--frozen", "--frozen", "check"], ["deny", "--config=/d.toml", "check"], ["deny", "--metadata-path", "m.json", "check"], ["deny", "--frozen"], ["deny", "--offline", "check"], ["deny", "--config", "/d.toml", "--", "check"]]) assert.equal(acceptedAfterFix(no), false, no.join(" "));
  assert.equal(acceptedAfterFix(["audit", "--db", "/db", "--no-fetch"]), true);
  for (const no of [["audit"], ["audit", "--db", "/db"], ["audit", "--no-fetch"], ["audit", "--db", "db", "--no-fetch"], ["audit", "--db", "/db", "--no-fetch", "--deny", "warnings"]]) assert.equal(acceptedAfterFix(no), false, no.join(" "));
  for (const no of [["metadata", "--no-deps", "--config", "x=y"], ["metadata", "--no-deps", "-Zfoo"], ["+nightly", "metadata", "--no-deps"], ["metadata", "--no-deps", "--manifest-path", "/x"], ["metadata", "--no-deps", "--", "x"]]) assert.equal(acceptedAfterFix(no), false, no.join(" "));
  assert.equal(acceptedAfterFix(["about", "generate", "--frozen"]), true);
  assert.equal(acceptedAfterFix(["about", "generate", "--locked"]), false);
});

test("data: every read-only call right-release itself carries is inside the post-fix allowlist (metadata --no-deps, or with dependencies --frozen; cargo about --frozen)", () => {
  const seamRows = CARGO_CLASSIFICATION.filter((r) => r.class === "read-only" && r.callers.includes("seam"));
  assert.ok(seamRows.length >= 3);
  for (const row of seamRows) if (row.argv[0] !== "--version" && row.argv[0] !== "audit") assert.equal(acceptedAfterFix(row.argv), true, `cargo ${row.argv.join(" ")}`);
  const here = (f) => fs.readFileSync(new URL(f, import.meta.url), "utf8");
  assert.match(here("stage-sidecar.mjs"), /"metadata", "--frozen", "--format-version", "1"\]/);
  assert.match(here("media-sidecar-catalog.mjs"), /\["metadata", "--frozen", "--format-version", "1"/);
  assert.match(here("cargo-license-notices.mjs"), /"about", "generate", "--frozen", "--fail"/);
  assert.match(here("cargo-contract.mjs"), /"metadata", "--no-deps"/);
});

test("data: a build or run is sent locked: --locked is added after the subcommand unless --locked or --frozen is already there (before any --)", () => {
  assert.deepEqual(lockedArgv(["build", "-p", "a"]), ["build", "--locked", "-p", "a"]);
  assert.deepEqual(lockedArgv(["+nightly", "run", "--", "x"]), ["+nightly", "run", "--locked", "--", "x"]);
  assert.deepEqual(lockedArgv(["--color", "always", "test"]), ["--color", "always", "test", "--locked"]);
  assert.deepEqual(lockedArgv(["build", "--frozen"]), ["build", "--frozen"]);
  assert.deepEqual(lockedArgv(["build", "--locked"]), ["build", "--locked"]);
  assert.deepEqual(lockedArgv(["run", "--", "--locked"]), ["run", "--locked", "--", "--locked"], "after -- the flag is the program's, not Cargo's");
  for (const argv of [["metadata", "--no-deps"], ["about", "generate"], ["--version"], ["fmt", "--check"]]) assert.deepEqual(lockedArgv(argv), argv);
});

test("data: [GB-82] the gate's admission IS RustRight's vector grammar: every row of RustRight's cargo-shapes vector that the broker accepts as a carried snapshot request is admitted, every refusal is not", async () => {
  const { loadVector } = await import("./test-support/vectors.mjs");
  const checkout = path.join(os.tmpdir(), "rk-cc-live-checkout");
  const rows = loadVector("cargo-shapes.fixtures.json").rows.filter((r) => !r.refused);
  for (const row of rows) {
    const expected = !row.carriedRefusal && (row.compile || (row.carriedReadOnly && !["deny", "audit"].includes(row.command)));
    const accepted = acceptedAfterFix(row.normalizedArgv, { checkout });
    assert.equal(accepted, expected, `${row.name}: the gate's admission`);
  }
  // The version probes are matched as the WHOLE argv, never as a flag anywhere: `build --version`-like forms are no probe.
  const { isVersionProbe } = await import("./cargo-shapes.mjs");
  for (const argv of [["-V"], ["--version"], ["-vV"], ["-Vv"], ["version"], ["--version", "--verbose"], ["-V", "--verbose"], ["-v", "-V"]]) assert.equal(isVersionProbe(argv), true, argv.join(" "));
  for (const argv of [["build", "--version"], ["--version", "build"], ["check", "-V"], ["version", "--verbose"], ["-V", "-V"], []]) assert.equal(isVersionProbe(argv), false, argv.join(" "));
});
