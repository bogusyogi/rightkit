// The package stage of `rightkit-gate --to release --unit npm:@rightkit/release` on a broker host, end to end up to the broker: the
// REAL swap-helper step (prepareSwapHelper) runs through the REAL carried seam (runCargoSync -> runCarriedCargo -> carryBrokerCargo)
// against the REAL installed RustRight broker (an isolated instance, test-support/real-broker.mjs) over the gate's real snapshot of a real git checkout,
// and the argv and directory the broker actually signed are what the stage's expected commands (stage-commands.mjs, the same rows the gate and the
// hosted admission use) are checked against, through the gate's real receipt matching (assignJobs). The signing step's own tools (identity lookup,
// codesign) are the only stand-ins: they are not the broker.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { OWNERSHIP_ENV, OWNERSHIP_FILE_ENV, brokerLeaseState, markCarriedSend } from "./build-ownership.mjs";
import { enrollBrokerPins } from "./broker-pins.mjs";
import { readLedger, UNSCOPED } from "./cargo-ledger.mjs";
import { CARRIED_CARGO_MAP_ENV, installCarrier, runCarriedAdapter } from "./carried-cargo.mjs";
import { runCargoSync } from "./managed-cargo.mjs";
import { carriedCallNonce, qualificationNonce } from "./provenance-verify.mjs";
import { syncSnapshot } from "./snapshot.mjs";
import { brokerSection, createSnapshotBroker } from "./snapshot-broker.mjs";
import { expectedStageCommands, ANY_ABSOLUTE_PATH, assignJobs } from "./stage-commands.mjs";
import { prepareSwapHelper, SWAP_HELPER_CARGO_ARGS, SWAP_HELPER_METADATA_ARGS, SWAP_HELPER_PACKAGE } from "./swap-helper-bundle.mjs";
import { defaultStageCommands } from "./unit-gate.mjs";
import { spawnSync } from "./background-process.mjs";
import { startRealBroker } from "./test-support/real-broker.mjs";

const POSIX = process.platform === "win32" ? "the carried adapter PATH is POSIX only" : false;
const GATE = "/gate/bin/rightkit-gate.mjs";
const IDENTITY = "Developer ID Application: Orthic Labs (TEAM123456)";
const unit = { kind: "npm", name: SWAP_HELPER_PACKAGE, version: "1.0.0" };

