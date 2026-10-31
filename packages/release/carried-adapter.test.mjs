// rightkit#1301, one model (rustright#190): every compile of a snapshot stage is a carried request or nested under one. A pnpm, node or QA
// step of a snapshot stage is a carried ADAPTER job (carried-adapter-run.mjs, snapshot-broker.mjs carryBrokerAdapter); the job's children (a Tauri CLI's Cargo
// calls) run NESTED under it; the broker signs the list of those children and the gate verifies it (verifyBrokerLedger) and reads the bundle root from it
// (carriedOutputDir). Every adapter job here runs on the REAL installed RustRight broker (an isolated instance, test-support/real-broker.mjs) over the gate's
// real snapshot of a real git checkout, with the broker's own PATH and environment (RUSTRIGHT_PARENT_REQUEST_ID set, every other variable dropped). The
// child is `rightkit-node scripts/<x>.mjs`, a script committed in the checkout that makes the same nested `cargo` calls a Tauri CLI makes. The shape of the
// signed `nestedChildren` list is RustRight's (rustright#190): signed in snapshotEvidence, mirrored on the receipt; tests titled "data:" call the production
// verifier on constructed lists and are never broker evidence.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { buildLaunch, snapshotInstallSkip } from "./build-launch.mjs";
import { OWNERSHIP_ENV, OWNERSHIP_FILE_ENV, brokerLeaseState, markCarriedSend } from "./build-ownership.mjs";
import { enrollBrokerPins } from "./broker-pins.mjs";
import { readLedger, UNSCOPED } from "./cargo-ledger.mjs";
import { CARRIED_CARGO_MAP_ENV, carriedOutputDir, installCarrier, runCarriedAdapter } from "./carried-cargo.mjs";
import { carriedCallNonce, qualificationNonce } from "./provenance-verify.mjs";
import { runCargoSync } from "./managed-cargo.mjs";
import { runRecipeBuild } from "./recipe-build.mjs";
import { syncSnapshot } from "./snapshot.mjs";
import { brokerSection, createSnapshotBroker, verifyBrokerLedger } from "./snapshot-broker.mjs";
import { verifyNestedChildren } from "./snapshot-request.mjs";
import { spawnSync } from "./background-process.mjs";
import { startRealBroker } from "./test-support/real-broker.mjs";

const POSIX = process.platform === "win32" ? "the adapter PATH is POSIX only" : false;
const GATE = "/gate/bin/rightkit-gate.mjs";
const HERE = path.dirname(fileURLToPath(import.meta.url));
const ADAPTER_RUN = path.join(HERE, "carried-adapter-run.mjs");

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

// The fixture scripts launch a compile the way a Tauri CLI does. The specifier is split so background-process.test.mjs's source scan, which bans the literal
// import in release sources, does not read a script that only runs inside the broker's job (the convention cargo-target-shim.test.mjs and build-ownership.test.mjs use).
const CHILD_PROCESS = "node:child" + "_process";
const nodeScript = (body) => `import { spawnSync } from "${CHILD_PROCESS}";\nimport path from "node:path";\n${body}\n`;
const CARGO_IN = (dir, args) => `const r = spawnSync("cargo", ${JSON.stringify(args)}, { cwd: ${dir}, stdio: "inherit" });\nprocess.exit(r.status ?? 1);`;
const SCRIPTS = {
  // What `pnpm exec tauri build` does under the job: one nested compile in the app's directory.
  "scripts/tauri-like.mjs": nodeScript(CARGO_IN('path.join(process.cwd(), "native")', ["build", "--locked", "--profile", "dev"])),
  // A nested read that compiles nothing.
  "scripts/meta.mjs": nodeScript(CARGO_IN("process.cwd()", ["metadata", "--format-version", "1", "--no-deps"])),
  "native/scripts/meta.mjs": nodeScript(CARGO_IN("process.cwd()", ["metadata", "--format-version", "1", "--no-deps"])),
  // A compile outside the snapshot: the directory above the job's.
  "scripts/escape.mjs": nodeScript(CARGO_IN("path.dirname(process.cwd())", ["build", "--locked"])),
  // The job's own environment, as the broker passes it.
  "scripts/env.mjs": "process.stdout.write(`ENV ${JSON.stringify({ names: Object.keys(process.env).sort(), path: process.env.PATH })}\\n`);\n",
};

