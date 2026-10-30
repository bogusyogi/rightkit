// The package stage of `rightkit-gate --to release --unit npm:@rightkit/release` on a broker host, end to end up to the broker: the
// REAL swap-helper step (prepareSwapHelper) runs through the REAL carried seam (runCargoSync -> runCarriedCargo -> carryBrokerCargo)
// against the fixture broker (test-support/fake-broker.mjs: a fake `rustright` client that applies the reader's carrier rules and
// signs receipts in-test), and the argv and directory the broker actually received are what the stage's expected commands
// (stage-commands.mjs, the same rows the gate and the hosted admission use) are checked against, through the gate's real
// receipt collection (runUnitGate). No cargo, no real broker, no signing identity.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { installCarrier } from "./carried-cargo.mjs";
import { runCargoSync } from "./managed-cargo.mjs";
import { carriedCallNonce, qualificationNonce } from "./provenance-verify.mjs";
import { brokerSection } from "./snapshot-broker.mjs";
import { expectedStageCommands, ANY_ABSOLUTE_PATH, assignJobs } from "./stage-commands.mjs";
import { prepareSwapHelper, SWAP_HELPER_CARGO_ARGS, SWAP_HELPER_METADATA_ARGS, SWAP_HELPER_PACKAGE } from "./swap-helper-bundle.mjs";
import { defaultStageCommands, runUnitGate } from "./unit-gate.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const POSIX = process.platform === "win32" ? "the fixture wrapper is POSIX only" : false;
const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const GATE = "/gate/bin/rightkit-gate.mjs";
const IDENTITY = "Developer ID Application: Orthic Labs (TEAM123456)";
const commit = "a".repeat(40), tree = "b".repeat(40);
const unit = { kind: "npm", name: SWAP_HELPER_PACKAGE, version: "1.0.0" };
const sha = (s) => createHash("sha256").update(s).digest("hex");

/** A fixture broker host whose snapshot and checkout both hold crates/rightkit-fs as its own Cargo workspace (rightkit#1285), with the carrier installed. */
function host(t) {
  const fx = makeFakeBroker(t, { readOnlySnapshots: true });
  for (const root of [fx.checkout, fx.snapshot]) {
    fs.mkdirSync(path.join(root, "crates", "rightkit-fs", "src"), { recursive: true });
    fs.writeFileSync(path.join(root, "crates", "rightkit-fs", "Cargo.toml"), '[workspace]\n[package]\nname = "rightkit-fs"\nversion = "0.1.0"\n');
    fs.writeFileSync(path.join(root, "crates", "rightkit-fs", "src", "lib.rs"), "\n");
  }
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { "crates/rightkit-fs": path.join(fx.checkout, "crates", "rightkit-fs") }, section: brokerSection(fx.sb), platform: process.platform, env: {} });
  t.after(() => installed.cleanup());
  // What the build leaves and what metadata prints: the broker's pinned output directory.
  fs.mkdirSync(path.join(fx.outputDir, "release"), { recursive: true });
  fs.writeFileSync(path.join(fx.outputDir, "release", "rightkit-swap"), "macho-bytes");
  fx.setBehavior({ stdout: `${JSON.stringify({ target_directory: fx.outputDir })}\n` });
  const env = { PATH: process.env.PATH, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE, ...installed.env };
  // `run` as local-publish hands it to prepareSwapHelper: a `cargo` step of a snapshot stage, which build-launch.mjs turns into the carried seam.
  const run = (_command, args, options) => runCargoSync(args, { ...options, encoding: "utf8" }, { platform: "darwin" });
  return { fx, installed, env, run };
}

const swapDeps = (h, packageDir, over = {}) => ({
  workspaceRoot: h.fx.snapshot, packageDir, platform: "darwin", env: h.env, run: h.run,
  resolveIdentity: () => IDENTITY,
  signOne: () => ({ status: 0, stdout: "", stderr: "" }),
  inspect: (_cmd, args) => (args.includes("--verify") ? "valid on disk\nsatisfies its designated requirement\n" : `Authority=${IDENTITY}\nTimestamp=Oct 17 2026\n`),
  ...over,
});

/** The jobs the broker would report for what the fixture client received, as the gate fetches them (`rustright status <id>`). */
const jobsOf = (fx) => fx.calls().map((c) => ({ id: c.id, payload: { request: { kind: "cargo", requestedArgv: c.argv, workspace: c.cwd } } }));

test("the swap helper's build and metadata reach the broker in the carried form and run in the crate's checkout directory; the package stage's expected commands are exactly that", { skip: POSIX }, () => {
  // (the carried form is asserted against the fixture client's own record below)
  const rows = expectedStageCommands({ kind: "npm", unit: SWAP_HELPER_PACKAGE, config: {}, manifest: { scripts: { test: "x" } }, platform: "darwin" }).package;
  const [build, metadata, pack] = rows;
  assert.deepEqual(build.argvs, [["build", "--locked", "--release", "-p", "rightkit-fs", "--features", "fs", "--bin", "rightkit-swap"]]);
  assert.deepEqual(metadata.argvs, [["metadata", "--format-version", "1", "--no-deps"]]);
  assert.deepEqual([build.cwd, metadata.cwd, pack.cwd], ["crates/rightkit-fs", "crates/rightkit-fs", undefined]);
  assert.deepEqual(pack.argvs[0], ["--dir", "packages/release", "pack", "--pack-destination", ANY_ABSOLUTE_PATH], "packed from the snapshot: a relative --dir");
});