// ---- the real broker host: the installed RustRight broker (an isolated instance), a real git checkout, the gate's real snapshot of it, the enrolled pins of THAT broker, and the production snapshot broker over them ----
const HANDLE = "own-7";
const PLATFORM_KEY = process.platform === "win32" ? "win32" : process.platform === "darwin" ? "darwin" : "linux";
const LOCK = (name) => `version = 4\n\n[[package]]\nname = "${name}"\nversion = "0.1.0"\n`;
const BASE_FILES = {
  ".gitignore": "target/\nnode_modules/\n",
  "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\nresolver = "2"\n', "Cargo.lock": LOCK("a"), "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\nedition = "2021"\n', "crates/a/src/lib.rs": "\n",
  "native/Cargo.toml": '[workspace]\nmembers = []\n[package]\nname = "b"\nversion = "0.1.0"\nedition = "2021"\n', "native/Cargo.lock": LOCK("b"), "native/src/lib.rs": "\n",
  "packages/release/package.json": '{"name":"@x/release","version":"0.0.0"}\n',
};
const gitIn = (cwd, ...args) => { const r = spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", ...args], { cwd, encoding: "utf8" }); if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr}`); return String(r.stdout).trim(); };
/** The enrolled pins of the broker instance `rb`: its own attestation key (public fields only), host and broker digest as its own `doctor --json` reports them. */
function instancePins(rb) {
  const key = JSON.parse(fs.readFileSync(path.join(rb.controlRoot, "attestation", "key.json"), "utf8"));
  const run = rb.run(["doctor", "--json", "--no-scan"], {});
  const doctor = JSON.parse(run.stdout);
  const find = (value, name) => { if (value && typeof value === "object") { if (typeof value[name] === "string") return value[name]; for (const inner of Object.values(value)) { const hit = find(inner, name); if (hit) return hit; } } return null; };
  const host = find(doctor, "hostName") ?? find(doctor, "host"); const brokerDigest = find(doctor, "brokerDigest");
  if (!host || !brokerDigest) throw new Error(`the isolated broker's doctor report names no host or brokerDigest to enroll: ${run.stdout.slice(0, 300)}`);
  return { publicKey: key.publicKey, keyId: key.keyId, host, brokerDigest };
}
/** `files` are committed over BASE_FILES; the snapshot is the gate's own (syncSnapshot) inside the broker's data root; `sb` is createSnapshotBroker's. */
async function realBrokerHost(t, name, files = {}) {
  const rb = await startRealBroker({ name });
  t.after(() => rb.stop());
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), `rk-${name}-`)));
  t.after(() => fs.rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 }));
  const checkout = path.join(root, "checkout"); fs.mkdirSync(checkout);
  for (const [rel, body] of Object.entries({ ...BASE_FILES, ...files })) { const f = path.join(checkout, rel); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, body); }
  gitIn(checkout, "init", "-q", "-b", "main"); gitIn(checkout, "add", "-A"); gitIn(checkout, "commit", "-q", "-m", "c");
  const snap = syncSnapshot({ checkout, rev: "HEAD", root: path.join(rb.dataRoot, "snapshots"), managedRoot: rb.dataRoot });
  enrollBrokerPins({ dataRoot: rb.dataRoot, platform: PLATFORM_KEY, ...instancePins(rb) });
  const ownership = path.join(root, "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(ownership), { recursive: true });
  fs.writeFileSync(ownership, JSON.stringify({ handle: HANDLE, broker: { submitted: false } }));
  const submits = [];
  const sb = createSnapshotBroker({
    env: rb.env, platform: process.platform, checkout, commit: { sha: snap.commit, tree: snap.tree }, snapshot: { path: snap.path, expectedTree: snap.tree },
    ownership: { handle: HANDLE, markBrokerSubmit: (callHandle) => { submits.push(callHandle); return markCarriedSend({ file: ownership, handle: HANDLE, callHandle }); } },
    client: rb.clientPath, gateFile: path.join(checkout, "x", "rightkit-gate.mjs"), dataRoot: rb.dataRoot, lease: (q) => brokerLeaseState(q),
  });
  return { rb, root, checkout, snapshot: snap.path, commit: { sha: snap.commit, tree: snap.tree }, sb, submits, ownershipEnv: { [OWNERSHIP_ENV]: HANDLE, [OWNERSHIP_FILE_ENV]: ownership } };
}

const FS_FILES = {
  "crates/rightkit-fs/Cargo.toml": '[workspace]\n[package]\nname = "rightkit-fs"\nversion = "0.1.0"\nedition = "2021"\n[features]\nfs = []\n[[bin]]\nname = "rightkit-swap"\npath = "src/main.rs"\nrequired-features = ["fs"]\n',
  "crates/rightkit-fs/Cargo.lock": LOCK("rightkit-fs"), "crates/rightkit-fs/src/lib.rs": "\n", "crates/rightkit-fs/src/main.rs": "fn main() {}\n",
};

/** A real broker host whose snapshot and checkout both hold crates/rightkit-fs as its own Cargo workspace (rightkit#1285), with the carrier installed. */
async function host(t) {
  const fx = await realBrokerHost(t, "swap-package", FS_FILES);
  const crateDir = path.join(fx.snapshot, "crates", "rightkit-fs");
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { "crates/rightkit-fs": path.join(fx.checkout, "crates", "rightkit-fs"), ".": fx.checkout }, section: brokerSection(fx.sb), platform: process.platform, env: {} });
  t.after(() => installed.cleanup());
  const env = { ...fx.rb.env, PATH: process.env.PATH, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE, ...fx.ownershipEnv, ...installed.env };
  // `run` as local-publish hands it to prepareSwapHelper: a `cargo` step of a snapshot stage, which build-launch.mjs turns into the carried seam.
  const run = (_command, args, options) => runCargoSync(args, { ...options, encoding: "utf8" }, { platform: process.platform });
  const entries = () => readLedger(installed.ledgerDir, UNSCOPED).entries;
  const adapter = (name, args, cwd = fx.snapshot) => runCarriedAdapter(name, args, { cwd, env, stdio: "capture" }, { map: JSON.parse(fs.readFileSync(installed.env[CARRIED_CARGO_MAP_ENV], "utf8")), err: () => {} });
  return { fx, installed, env, run, entries, adapter, crateDir };
}