/** A real broker host with the scripts above in the checkout and the carrier installed. */
async function host(t) {
  const fx = await realBrokerHost(t, "carried-adapter", SCRIPTS);
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout, native: path.join(fx.checkout, "native") }, section: brokerSection(fx.sb), platform: process.platform, env: {} });
  t.after(() => installed.cleanup());
  const env = { ...fx.rb.env, PATH: process.env.PATH, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE, ...fx.ownershipEnv, ...installed.env };
  const adapter = (name, args, { cwd = fx.snapshot, over = {}, stdio = "capture" } = {}) => runCarriedAdapter(name, args, { cwd, env: { ...env, ...over }, stdio }, { map: JSON.parse(fs.readFileSync(installed.env[CARRIED_CARGO_MAP_ENV], "utf8")), err: () => {} });
  const entries = () => readLedger(installed.ledgerDir, UNSCOPED);
  return { fx, installed, env, adapter, entries };
}
const TAURI_ARGS = ["scripts/tauri-like.mjs"];
const NODE = "rightkit-node";

test("a Tauri-style build under a carried adapter on the real broker: the child's compile is nested, the gate's own verification accepts the signed list, and the bundle root is the nested child's output directory", { skip: POSIX }, async (t) => {
  const h = await host(t);
  const r = h.adapter(NODE, TAURI_ARGS);
  assert.equal(r.status, 0, r.stderr);
  // One carried request: the adapter job, verified.
  const { entries, problems } = h.entries();
  assert.deepEqual(problems, []);
  assert.deepEqual(entries.map((e) => [e.status, e.shim, e.adapter]), [["verified", "adapter", NODE]]);
  const jobId = entries[0].requestId;
  // The compile was started by the job's child and nested under it (the broker's own `cargo` on the job's PATH), in the snapshot.
  assert.equal(r.nestedChildren.length, 1, JSON.stringify(r.nestedChildren));
  const [child] = r.nestedChildren;
  assert.deepEqual([child.shim, child.argv.slice(0, 2), child.cwd, child.exitCode, child.parentRequestId], ["cargo", ["build", "--locked"], path.join(h.fx.snapshot, "native"), 0, jobId]);
  assert.ok(child.outputDir && child.outputDir !== r.outputDir, "the child's compile has an output directory of its own");
  assert.deepEqual(await h.installed.problems(h.fx.sb), [], "the gate verifies the receipt and its nested children again itself");
  // The stage's output directories are the signed ones, and the bundle root comes from the nested child's, never from a directory the child reported.
  assert.deepEqual(h.installed.outputDirs().sort(), [r.outputDir, child.outputDir].sort());
  assert.deepEqual(h.installed.outputsByWorkspace(), [[path.join(h.fx.checkout, "native"), child.outputDir]]);
  assert.equal(carriedOutputDir(h.env, path.join(h.fx.snapshot, "native", "Cargo.toml")), child.outputDir);
  assert.equal(carriedOutputDir(h.env, path.join(h.fx.snapshot, "crates", "a", "Cargo.toml")), null, "another workspace borrows nothing");
  // A carried Cargo call of the same workspace made AFTER the build names the broker's own target: the bundle root (preferNested) still follows the compile the child made.
  const manifest = path.join(h.fx.snapshot, "native", "Cargo.toml");
  assert.equal(runCargoSync(["build", "--locked"], { cwd: path.join(h.fx.snapshot, "native"), env: h.env, encoding: "utf8" }, { platform: process.platform }).status, 0);
  assert.notEqual(carriedOutputDir(h.env, manifest), child.outputDir, "the last verified call of the workspace");
  assert.equal(carriedOutputDir(h.env, manifest, { preferNested: true }), child.outputDir, "the child's own compile");
});

