// rightkit#1311: on a broker host the gate's Cargo reads inside the managed snapshot (unit discovery, workspace members) are carried read-only
// requests with a verified signed receipt, never root requests (RustRight refuses those: "a root request cannot run inside a managed snapshot").
// The fake broker (test-support/fake-broker.mjs) refuses a root request inside the snapshots directory the way the real one does and logs every
// uncarried request (`rootRequests()`).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { discoverRustWorkspaces, main } from "./bin/rightkit-gate.mjs";
import { carriedReadOnlyMetadata, snapshotCargoSpawn } from "./snapshot-metadata.mjs";
import { syncSnapshot } from "./snapshot.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const emptyConfig = path.join(fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-sm-cfg-"))), "gitconfig");
fs.writeFileSync(emptyConfig, "");
Object.assign(process.env, { GIT_CONFIG_GLOBAL: emptyConfig, GIT_CONFIG_NOSYSTEM: "1", GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@t", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@t" });

const CARGO = ["rustright", "cargo"];
const GATE = "/gate/bin/rightkit-gate.mjs";
const tracked = (name) => ["Cargo.toml", "crates/a/Cargo.toml", "native/Cargo.toml"].filter((f) => f === name || f.endsWith(`/${name}`));
const root = (cmd, args, opts) => spawnSync(cmd, args, { encoding: "utf8", ...opts });