const swapDeps = (h, packageDir, over = {}) => ({
  workspaceRoot: h.fx.snapshot, packageDir, platform: "darwin", env: h.env, run: h.run,
  resolveIdentity: () => IDENTITY,
  signOne: () => ({ status: 0, stdout: "", stderr: "" }),
  inspect: (_cmd, args) => (args.includes("--verify") ? "valid on disk\nsatisfies its designated requirement\n" : `Authority=${IDENTITY}\nTimestamp=Oct 17 2026\n`),
  ...over,
});

/** The jobs the broker signed, as the gate fetches them: each ledgered request's real signed receipt (`rustright status <id>`). */
const jobsOf = async (h) => Promise.all(h.entries().map(async (e) => { const r = await h.fx.sb.receiptOf(e.requestId); return { id: e.requestId, receipt: r, payload: r.provenance.payload }; }));

test("data: the swap helper's build and metadata are expected in the carried form and in the crate's checkout directory; the package stage's expected commands are exactly that", () => {
  const rows = expectedStageCommands({ kind: "npm", unit: SWAP_HELPER_PACKAGE, config: {}, manifest: { scripts: { test: "x" } }, platform: "darwin" }).package;
  const [build, metadata, pack] = rows;
  assert.deepEqual(build.argvs, [["build", "--locked", "--release", "-p", "rightkit-fs", "--features", "fs", "--bin", "rightkit-swap"]]);
  assert.deepEqual(metadata.argvs, [["metadata", "--format-version", "1", "--no-deps"]]);
  assert.deepEqual([build.cwd, metadata.cwd, pack.cwd], ["crates/rightkit-fs", "crates/rightkit-fs", undefined]);
  assert.deepEqual(pack.argvs[0], ["--dir", "packages/release", "pack", "--pack-destination", ANY_ABSOLUTE_PATH], "packed from the snapshot: a relative --dir");
});

test("end to end on the real broker: the real swap step through the real seam sends the broker what the stage expects, and the gate's receipt matching accepts the broker's signed receipts for npm:@rightkit/release's package stage", { skip: POSIX }, async (t) => {
  const h = await host(t);
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "rk-local-publish-"));
  t.after(() => fs.rmSync(scratch, { recursive: true, force: true }));
  // The helper is written into the unit's own directory of the snapshot (a declared stage output), not a scratch copy.
  const packDir = path.join(h.fx.snapshot, "packages", "release");
  const built = prepareSwapHelper(swapDeps(h, packDir));
  assert.equal(built.signed, true);
  assert.ok(fs.statSync(path.join(packDir, "bin", "darwin", "rightkit-swap")).size > 0, "the broker's own build output was copied");
  // What the broker actually signed.
  const swapJobs = await jobsOf(h);
  assert.deepEqual(swapJobs.map((j) => j.payload.request.requestedArgv), [
    ["build", "--locked", "--release", "-p", "rightkit-fs", "--features", "fs", "--bin", "rightkit-swap"],
    ["metadata", "--format-version", "1", "--no-deps"],
  ], "no --manifest-path (the client refuses to rewrite an attested call), --locked added");
  assert.deepEqual(swapJobs.map((j) => j.payload.request.workspace), [h.crateDir, h.crateDir].map((d) => path.join(h.fx.checkout, path.relative(h.fx.snapshot, d))), "run in the crate's directory of the checkout");
  assert.deepEqual(await h.installed.problems(h.fx.sb), [], "both calls verified against the broker's signed receipts");

  // The package stage's pack, a real carried adapter job on the same broker.
  const out = path.join(scratch, "pack"); fs.mkdirSync(out);
  const pack = h.adapter("rightkit-pnpm", ["--dir", "packages/release", "pack", "--pack-destination", out]);
  assert.equal(pack.status, 0, pack.stderr);

  // The gate's own collection over the REAL expected commands of npm:@rightkit/release (ci/gate.config.json and its package.json at the repository).
  const expected = defaultStageCommands({ unit, root: path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", ".."), platform: "darwin" });
  const jobs = await jobsOf(h);
  const collected = assignJobs(expected.package, jobs.map((j) => ({ id: j.id, payload: j.payload })), { checkout: h.fx.checkout });
  assert.deepEqual(collected.missing, [], "every expected command has a receipt of its own job");
  assert.deepEqual(collected.unmatched, [], "and every job is one of the stage's commands");

  // The same argv from another crate's directory is not the stage's command: nothing is matched and the stage owes the receipt (data: the real signed payload with its workspace edited).
  const moved = (workspace) => jobs.map((j) => ({ id: j.id, payload: j.payload.request.kind === "cargo" ? { ...j.payload, request: { ...j.payload.request, workspace } } : j.payload }));
  const owed = assignJobs(expected.package, moved(path.join(h.fx.checkout, "crates", "rightkit-other")), { checkout: h.fx.checkout });
  assert.ok(owed.missing.some((row) => row.argvs[0].includes("--bin")), "the build has no broker job receipt from another crate's directory");
  // ... and from the checkout's root, or from the snapshot instead of the checkout.
  for (const where of [h.fx.checkout, path.join(h.fx.snapshot, "crates", "rightkit-fs")]) {
    assert.ok(assignJobs(expected.package, moved(where), { checkout: h.fx.checkout }).missing.length > 0, where);
  }
});