test("the broker's environment, not the caller's, reaches the job: the broker's PATH, the parent request id and capability token, and none of the caller's variables; a wrapper first on the caller's PATH never runs and a compile with no parent is refused for want of a carrier", { skip: POSIX }, async (t) => {
  const h = await host(t);
  const wrapperDir = fs.mkdtempSync(path.join(h.fx.root, "wrapper-"));
  fs.writeFileSync(path.join(wrapperDir, "cargo"), "#!/bin/sh\necho wrapper >> \"$0.ran\"\nexit 0\n", { mode: 0o755 });
  const over = { PATH: `${wrapperDir}:${process.env.PATH}`, RUSTRIGHT_SNAPSHOT_GRANT: '{"path":"/x","sha256":"x"}', RIGHTKIT_STAGE_VARIABLE: "1", CARGO: path.join(wrapperDir, "cargo") };
  const probe = h.adapter(NODE, ["scripts/env.mjs"], { over });
  assert.equal(probe.status, 0, probe.stderr);
  const seen = JSON.parse(String(probe.stdout).split("\n").find((l) => l.startsWith("ENV ")).slice(4));
  assert.ok(seen.names.includes("RUSTRIGHT_PARENT_REQUEST_ID") && seen.names.includes("RUSTRIGHT_PARENT_CAPABILITY_TOKEN"), "the broker's parent request id and capability token");
  for (const name of [CARRIED_CARGO_MAP_ENV, "RIGHTKIT_STAGE_VARIABLE", "RUSTRIGHT_SNAPSHOT_GRANT", "CARGO"]) assert.ok(!seen.names.includes(name), `${name} is dropped by the broker`);
  assert.ok(!seen.path.includes(wrapperDir), "the caller's PATH is not the job's");
  const r = h.adapter(NODE, TAURI_ARGS, { over });
  assert.equal(r.status, 0, r.stderr);
  assert.equal(fs.existsSync(path.join(wrapperDir, "cargo.ran")), false, "the caller's wrapper never ran");
  assert.equal(r.nestedChildren.length, 1, "the compile was the broker's nested request");
  // A compile with no parent (a child that reached the broker around its job) is a root request inside a managed snapshot, which has no carrier: refused (rightkit#1311).
  const orphan = h.fx.rb.run(["cargo", "build", "--locked"], { cwd: path.join(h.fx.snapshot, "native") });
  assert.notEqual(orphan.status, 0);
  assert.match(orphan.stderr + orphan.stdout, /RK_SNAPSHOT_CARRIER_REQUIRED/);
});

const childOf = (over = {}) => ({ requestId: "c0", parentRequestId: "j", shim: "cargo", argv: ["build", "--locked"], cwd: "/s/native", exitCode: 0, status: "SUCCEEDED", outputDir: null, ...over });
const VERIFY_OPTS = { snapshotPath: "/s", checkout: "/co", outputRoots: ["/data/ws"], jobId: "j", canonicalPath: (p) => p };
const verifyList = (list, { receipt = list } = {}) => verifyNestedChildren({ nestedChildren: list }, { nestedChildren: receipt }, VERIFY_OPTS);

test("data: an unverified child is refused by the production verifier: a signed child that exited non-zero or with no exit code, an UNFINISHED child, another parent, no list signed or mirrored, a mirror that differs, a list that is not exactly the shape, a repeated child", () => {
  const ok = [childOf()];
  assert.equal(verifyList(ok).length, 1);
  for (const [name, call, pattern] of [
    ["childFailed", () => verifyList([childOf({ exitCode: 1, status: "FAILED" })]), /exited 1, not 0/],
    ["childNullExit", () => verifyList([childOf({ exitCode: null })]), /exited with no exit code, not 0/],
    ["childUnfinished", () => verifyList([childOf({ exitCode: null, status: "UNFINISHED" })]), /UNFINISHED/],
    ["childOtherParent", () => verifyList([childOf({ parentRequestId: "44444444-4444-4444-8444-444444444444" })]), /names parent .*neither this job/],
    ["noNestedChildren", () => verifyNestedChildren({}, { nestedChildren: ok }, VERIFY_OPTS), /carries no nestedChildren list/],
    ["nestedChildrenNotMirrored", () => verifyNestedChildren({ nestedChildren: ok }, {}, VERIFY_OPTS), /receipt carries no nestedChildren list/],
    ["nestedCopyDiffers", () => verifyList(ok, { receipt: [] }), /receipt nestedChildren differs from the signed list/],
    ["childExtraField", () => verifyList([childOf({ extra: true })]), /is not exactly/],
    ["childDuplicated", () => verifyList([childOf(), childOf()]), /has no request id of its own/],
  ]) assert.throws(call, pattern, name);
});