test("end to end: the real swap step through the real seam sends the fixture broker what the stage expects, and the gate's receipt collection accepts it for npm:@rightkit/release", { skip: POSIX }, async (t) => {
  const h = host(t);
  const scratch = fs.mkdtempSync(path.join(h.fx.root, "pack-"));
  // The helper is written into the unit's own directory of the snapshot (a declared stage output), not a scratch copy.
  const packDir = path.join(h.fx.snapshot, "packages", "release");
  fs.mkdirSync(packDir, { recursive: true });
  const built = prepareSwapHelper(swapDeps(h, packDir));
  assert.equal(built.signed, true);
  assert.equal(fs.readFileSync(path.join(packDir, "bin", "darwin", "rightkit-swap"), "utf8"), "macho-bytes");
  // What the broker actually received.
  const calls = h.fx.calls();
  assert.deepEqual(calls.map((c) => c.argv), [
    ["build", "--locked", "--release", "-p", "rightkit-fs", "--features", "fs", "--bin", "rightkit-swap"],
    ["metadata", "--format-version", "1", "--no-deps"],
  ], "no --manifest-path (the client refuses to rewrite an attested call), --locked added");
  assert.deepEqual(calls.map((c) => c.cwd), [path.join(h.fx.checkout, "crates", "rightkit-fs"), path.join(h.fx.checkout, "crates", "rightkit-fs")], "run in the crate's directory of the checkout");
  assert.deepEqual(await h.installed.problems(h.fx.sb), [], "both calls verified against the broker's signed receipts");

  // The gate's own collection over the REAL expected commands of npm:@rightkit/release (ci/gate.config.json and its package.json at the repository).
  const stageCommands = ({ unit: u, platform }) => defaultStageCommands({ unit: u, root: REPO, platform });
  const expected = stageCommands({ unit, platform: "darwin" });
  const fill = (row) => row.argvs[0].map((a) => (typeof a === "string" ? a : path.join(scratch, "x")));
  const pnpmJob = (id, row) => ({ id, payload: { request: { kind: "adapter", adapter: "rightkit-pnpm", requestedArgv: fill(row), workspace: h.fx.snapshot } } });
  const packJobs = [...jobsOf(h.fx), pnpmJob("pack-1", expected.package.at(-1))];
  const compileJobs = expected.compile.map((row, i) => pnpmJob(`compile-${i + 1}`, row));
  const byId = new Map([...packJobs, ...compileJobs].map((j) => [j.id, j]));
  const fetchReceipt = (id) => ({ id, provenance: { schema: "rustright.provenance.v1", payload: { jobId: id, ...byId.get(id).payload }, signature: { alg: "ES256", keyId: "k", value: "ab" } } });

  // The package stage's collection: every expected command has a receipt of its own job.
  assert.deepEqual(assignJobs(expected.package, packJobs.map((j) => ({ id: j.id, payload: j.payload })), { checkout: h.fx.checkout }).missing, []);
  assert.deepEqual(assignJobs(expected.package, packJobs.map((j) => ({ id: j.id, payload: j.payload })), { checkout: h.fx.checkout }).unmatched, []);

  // The same through runUnitGate (release: attested stages), with the owner returning the broker job ids of each stage.
  const artifactFile = path.join(scratch, "artifact.tgz"); fs.writeFileSync(artifactFile, "artifact");
  const result = { ...unit, commit, artifactFile, artifactSha256: sha("artifact"), contentSha256: sha("content"), contentExcluded: ["package/bin/darwin/rightkit-swap", "package/bin/darwin/rightkit-swap.sign.json"] };
  const owner = {
    compileLocalUnit: async () => ({}), localUnitCompileRequests: () => compileJobs.map((j) => j.id),
    packageLocalUnit: async () => ({ ...result, brokerRequests: packJobs.map((j) => j.id) }),
    qualifyLocalUnit: async () => ({ ...result, qualifiedOnly: true, evidencePath: "gate.json" }), closeLocalUnit: async () => {},
  };
  let qualification;
  const errs = [];
  const run = (over = {}) => runUnitGate({ selector: `npm:${SWAP_HELPER_PACKAGE}`, to: "release", root: h.fx.snapshot, commonDir: h.fx.snapshot, commit, tree, executor: { kind: "broker" }, env: {}, platform: "darwin", resolve: () => unit, verifyReceipts: () => {}, check: async () => 0, loadOwner: async () => owner, out: () => {}, err: (l) => errs.push(l), source: { checkout: h.fx.checkout, commit, tree }, fetchReceipt, stageCommands,
    releaser: async ({ qualification: file }) => { qualification = JSON.parse(fs.readFileSync(file, "utf8")); return { registry: "npm" }; }, ...over });
  assert.equal(await run(), 0, errs.join("\n"));
  assert.deepEqual(qualification.stages.package.receipts.map((r) => r.id).sort(), packJobs.map((j) => j.id).sort(), "the build, the metadata read and the pack each carry their own signed receipt");

  // The same argv from another crate's directory is not the stage's command: nothing is carried and the stage owes the receipt.
  const other = (job) => (job.payload.request.kind === "cargo" ? { ...job, payload: { request: { ...job.payload.request, workspace: path.join(h.fx.checkout, "crates", "rightkit-other") } } } : job);
  for (const job of packJobs.map(other)) byId.set(job.id, job);
  errs.length = 0;
  assert.equal(await run({ releaser: async () => assert.fail("not released") }), 1);
  assert.match(errs.join("\n"), /package stage's cargo build --locked --release -p rightkit-fs --features fs --bin rightkit-swap has no broker job receipt/);
  // ... and from the checkout's root, or from the snapshot instead of the checkout.
  for (const where of [h.fx.checkout, path.join(h.fx.snapshot, "crates", "rightkit-fs")]) {
    for (const job of packJobs) byId.set(job.id, job.payload.request.kind === "cargo" ? { ...job, payload: { request: { ...job.payload.request, workspace: where } } } : job);
    errs.length = 0;
    assert.equal(await run({ releaser: async () => assert.fail("not released") }), 1, where);
  }
});

test("B8: a carried call in a release-bound stage derives its nonce from the stage nonce, its directory and its argv: the very nonce the hosted admission recomputes from the stage's rows; the stage nonce never reaches the client", { skip: POSIX }, async (t) => {
  const h = host(t);
  const stageNonce = qualificationNonce({ commit, tree, unit: `${unit.kind}:${unit.name}`, stage: "package" });
  const rows = expectedStageCommands({ kind: "npm", unit: SWAP_HELPER_PACKAGE, config: {}, manifest: { scripts: { test: "x" } }, platform: "darwin" }).package;
  // The environment a release-bound package stage hands its steps (unit-gate.mjs stageEnv, then testEnv in local-publish.mjs): the stage nonce
  // in RIGHTKIT_ATTEST_STAGE_NONCE, which is no carrier variable, so the seam accepts it.
  const staged = { ...h.env, RIGHTKIT_ATTEST_STAGE_NONCE: stageNonce };
  for (const args of [SWAP_HELPER_CARGO_ARGS, SWAP_HELPER_METADATA_ARGS]) {
    const r = runCargoSync(args, { cwd: h.fx.snapshot, env: staged, encoding: "utf8" }, { platform: "darwin" });
    assert.equal(r.status, 0, r.stderr);
  }
  const calls = h.fx.calls();
  assert.equal(calls.length, 2);
  for (const [i, row] of rows.slice(0, 2).entries()) {
    const receipt = await h.fx.sb.receiptOf(calls[i].id);
    assert.deepEqual(calls[i].argv, row.argvs[0]);
    assert.equal(receipt.provenance.payload.nonce, carriedCallNonce({ stageNonce, cwd: row.cwd, argv: row.argvs[0] }), `${row.argvs[0][0]}: the nonce the admission derives from the row's directory and argv`);
    assert.notEqual(receipt.provenance.payload.nonce, stageNonce);
  }
  assert.notEqual((await h.fx.sb.receiptOf(calls[0].id)).provenance.payload.nonce, (await h.fx.sb.receiptOf(calls[1].id)).provenance.payload.nonce, "two commands, two nonces");
  assert.deepEqual(calls.map((c) => c.stageNonceSeen), [false, false], "the client never sees the stage nonce");
  assert.deepEqual(calls.map((c) => c.carrierEnvNames), calls.map(() => ["RUSTRIGHT_ATTEST_NONCE", "RUSTRIGHT_CALLING_GATE", "RUSTRIGHT_REQUEST_ID_FILE", "RUSTRIGHT_SNAPSHOT_REQUEST"]), "only the per-call carrier");
  assert.deepEqual(await h.installed.problems(h.fx.sb), [], "and the gate's own ledger verification accepts the derived nonces");
  // Without a stage nonce (a run that is not headed to qualify) the nonce is fresh, never derived; a malformed one is no stage nonce either.
  for (const bad of [undefined, "not-hex", "AB".repeat(32), "ab".repeat(31)]) {
    const before = h.fx.calls().length;
    const r = runCargoSync(SWAP_HELPER_METADATA_ARGS, { cwd: h.fx.snapshot, env: { ...h.env, ...(bad === undefined ? {} : { RIGHTKIT_ATTEST_STAGE_NONCE: bad }) }, encoding: "utf8" }, { platform: "darwin" });
    assert.equal(r.status, 0, r.stderr);
    const id = h.fx.calls()[before].id;
    assert.notEqual((await h.fx.sb.receiptOf(id)).provenance.payload.nonce, carriedCallNonce({ stageNonce: bad ?? "", cwd: "crates/rightkit-fs", argv: rows[1].argvs[0] }), String(bad));
  }
});