function git(cwd, ...args) {
  const r = spawnSync("git", args, { cwd, encoding: "utf8" });
  if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr}`);
  return r.stdout.trim();
}

test("the shapes the broker admits as a carried read-only request: metadata --no-deps, or lock-pinned offline metadata; nothing else", () => {
  for (const ok of [["metadata", "--no-deps", "--format-version", "1", "--locked"], ["metadata", "--format-version", "1", "--no-deps"], ["metadata", "--frozen"], ["metadata", "--locked", "--offline"]]) assert.equal(carriedReadOnlyMetadata(ok), true, ok.join(" "));
  for (const no of [["metadata", "--locked"], ["metadata", "--offline"], ["metadata"], ["fetch", "--locked"], ["deny", "check"], ["audit"], ["metadata", "--no-deps", "--config", "x=y"], ["metadata", "--no-deps", "-Zfoo"], ["metadata", "--", "--no-deps"], ["build", "--locked"]]) assert.equal(carriedReadOnlyMetadata(no), false, no.join(" "));
});

test("[1311] discovery inside the snapshot is carried: units are found, no root request reaches the broker, each call has a verified receipt", (t) => {
  const fake = makeFakeBroker(t, { readOnlySnapshots: true });
  const records = [];
  const spawn = snapshotCargoSpawn({ spawn: root, cargoCommand: CARGO, snapshotRoot: fake.snapshot, broker: () => fake.sb, env: { ...process.env, RUSTRIGHT_CALLING_GATE: GATE }, records });
  // The fake's own rule, as the real broker's: a root request inside the snapshot is refused.
  const plain = root(fake.client, ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"], { cwd: fake.snapshot, env: process.env });
  assert.equal(plain.status, 2); assert.match(plain.stderr, /a root request cannot run inside a managed snapshot/);
  fake.setBehavior({});
  const before = fake.rootRequests().length;
  const found = discoverRustWorkspaces({ root: fake.snapshot, spawn, cargoCommand: CARGO, tracked, readFile: (f) => fs.readFileSync(f, "utf8") });
  assert.ok(found.workspaces.some((w) => w.cargoDir === "."), JSON.stringify(found.workspaces.map((w) => w.cargoDir)));
  assert.ok(found.units.some((u) => u.crate === "a"), "the member crate is discovered from the carried metadata");
  assert.equal(fake.rootRequests().length, before, "the discovery made no uncarried request");
  assert.ok(fake.calls().length >= 1 && fake.calls().every((c) => c.argv[0] === "metadata" && c.argv.includes("--no-deps")));
  assert.ok(fake.calls().every((c) => c.cwd === fs.realpathSync(fake.checkout) || c.cwd.startsWith(`${fs.realpathSync(fake.checkout)}${path.sep}`)), "each call ran in the primary checkout's counterpart of the snapshot directory");
  // Each call is recorded pending, then verified (the receipt the gate checked against its pins, with the broker's request id).
  const settled = records.filter((r) => r.status !== "pending");
  assert.ok(settled.length === fake.calls().length && settled.every((r) => r.status === "verified" && typeof r.requestId === "string"), JSON.stringify(records.map((r) => r.status)));
  assert.ok(fake.submits.length >= 1, "the ownership recorded the broker submit");
});

test("[1311] a carried request without a valid receipt, or a call the broker would not admit, fails discovery and never becomes a root request", (t) => {
  const fake = makeFakeBroker(t, { readOnlySnapshots: true });
  const errors = [];
  const spawn = snapshotCargoSpawn({ spawn: root, cargoCommand: CARGO, snapshotRoot: fake.snapshot, broker: () => fake.sb, env: { ...process.env, RUSTRIGHT_CALLING_GATE: GATE }, err: (m) => errors.push(m) });
  const discover = () => discoverRustWorkspaces({ root: fake.snapshot, spawn, cargoCommand: CARGO, tracked, readFile: (f) => fs.readFileSync(f, "utf8") });
  for (const tamper of ["signedByOtherKey", "noProvenance", "treeAtExit", "otherHandle", "otherNonce", "requestOtherArgv"]) {
    fake.setBehavior({ tamper });
    assert.throws(discover, /cargo metadata failed/, tamper);
  }
  assert.ok(errors.some((m) => /RK_CARGO_BROKER_UNVERIFIED/.test(m)));
  fake.setBehavior({ refuse: true });
  assert.throws(discover, /cargo metadata failed/);
  fake.setBehavior({});
  // Not admitted by the broker inside a snapshot (fetch, deny, audit, an unpinned metadata): refused here, before anything is sent.
  const calls = fake.calls().length;
  for (const argv of [["fetch", "--locked"], ["deny", "check"], ["metadata", "--locked"]]) {
    const r = spawn("rustright", [...CARGO.slice(1), ...argv], { cwd: fake.snapshot });
    assert.notEqual(r.status, 0); assert.match(r.stderr, /RK_SNAPSHOT_CARRIED_REFUSED/);
  }
  assert.equal(fake.calls().length, calls, "nothing was sent to the broker");
  assert.deepEqual(fake.rootRequests().filter((r) => r.refused), [], "and no root request was made inside the snapshot");
  // No gate named: the carrier is never forged.
  const noGate = snapshotCargoSpawn({ spawn: root, cargoCommand: CARGO, snapshotRoot: fake.snapshot, broker: () => fake.sb, env: { ...process.env, RUSTRIGHT_CALLING_GATE: "" } });
  assert.notEqual(noGate("rustright", [...CARGO.slice(1), "metadata", "--no-deps"], { cwd: fake.snapshot }).status, 0);
});

test("calls outside the snapshot (the primary checkout, git) pass through unchanged", (t) => {
  const fake = makeFakeBroker(t, { readOnlySnapshots: true });
  const seen = [];
  const spawn = snapshotCargoSpawn({ spawn: (c, a, o) => { seen.push([c, o.cwd]); return { status: 0, stdout: "x" }; }, cargoCommand: CARGO, snapshotRoot: fake.snapshot, broker: () => { throw new Error("no broker needed"); }, env: {} });
  assert.equal(spawn("rustright", ["cargo", "metadata", "--no-deps"], { cwd: fake.checkout }).stdout, "x");
  assert.equal(spawn("git", ["status"], { cwd: fake.snapshot }).stdout, "x");
  assert.equal(seen.length, 2);
});

/** A Cargo repository with a committed workspace, a gate-owned snapshot of it, and a fixture broker that serves both. */
function scenario(t) {
  const repo = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-sm-repo-")));
  const snaps = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-sm-snaps-")));
  t.after(() => { fs.rmSync(repo, { recursive: true, force: true }); fs.rmSync(snaps, { recursive: true, force: true }); });
  git(repo, "init", "-q", "-b", "main");
  const files = { "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "Cargo.lock": "version = 4\n", "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n', "crates/a/src/lib.rs": "\n", "README.md": "r\n" };
  for (const [f, body] of Object.entries(files)) { fs.mkdirSync(path.dirname(path.join(repo, f)), { recursive: true }); fs.writeFileSync(path.join(repo, f), body); }
  git(repo, "add", "-A"); git(repo, "commit", "-q", "-m", "A");
  const sha = git(repo, "rev-parse", "HEAD");
  const snap = syncSnapshot({ checkout: repo, rev: sha, root: snaps });
  const fake = makeFakeBroker(t, { readOnlySnapshots: true });
  fake.rebase({ checkout: repo, snapshotsRoot: path.dirname(snap.path) });
  const sb = { ...fake.sb, checkout: repo, markBrokerSubmit: () => ({ handle: fake.sb.handle, broker: { submitted: true } }), get snapshot() { return { path: snap.path, expectedTree: fake.sb.snapshot.expectedTree }; } };
  return { repo, sha, snap, fake, sb };
}

function gateIo(t, s, { runScans }) {
  const out = []; const err = [];
  const gatePackage = fs.mkdtempSync(path.join(os.tmpdir(), "rk-sm-gatepkg-"));
  t.after(() => fs.rmSync(gatePackage, { recursive: true, force: true }));
  fs.writeFileSync(path.join(gatePackage, "rightkit-gate.stub.mjs"), "// stand-in gate package\n");
  const env = { ...process.env, RK_GATE_CACHE: "0", RUSTRIGHT_CALLING_GATE: GATE, PATH: `${s.fake.root}${path.delimiter}${process.env.PATH}` };
  return {
    out, err,
    io: {
      cwd: s.repo, gatePackage, env, snapshotRoot: path.join(fs.mkdtempSync(path.join(os.tmpdir(), "rk-sm-own-")), "snapshots"), out: (m) => out.push(m), err: (m) => err.push(m),
      appCheck: async () => 0, selfPath: path.join(gatePackage, "rightkit-gate.mjs"), platform: "darwin",
      resolveExecutor: () => ({ kind: "broker", cargo: CARGO }), snapshotBroker: s.sb, runScans,
      // `rustright` is the fixture broker's client (the guard stub would answer a checkout metadata call on its own).
      spawn: (cmd, args, opts = {}) => spawnSync(cmd === "rustright" ? s.fake.client : cmd, args, { encoding: "utf8", ...opts, env: opts.env ?? env }),
    },
  };
}

test("[1311] the check stage of the snapshot route (class A of the snapshot) makes zero root requests inside the snapshot: discovery and the member comparison are carried", async (t) => {
  const s = scenario(t);
  const plans = [];
  const run = gateIo(t, s, { runScans: async (plan) => { plans.push(plan); return { schema: "rightkit.deterministic-scans/1", ok: true, complete: true, classes: ["A"], scans: [], toolVersions: {}, cacheHits: [], notes: [], deferred: [] }; } });
  const code = await main(["check", "--commit", s.sha, "--full-a", "--snapshot", s.snap.path], run.io);
  assert.equal(code, 0, run.err.join("\n"));
  assert.ok(plans.length >= 1, "the scans were planned from the discovered units");
  assert.deepEqual(s.fake.rootRequests().filter((r) => r.refused), [], "no root request inside the snapshot");
  assert.ok(s.fake.calls().length >= 2, "discovery (and the snapshot side of the member comparison) were carried requests");
  assert.ok(s.fake.calls().every((c) => c.argv[0] === "metadata" && c.cwd.startsWith(s.repo)), "carried, read-only, from the primary checkout's counterpart");
  // A carried request whose receipt does not verify blocks the stage with the discovery message, not a root fallback.
  s.fake.setBehavior({ tamper: "signedByOtherKey" });
  const bad = gateIo(t, s, { runScans: async () => ({ ok: true, scans: [] }) });
  assert.notEqual(await main(["check", "--commit", s.sha, "--full-a", "--snapshot", s.snap.path], bad.io), 0);
  assert.match(bad.err.join("\n"), /cannot discover the repository's units: cargo metadata failed/);
  assert.deepEqual(s.fake.rootRequests().filter((r) => r.refused), []);
});