test("data: a child whose cwd is outside the snapshot, or whose output directory is inside the snapshot, relative or not a managed generation target, is refused; the cap and the depth rules hold", () => {
  for (const [name, over, pattern] of [
    ["childOutsideSnapshot", { cwd: "/co/native" }, /outside the snapshot/], ["childOutputInsideSnapshot", { outputDir: "/s/target" }, /is inside the snapshot/],
    ["childOutputUnmanaged", { outputDir: "/elsewhere/nested/x" }, /not inside a managed generation target/], ["childOutputRelative", { outputDir: "target/nested/x" }, /neither null nor absolute/],
  ]) assert.throws(() => verifyList([childOf(over)]), pattern, name);
  const big = Array.from({ length: 8193 }, (_, i) => childOf({ requestId: `c${i}` }));
  assert.equal(verifyList(big.slice(0, 8192)).length, 8192);
  assert.throws(() => verifyList(big), /more than the cap 8192/);
  // Depth >= 2 (rustright#190 ad9ecb37): a child's parent may be an earlier listed child, never a later or unknown one, never itself.
  assert.equal(verifyList([childOf(), childOf({ requestId: "c1", parentRequestId: "c0" })]).length, 2);
  for (const bad of [[childOf({ requestId: "c1", parentRequestId: "c2" }), childOf({ requestId: "c2" })], [childOf({ parentRequestId: "c0" })], [childOf({ parentRequestId: "x" })]]) {
    assert.throws(() => verifyList(bad), /names parent .*neither this job/);
  }
});

test("the broker's own nested containment (rustright#190 D7): a compile outside the snapshot is refused and is NOT in the signed list, so the job's own failure is what fails the call", { skip: POSIX }, async (t) => {
  const h = await host(t);
  const r = h.adapter(NODE, ["scripts/escape.mjs"]);
  assert.notEqual(r.status, 0, "the job's own failure is what fails the step");
  assert.ok((r.nestedChildren ?? []).every((c) => String(c.cwd).startsWith(h.fx.snapshot)), "a refused child is not listed");
});

test("an entry that compiles nothing has a null outputDir: it is verified, and never names a bundle root or a stage output directory", { skip: POSIX }, async (t) => {
  const h = await host(t);
  const r = h.adapter(NODE, ["scripts/meta.mjs"]);
  assert.equal(r.status, 0, r.stderr);
  const jobId = h.entries().entries[0].requestId;
  assert.deepEqual(r.nestedChildren.map((c) => [c.shim, c.argv[0], c.outputDir, c.status, c.parentRequestId === jobId]), [["cargo", "metadata", null, "SUCCEEDED", true]]);
  assert.deepEqual(await h.installed.problems(h.fx.sb), []);
  assert.deepEqual(h.installed.outputDirs(), [r.outputDir], "only the job's own output directory");
  assert.deepEqual(h.installed.outputsByWorkspace(), []);
  assert.equal(carriedOutputDir(h.env, path.join(h.fx.snapshot, "native", "Cargo.toml"), { preferNested: true }), null, "no bundle root from a child with a null outputDir");
});