test("B8: a carried call in a release-bound stage derives its nonce from the stage nonce, its directory and its argv: the very nonce the hosted admission recomputes from the stage's rows, as the real broker signs it", { skip: POSIX }, async (t) => {
  const h = await host(t);
  const stageNonce = qualificationNonce({ commit: h.fx.commit.sha, tree: h.fx.commit.tree, unit: `${unit.kind}:${unit.name}`, stage: "package" });
  const rows = expectedStageCommands({ kind: "npm", unit: SWAP_HELPER_PACKAGE, config: {}, manifest: { scripts: { test: "x" } }, platform: "darwin" }).package;
  // The environment a release-bound package stage hands its steps: the stage nonce in RIGHTKIT_ATTEST_STAGE_NONCE, which is no carrier variable, so the seam accepts it.
  const staged = { ...h.env, RIGHTKIT_ATTEST_STAGE_NONCE: stageNonce };
  for (const args of [SWAP_HELPER_CARGO_ARGS, SWAP_HELPER_METADATA_ARGS]) {
    const r = runCargoSync(args, { cwd: h.crateDir, env: staged, encoding: "utf8" }, { platform: process.platform });
    assert.equal(r.status, 0, r.stderr);
  }
  const jobs = await jobsOf(h);
  assert.equal(jobs.length, 2);
  for (const [i, row] of rows.slice(0, 2).entries()) {
    assert.deepEqual(jobs[i].payload.request.requestedArgv, row.argvs[0]);
    assert.equal(jobs[i].payload.nonce, carriedCallNonce({ stageNonce, cwd: row.cwd, argv: row.argvs[0] }), `${row.argvs[0][0]}: the nonce the admission derives from the row's directory and argv`);
    assert.notEqual(jobs[i].payload.nonce, stageNonce);
  }
  assert.notEqual(jobs[0].payload.nonce, jobs[1].payload.nonce, "two commands, two nonces");
  assert.deepEqual(await h.installed.problems(h.fx.sb), [], "and the gate's own ledger verification accepts the derived nonces");
  // Without a stage nonce (a run that is not headed to qualify) the nonce is fresh, never derived; a malformed one is no stage nonce either.
  for (const bad of [undefined, "not-hex", "AB".repeat(32), "ab".repeat(31)]) {
    const before = h.entries().length;
    const r = runCargoSync(SWAP_HELPER_METADATA_ARGS, { cwd: h.crateDir, env: { ...h.env, ...(bad === undefined ? {} : { RIGHTKIT_ATTEST_STAGE_NONCE: bad }) }, encoding: "utf8" }, { platform: process.platform });
    assert.equal(r.status, 0, r.stderr);
    const receipt = await h.fx.sb.receiptOf(h.entries()[before].requestId);
    assert.notEqual(receipt.provenance.payload.nonce, carriedCallNonce({ stageNonce: bad ?? "", cwd: "crates/rightkit-fs", argv: rows[1].argvs[0] }), String(bad));
  }
});