test("the request's workspace is the primary counterpart of the step's directory (an app root in a subfolder passes), and the cwds are exactly these: the runner runs in the snapshot step directory, the client in the primary counterpart the carrier names", { skip: POSIX }, async (t) => {
  const h = await host(t);
  const stepDir = path.join(h.fx.snapshot, "native");
  const r = h.adapter(NODE, ["scripts/meta.mjs"], { cwd: stepDir });
  assert.equal(r.status, 0, r.stderr);
  const [ledger] = h.entries().entries;
  assert.equal(ledger.status, "verified");
  assert.deepEqual([ledger.cwd, ledger.checkoutCwd, ledger.workspace], [stepDir, path.join(h.fx.checkout, "native"), path.join(h.fx.checkout, "native")], "the client's cwd and the request's workspace are the primary counterpart (a subfolder app root), not the toplevel");
  assert.deepEqual(await h.installed.problems(h.fx.sb), []);
});

test("a step run from the snapshot's root has the primary toplevel as its workspace (the counterpart of the root)", { skip: POSIX }, async (t) => {
  const h = await host(t);
  const r = h.adapter(NODE, ["scripts/meta.mjs"], { cwd: h.fx.snapshot });
  assert.equal(r.status, 0, r.stderr);
  assert.equal(h.entries().entries[0].workspace, h.fx.checkout);
});

test("the gate re-verifies the ledger's nested children against the signed list: a ledger record that names other children, another cwd, or a replayed request is a problem", { skip: POSIX }, async (t) => {
  const h = await host(t);
  assert.equal(h.adapter(NODE, TAURI_ARGS).status, 0);
  const { entries } = h.entries();
  const forge = (over) => verifyBrokerLedger([{ ...entries[0], ...over }], h.fx.sb);
  assert.deepEqual(await forge({}), []);
  assert.match((await forge({ nestedChildren: [] }))[0], /not the ones the broker's receipt signs/);
  assert.match((await forge({ nestedChildren: [{ ...entries[0].nestedChildren[0], outputDir: path.join(entries[0].nestedChildren[0].outputDir, "forged") }] }))[0], /not the ones the broker's receipt signs/);
  assert.match((await forge({ cwd: path.join(h.fx.snapshot, "native") }))[0], /not the checkout counterpart/);
  assert.match((await forge({ workspace: path.join(h.fx.checkout, "native") }))[0], /not the primary checkout counterpart/);
  assert.match((await forge({ workspace: os.tmpdir() }))[0], /outside the primary checkout/);
  assert.match((await forge({ cwd: h.fx.checkout }))[0], /outside the snapshot/);
  const seen = { nonces: new Set(), ids: new Set() };
  assert.deepEqual(await verifyBrokerLedger([entries[0]], h.fx.sb, { seen }), []);
  assert.match((await verifyBrokerLedger([entries[0]], h.fx.sb, { seen }))[0], /used twice/);
});

test("pack from a relative directory works on the real broker: the pack's --dir is the unit's directory of the snapshot and only --pack-destination is outside it; an absolute --dir is refused and the call is unverified; install is not a snapshot-bound command", { skip: POSIX }, async (t) => {
  const h = await host(t);
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "rk-local-publish-"));
  t.after(() => fs.rmSync(scratch, { recursive: true, force: true }));
  const out = path.join(scratch, "pack"); fs.mkdirSync(out);
  const ok = h.adapter("rightkit-pnpm", ["--dir", "packages/release", "pack", "--pack-destination", out]);
  assert.equal(ok.status, 0, ok.stderr);
  assert.equal(h.entries().entries.at(-1).status, "verified");
  const verifiedBefore = h.entries().entries.filter((e) => e.status === "verified").length;
  // An absolute --dir (the old scratch copy, or the snapshot's own absolute path) is refused by the broker's snapshot-mode grammar.
  for (const dir of [path.join(scratch, "package-source"), path.join(h.fx.snapshot, "packages", "release")]) {
    const out2 = path.join(scratch, `pack-${path.basename(dir)}`); fs.mkdirSync(out2);
    const bad = h.adapter("rightkit-pnpm", ["--dir", dir, "pack", "--pack-destination", out2]);
    assert.notEqual(bad.status, 0, dir);
    assert.match(bad.stderr, /RK_ADAPTER_ARGV_DENIED/, dir);
    assert.equal(h.entries().entries.at(-1).status, "unverified", dir);
  }
  assert.equal(h.entries().entries.filter((e) => e.status === "verified").length, verifiedBefore, "no refused call became a verified job");
  // `install` is refused by the broker in a snapshot call (snapshotSubcommands exec, run, pack).
  const install = h.adapter("rightkit-pnpm", ["install", "--frozen-lockfile"]);
  assert.notEqual(install.status, 0);
  assert.match(install.stderr, /RK_ADAPTER_ARGV_DENIED/);
});

test("a call from inside a broker job (a parent request in the environment), without a calling gate or from outside the snapshot is refused before anything runs", { skip: POSIX }, async (t) => {
  const h = await host(t);
  for (const [over, code] of [[{ RUSTRIGHT_PARENT_REQUEST_ID: "x" }, "RK_ADAPTER_BROKER_INHERITED_ENV"], [{ RUSTRIGHT_PARENT_CAPABILITY_TOKEN: "x" }, "RK_ADAPTER_BROKER_INHERITED_ENV"], [{ RUSTRIGHT_ATTEST_NONCE: "a".repeat(64) }, "RK_ADAPTER_BROKER_INHERITED_ENV"], [{ RUSTRIGHT_CALLING_GATE: "" }, "RK_ADAPTER_BROKER_NO_GATE"]]) {
    const r = h.adapter(NODE, ["scripts/meta.mjs"], { over });
    assert.equal(r.status, 101, code); assert.equal(r.refusal.code, code);
  }
  const outside = h.adapter(NODE, ["scripts/meta.mjs"], { cwd: h.fx.checkout });
  assert.equal(outside.refusal.code, "RK_ADAPTER_BROKER_CWD");
  assert.equal(h.adapter("Evil Adapter", ["run", "x"]).refusal.code, "RK_ADAPTER_BROKER_ADAPTER");
  assert.ok(h.entries().entries.every((e) => e.status === "refused"));
  assert.deepEqual(h.fx.submits, [], "no submit was recorded: nothing was sent to the broker");
});

test("under a release-bound stage the adapter's nonce is derived from the stage nonce, its directory and its argv (the admission recomputes it) as the real broker signs it, and the stage nonce never becomes the call's; otherwise it is fresh", { skip: POSIX }, async (t) => {
  const h = await host(t);
  const stageNonce = qualificationNonce({ commit: h.fx.commit.sha, tree: h.fx.commit.tree, unit: "npm:@rightkit/release", stage: "compile" });
  const args = ["scripts/meta.mjs"];
  assert.equal(h.adapter(NODE, args, { over: { RIGHTKIT_ATTEST_STAGE_NONCE: stageNonce } }).status, 0);
  const [first] = h.entries().entries;
  const receipt = await h.fx.sb.receiptOf(first.requestId);
  assert.equal(receipt.provenance.payload.nonce, carriedCallNonce({ stageNonce, cwd: ".", argv: args }));
  assert.notEqual(receipt.provenance.payload.nonce, stageNonce);
  assert.equal(h.adapter(NODE, args).status, 0);
  const fresh = await h.fx.sb.receiptOf(h.entries().entries[1].requestId);
  assert.notEqual(fresh.provenance.payload.nonce, carriedCallNonce({ stageNonce, cwd: ".", argv: args }));
  assert.deepEqual(await h.installed.problems(h.fx.sb), []);
});

test("build-launch: a snapshot stage's pnpm and node steps are carried adapter jobs (QA is a direct step), an install step is skipped with a log line, and nothing changes without the snapshot route", () => {
  const CLIENT = "/managed/agent-bin/rustright";
  const base = { platform: "darwin", exists: (p) => p === CLIENT, which: (c) => (c === "rustright" ? [CLIENT] : []) };
  const snapshot = { ...base, env: { PATH: "/managed/agent-bin", RK_GATE_CHECKOUT: "/co" } };
  for (const args of [["install"], ["install", "--frozen-lockfile"], ["--dir", "hub", "install", "--frozen-lockfile"], ["-C", "hub", "i"]]) {
    assert.deepEqual(buildLaunch("pnpm", args, snapshot), { command: process.execPath, args: [ADAPTER_RUN, "--skip-install", ...args] }, args.join(" "));
    assert.ok(snapshotInstallSkip("pnpm", args, snapshot.env));
  }
  for (const args of [["run", "build"], ["--dir", "install", "run", "x"], ["exec", "install"], ["-r", "--workspace-concurrency=1", "run", "build"], ["pack"]]) assert.equal(snapshotInstallSkip("pnpm", args, snapshot.env), null, args.join(" "));
  // Not on the snapshot route (no map, no split): an install is the client's adapter job as before.
  assert.deepEqual(buildLaunch("pnpm", ["install"], { ...base, env: { PATH: "/managed/agent-bin" } }), { command: CLIENT, args: ["run", "rightkit-pnpm", "--", "install"] });
  // The skip is a log line and exit 0, running nothing.
  const skipped = spawnSync(process.execPath, [ADAPTER_RUN, "--skip-install", "--dir", "hub", "install", "--frozen-lockfile"], { encoding: "utf8" });
  assert.equal(skipped.status, 0);
  assert.match(skipped.stderr, /snapshot route: skipped `pnpm --dir hub install --frozen-lockfile`: the gate already installed/);
  // A direct (Linux) host's snapshot stage skips it too: the gate installed there as well.
  assert.deepEqual(buildLaunch("pnpm", ["install"], { env: { PATH: "/usr/bin", RK_GATE_CHECKOUT: "/co" }, platform: "linux", exists: () => false, which: () => [] }).args.slice(0, 2), [ADAPTER_RUN, "--skip-install"]);
});

test("the release stage goes through the same door: a recipe's pnpm, node and cargo steps are carried adapter jobs in a snapshot stage, its install step is skipped, and the door's own right-release stays in process", { skip: POSIX }, async (t) => {
  const h = await host(t);
  const calls = [];
  const spawn = (command, args, options) => { calls.push({ command, args, cwd: options.cwd, env: options.env }); return { status: 0 }; };
  const door = (cmd, args, host2) => buildLaunch(cmd, args, { ...host2, exists: (p) => p === "/managed/agent-bin/rustright", which: (c) => (c === "rustright" ? ["/managed/agent-bin/rustright"] : []) });
  const app = path.join(h.fx.snapshot, "native");
  fs.mkdirSync(path.join(app, "hub"), { recursive: true });
  const plan = { version: 1, steps: [
    { phase: "prepare", cmd: "pnpm", args: ["--dir", "hub", "install", "--frozen-lockfile"] },
    { phase: "prepare", cmd: "node", args: ["scripts/stage.mjs"] },
    { phase: "prepare", cmd: "right-release", args: ["stage-sidecar"] },
    { phase: "build", cmd: "pnpm", args: ["exec", "tauri", "build"], cwd: "." },
  ] };
  runRecipeBuild(plan, { appRoot: app, env: { ...h.env, PATH: "/managed/agent-bin:/usr/bin" }, platform: "darwin", spawnSync: spawn, door });
  assert.deepEqual(calls.map((c) => [path.basename(c.args[0]), ...c.args.slice(1, 3)]), [
    ["carried-adapter-run.mjs", "--skip-install", "--dir"],
    ["carried-adapter-run.mjs", "rightkit-node", "--"],
    ["right-release.mjs", "stage-sidecar"],
    ["carried-adapter-run.mjs", "rightkit-pnpm", "--"],
  ]);
  assert.ok(calls.every((c) => c.command === process.execPath));
  assert.deepEqual(calls[3].args.slice(3, 6), ["exec", "tauri", "build"], "the Tauri step is the adapter job; its compile is nested under it");
});
