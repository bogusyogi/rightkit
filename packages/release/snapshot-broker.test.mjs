// Item 7 rows 7.10, 7.20 and 7.24 (decision C) on a broker host: cargo reached by tests goes through the runtime wrapper's broker
// mode, one carried request per call, every call in the gate-owned ledger, and the scan fails on any refused or unverified call
// even when the test ignores cargo's exit status.
//
// Evidence labels. There is no simulated broker in this file. "real broker:" titles run the installed RustRight client against an isolated instance of the installed
// broker (test-support/real-broker.mjs) and assert on what it answers. "data:" titles call the real production functions with constructed inputs (a recording spawn,
// argv, wire-shaped lease answers, plain directories) and are NOT broker evidence. A call here never reaches a snapshot the broker holds (the snapshot is a plain
// directory under the instance's data root, not one the production sync made), so the real broker never answers a verified result: nothing below claims one.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";
import { SCAN_TOKEN_ENV, appendLedger, readLedger, scanTokenOf, unclaimedLedgers } from "./cargo-ledger.mjs";
import { decideCargoInvocation, installCargoShim, locateWorkspaceByWalk, shimScanEnv } from "./cargo-target-shim.mjs";
import { runScans } from "./deterministic-scans.mjs";
import { OWNERSHIP_ENV, OWNERSHIP_FILE_ENV, OWNERSHIP_SCHEMA, OwnershipBusy, acquireBuildOwnership, brokerLeaseState } from "./build-ownership.mjs";
import { brokerSection, carryBrokerAdapter, carryBrokerCargo, createSnapshotBroker, lexicalCounterpart, nearestExistingDirectory, receiptSync } from "./snapshot-broker.mjs";
import { carriedCallNonce } from "./provenance-verify.mjs";
import { isCallHandleOf, writeSnapshotRequest } from "./snapshot-request.mjs";
import { startRealBroker } from "./test-support/real-broker.mjs";
import { main } from "./bin/rightkit-gate.mjs";
import { enrollBrokerPins } from "./broker-pins.mjs";

const POSIX = process.platform === "win32" ? "the runtime wrapper is POSIX only: a Windows child finds only an .exe on PATH; the Dell refuses by the static walk" : false;
const GATE = "/gate/bin/rightkit-gate.mjs";
const COMMIT = { sha: "c".repeat(40), tree: "e".repeat(40) };
/** The gate's pins for this host: a real P-256 public key (so enrollment accepts them) with the matching key id. Nothing in this file judges a receipt against them. */
const PINS = (() => {
  const { publicKey } = crypto.generateKeyPairSync("ec", { namedCurve: "P-256" });
  const jwk = publicKey.export({ format: "jwk" });
  const sec1 = Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x, "base64url"), Buffer.from(jwk.y, "base64url")]).toString("hex");
  return { host: "host-1", keyId: crypto.createHash("sha256").update(Buffer.from(sec1, "hex")).digest("hex"), publicKey: sec1, brokerDigest: "b".repeat(64) };
})();
const WORKSPACES = {
  "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n', "crates/a/src/lib.rs": "\n",
  "native/Cargo.toml": '[workspace]\nmembers = []\n[package]\nname = "b"\nversion = "0.1.0"\n', "native/src/lib.rs": "\n",
};
const writeTree = (root, files) => { for (const [rel, body] of Object.entries(files)) { const f = path.join(root, rel); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, body); } };
const NOW = Date.now();
const probe = () => ({ platform: "darwin", identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW - 60_000, group: 4242 } : { alive: false, startedMs: null, group: null }), bootTime: () => NOW - 86_400_000, groupSnapshot: () => ({ members: [], byPid: new Map([[1, { pid: 1, ppid: 0, startedMs: NOW - 9e7 }]]), own: new Set([process.pid]) }), tokenHolders: () => null, snapshotUsers: () => null, ancestors: () => [{ pid: 5000, startedMs: NOW - 9e6 }] });
const noLease = () => { throw new Error("no lease query expected"); };
const tmpRoot = (t, prefix = "rk-sb-") => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix))); t.after(() => fs.rmSync(d, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 })); return d; };

/** Plain checkout and snapshot directories (two Cargo workspaces, the root and `native`). */
function hostDirs(t) {
  const root = tmpRoot(t);
  const checkout = path.join(root, "checkout"); const snapshot = path.join(root, "data", "snapshots", "0123456789abcdef");
  writeTree(checkout, WORKSPACES); writeTree(snapshot, WORKSPACES);
  return { root, checkout, snapshot };
}

/**
 * A broker host with the broker-mode wrapper installed, the REAL build ownership and a runner for `node -e` scans that reach cargo through it. With `rb` (an isolated real broker) the
 * client, the carrier root and the lease reader are the installed broker's; without it the client is a file that is never started (a test that sends passes its own `spawn`).
 */
function host(t, { inputs = null, rb = null } = {}) {
  const root = tmpRoot(t);
  const dataRoot = rb?.dataRoot ?? path.join(root, "data"); const controlRoot = rb?.controlRoot ?? path.join(root, "control");
  const checkout = path.join(root, "checkout"); const snapshot = path.join(dataRoot, "snapshots", "0123456789abcdef"); const wsRoot = path.join(dataRoot, "rightkit-build-control", "ws");
  writeTree(checkout, WORKSPACES); writeTree(snapshot, WORKSPACES);
  if (inputs) writeTree(snapshot, { ".rightkit/compile-inputs.json": JSON.stringify({ inputs }), "gen/in.txt": "declared input\n" });
  const own = acquireBuildOwnership({ file: path.join(root, "build-ownership", "owner.json"), platform: "darwin", probe: probe(), lease: noLease, env: {}, exportEnv: false, stage: "compile", snapshot });
  const marks = []; const carriers = [];
  const readCarriers = () => { const dir = path.join(controlRoot, "snapshot-requests"); try { return fs.readdirSync(dir).filter((n) => n.endsWith(".json") && !n.endsWith(".owner.json")).map((n) => JSON.parse(fs.readFileSync(path.join(dir, n), "utf8"))); } catch { return []; } };
  const client = rb?.clientPath ?? process.execPath;
  const sb = {
    controlRoot, commit: COMMIT, snapshot: { path: snapshot, expectedTree: COMMIT.tree }, declaredInputs: inputs ?? [], handle: own.handle, pins: PINS,
    markBrokerSubmit: (callHandle) => { marks.push(callHandle); carriers.push(...readCarriers()); return own.markBrokerSubmit(callHandle); }, markBrokerUnsent: own.markBrokerUnsent,
    checkout, outputRoots: [wsRoot], platform: process.platform, client,
    ...(rb ? { leaseStatus: ({ handle, waitMs = 0 }) => brokerLeaseState({ snapshot, handle, env: rb.env, platform: process.platform, client: rb.clientPath, waitMs }) } : {}),
  };
  const ownershipEnv = { [OWNERSHIP_ENV]: own.handle, [OWNERSHIP_FILE_ENV]: own.file };
  const fx = { root, checkout, dataRoot, controlRoot, snapshot, wsRoot, handle: own.handle, pins: PINS, sb, own, marks, carriers, ownershipEnv, owner: () => JSON.parse(fs.readFileSync(own.file, "utf8")), sent: () => (JSON.parse(fs.readFileSync(own.file, "utf8")).broker?.handles ?? []).length, requestsAtBroker: () => (rb && fs.existsSync(rb.receiptsDir) ? fs.readdirSync(rb.receiptsDir).length : 0) };
  const installed = installCargoShim({
    base: path.join(root, "shim"), snapshotRoot: snapshot, realCargo: null,
    broker: { checkoutDirs: { ".": checkout, native: path.join(checkout, "native") }, section: brokerSection(sb) },
  });
  t.after(() => fs.rmSync(installed.dir, { recursive: true, force: true }));
  const clean = { ...process.env, NODE_OPTIONS: "", RUSTRIGHT_CALLING_GATE: GATE, ...(rb?.env ?? {}), ...ownershipEnv };
  for (const k of ["CARGO_TARGET_DIR", "CARGO", "NODE_TEST_CONTEXT", SCAN_TOKEN_ENV]) delete clean[k];
  const scanEnv = shimScanEnv(clean, installed);
  const cargo = (cwd, args) => `spawnSync("cargo", ${JSON.stringify(args)}, { cwd: ${JSON.stringify(cwd)}, stdio: "inherit" }).status`;
  const scan = (id, body, extra = {}) => ({ id, class: "A", tool: "node", args: ["-e", `const { spawnSync } = require("node:child" + "_process"); const fs = require("node:fs"); ${body}`], env: {}, root: "content", command: id, ...extra });
  const run = (scans, over = {}) => {
    const plan = { scans, roots: { contentRoot: snapshot, liveRoot: snapshot }, notes: [], deferred: [], inputDigest: "d", classDigests: {} };
    return runScans(plan, { classes: ["A"], env: scanEnv, cargoSentinel: installed.sentinel, cargoLedger: { dir: installed.ledgerDir }, snapshotBroker: sb, platform: process.platform, ...over });
  };
  const row = (result, id) => result.scans.find((r) => r.id === id);
  /** One carried call in this process (the section holds the real ownership hooks); `spawn` is the OS seam of a data test. */
  const carry = (words, { cwd = path.join(snapshot, "crates", "a"), env = { PATH: process.env.PATH, RUSTRIGHT_CALLING_GATE: GATE, ...(rb?.env ?? {}) }, records = [], ...extra } = {}) => carryBrokerCargo({ words, cwd, env, section: brokerSection(sb), workspace: snapshot, checkoutWorkspace: checkout, record: (e) => records.push(e), err: () => {}, stdio: "capture", ...extra });
  return { fx, installed, scanEnv, cargo, scan, run, row, carry, a: path.join(snapshot, "crates", "a"), native: path.join(snapshot, "native"), ledger: (id) => readLedger(installed.ledgerDir, scanTokenOf(id)) };
}
const recording = (reply = { status: 0, stdout: "", stderr: "" }) => { const calls = []; const spawn = (file, args, options) => { calls.push({ file, args, options }); return reply; }; spawn.calls = calls; return spawn; };
const carrierLeft = (fx) => { try { return fs.readdirSync(path.join(fx.controlRoot, "snapshot-requests")); } catch { return []; } };


test("real broker: [7.24] a node unit spawning cargo in two workspaces sends two carried requests to the real broker, each from the checkout cwd of the gate's map with a fresh nonce and its own sub-handle; no carrier is left behind and the snapshot holds no target/; a snapshot the real broker does not hold is never verified, so the scan fails RK_CARGO_LEDGER", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "sb-two" });
  t.after(() => rb.stop());
  const h = host(t, { rb });
  const result = await h.run([h.scan("two", `${h.cargo(h.a, ["build", "--locked"])}; ${h.cargo(h.native, ["test", "--locked", "-p", "b"])}; process.exit(0)`)]);
  const r = h.row(result, "two");
  assert.equal(r.status, "fail", JSON.stringify(r)); assert.equal(r.code, "RK_CARGO_LEDGER");
  const { entries } = h.ledger("two");
  assert.deepEqual(entries.map((e) => e.argv), [["build", "--locked"], ["test", "--locked", "-p", "b"]]);
  assert.ok(entries.every((e) => e.status !== "verified"), "the real broker holds no such snapshot: nothing is verified");
  // primaryCwd comes only from the gate's snapshot-to-checkout map: the client ran in the checkout, never in the snapshot.
  assert.deepEqual(entries.map((e) => e.checkoutCwd), [path.join(h.fx.checkout, "crates", "a"), path.join(h.fx.checkout, "native")]);
  assert.deepEqual(entries.map((e) => e.spawnCwd), entries.map((e) => e.checkoutCwd));
  assert.equal(new Set(entries.map((e) => e.nonce)).size, 2, "a fresh nonce per call");
  assert.equal(new Set(entries.map((e) => e.ownershipHandle)).size, 2, "and its own lease sub-handle");
  for (const e of entries) assert.equal(isCallHandleOf(e.ownershipHandle, h.fx.handle), true, e.ownershipHandle);
  assert.deepEqual(h.fx.owner().broker.handles, entries.map((e) => e.ownershipHandle), "the real ownership recorded each sub-handle before its send");
  assert.deepEqual(carrierLeft(h.fx), [], "no carrier is left behind");
  assert.equal(fs.existsSync(path.join(h.fx.snapshot, "target")), false, "the snapshot holds no target/");
});

test("data: [7.20] the declared inputs of a carried request are the snapshot's manifest, never the checkout's; a malformed manifest is refused before the request", { skip: POSIX }, (t) => {
  const h = host(t, { inputs: ["gen/in.txt"] });
  fs.mkdirSync(path.join(h.fx.checkout, ".rightkit"), { recursive: true });
  fs.writeFileSync(path.join(h.fx.checkout, ".rightkit", "compile-inputs.json"), JSON.stringify({ inputs: ["Cargo.toml"] }));
  const spawn = recording();
  h.carry(["check", "--locked"], { spawn });
  const [carrier] = h.fx.carriers;
  assert.deepEqual(carrier.declaredInputs, { inputs: ["gen/in.txt"] }, "the request carries the snapshot's declared inputs");
  assert.equal(isCallHandleOf(carrier.ownership.handle, h.fx.handle), true, "the request names a sub-handle of the gate's ownership"); assert.deepEqual(Object.keys(carrier.ownership), ["handle"]); assert.deepEqual(carrier.commit, h.fx.sb.commit);
  assert.deepEqual(carrier.snapshot, h.fx.sb.snapshot); assert.equal(carrier.primaryCwd, path.join(h.fx.checkout, "crates", "a"));
  assert.equal(spawn.calls.length, 1);
  // A malformed declared-inputs manifest is refused before any request is made.
  fs.writeFileSync(path.join(h.fx.snapshot, ".rightkit", "compile-inputs.json"), "{ not json");
  const marks = h.fx.marks.length;
  const bad = h.carry(["check", "--locked"], { spawn });
  assert.equal(bad.outcome.code, "RK_CARGO_BROKER_DECLARED_INPUTS");
  assert.equal(h.fx.marks.length, marks); assert.equal(spawn.calls.length, 1, "nothing was launched");
  assert.deepEqual(carrierLeft(h.fx), []);
});

test("data: [7.10] a refused wrapper call fails its unit even when the test swallows cargo's exit status; the unit that does not reach cargo passes; no refused call reaches the client", { skip: POSIX }, async (t) => {
  const h = host(t);
  fs.mkdirSync(path.join(h.fx.root, "outside"), { recursive: true }); fs.writeFileSync(path.join(h.fx.root, "outside", "Cargo.toml"), "[workspace]\n");
  const result = await h.run([
    h.scan("swallows-unknown", `${h.cargo(path.join(h.fx.root, "outside"), ["build"])}; process.exit(0)`),
    h.scan("swallows-redirect", `${h.cargo(h.a, ["build", "--target-dir", "/tmp/x"])}; process.exit(0)`),
    h.scan("swallows-unbound-command", `${h.cargo(h.a, ["clean"])}; process.exit(0)`),
    h.scan("quiet", "process.exit(0)"),
  ]);
  for (const id of ["swallows-unknown", "swallows-redirect", "swallows-unbound-command"]) {
    const r = h.row(result, id);
    assert.equal(r.status, "fail", `${id}: ${JSON.stringify(r)}`); assert.equal(r.code, "RK_CARGO_LEDGER", id);
    assert.match(r.detail, /did not complete as a verified success/);
    assert.ok(h.ledger(id).entries.every((e) => e.status === "refused"), `${id}: the wrapper refused it`);
  }
  assert.match(h.row(result, "swallows-unknown").detail, /RK_CARGO_UNBOUND_WORKSPACE/);
  assert.match(h.row(result, "swallows-redirect").detail, /RK_CARGO_REDIRECT/);
  assert.match(h.row(result, "swallows-unbound-command").detail, /RK_CARGO_UNBOUND_COMMAND/);
  assert.equal(h.row(result, "quiet").status, "pass");
  assert.equal(h.fx.sent(), 0, "no refused call was ever marked or sent");
});

test("real broker: [7.24] one completion per (snapshot, lease handle): N carried requests in one stage go out under distinct sub-handles; the real broker never completes a reused sub-handle for a snapshot it does not hold; the bare handle's lease status is UNKNOWN with no members, never REAPED", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "sb-handles" });
  t.after(() => rb.stop());
  const h = host(t, { rb });
  const result = await h.run([h.scan("many", `${[0, 1, 2, 3].map((i) => h.cargo(i % 2 ? h.native : h.a, ["build", "--locked", ...(i > 1 ? ["-q"] : [])])).join("; ")}; process.exit(0)`)]);
  const r = h.row(result, "many");
  assert.equal(r.status, "fail", JSON.stringify(r)); assert.equal(r.code, "RK_CARGO_LEDGER");
  const { entries } = h.ledger("many");
  assert.equal(entries.length, 4);
  const handles = entries.map((e) => e.ownershipHandle);
  assert.equal(new Set(handles).size, 4, "four calls, four sub-handles");
  assert.ok(handles.every((x) => isCallHandleOf(x, h.fx.handle)));
  // Two carried requests under one sub-handle, sent to the real client by hand: neither can succeed against a snapshot the real broker does not hold.
  const sendTwice = () => {
    const cwd = h.fx.checkout; const argv = ["build", "--locked"];
    return [1, 2].map(() => {
      const made = writeSnapshotRequest({ controlRoot: h.fx.controlRoot, commit: h.fx.sb.commit, snapshot: h.fx.sb.snapshot, declaredInputs: [], handle: h.fx.handle, callHandle: `${h.fx.handle}#reused`, primaryCwd: cwd, argv, requestIdFile: path.join(h.fx.root, "ref.json") });
      const res = rb.run(["cargo", ...argv], { cwd, env: { ...made.env, RUSTRIGHT_CALLING_GATE: GATE } });
      fs.rmSync(made.file, { force: true });
      return res;
    });
  };
  const [first, second] = sendTwice();
  assert.notEqual(first.status, 0, "the real broker holds no such snapshot");
  assert.notEqual(second.status, 0, "a reused sub-handle never completes");
  // `snapshot-lease status --ownership <bare>` for a handle the real broker never completed anything under: UNKNOWN, no members (GB-23: never REAPED without a completed member).
  const status = JSON.parse(rb.run(["snapshot-lease", "status", "--snapshot", h.fx.snapshot, "--ownership", `${h.fx.handle}-never-held`]).stdout);
  assert.equal(status.schema, "rustright.snapshot-lease-status.v1");
  assert.equal(status.state, "UNKNOWN"); assert.deepEqual(status.members, []);
});

test("real broker: [7.24] sub-handles are per invocation: two sequential calls on the same workspace and two independently launched callers draw different handles, each a random UUID under the gate's handle", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "sb-uuid" });
  t.after(() => rb.stop());
  const h = host(t, { rb });
  const same = h.row(await h.run([h.scan("same-ws", `${h.cargo(h.a, ["build", "--locked"])}; ${h.cargo(h.a, ["test", "--locked"])}; process.exit(0)`)]), "same-ws");
  assert.equal(same.code, "RK_CARGO_LEDGER", JSON.stringify(same));
  const entries = h.ledger("same-ws").entries;
  assert.equal(entries.length, 2);
  assert.notEqual(entries[0].ownershipHandle, entries[1].ownershipHandle);
  for (const e of entries) assert.match(e.ownershipHandle, new RegExp(`^${h.fx.handle}#[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$`), "a random UUID, never an index");
  // Two independently launched processes (wrapper children) never collide: each draws its own id.
  const draw = () => spawnSync(process.execPath, ["--input-type=module", "-e", `import { nextCallHandle } from ${JSON.stringify(new URL("./snapshot-request.mjs", import.meta.url).href)}; process.stdout.write(nextCallHandle(${JSON.stringify(h.fx.handle)}))`], { encoding: "utf8" }).stdout;
  const [x, y] = [draw(), draw()];
  assert.ok(isCallHandleOf(x, h.fx.handle) && isCallHandleOf(y, h.fx.handle)); assert.notEqual(x, y);
});

test("real broker: [7.24] negatives: an unverified result, no calling gate and a call the checkout cannot host each fail the scan; nothing launches when the request cannot be built", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "sb-neg" });
  t.after(() => rb.stop());
  const h = host(t, { inputs: ["gen/in.txt"], rb });
  const swallow = (cwd = h.a, args = ["build", "--locked"]) => `${h.cargo(cwd, args)}; process.exit(0)`;
  // The real broker does not hold this snapshot: whatever it answers, the call is unverified and the scan fails, never passed.
  let r = h.row(await h.run([h.scan("unheld", swallow())]), "unheld");
  assert.equal(r.status, "fail"); assert.equal(r.code, "RK_CARGO_LEDGER"); assert.match(r.detail, /RK_CARGO_BROKER_UNVERIFIED|did not complete as a verified success|no request reference/);
  assert.notEqual(h.ledger("unheld").entries.at(-1).status, "verified");
  const before = h.fx.sent();
  // No calling gate in the environment: the wrapper never forges one.
  r = h.row(await h.run([h.scan("no-gate", `${h.cargo(h.a, ["build", "--locked"])} ; process.exit(0)`, { env: {} })], { env: { ...h.scanEnv, RUSTRIGHT_CALLING_GATE: "" } }), "no-gate");
  assert.equal(r.code, "RK_CARGO_LEDGER"); assert.match(r.detail, /RK_CARGO_BROKER_NO_GATE/);
  // --manifest-path (the client refuses to rewrite the invocation) and a working directory the checkout does not have.
  fs.mkdirSync(path.join(h.a, "src", "only-in-snapshot"), { recursive: true });
  r = h.row(await h.run([h.scan("manifest", swallow(h.a, ["build", "--manifest-path", "Cargo.toml", "--locked"]))]), "manifest");
  assert.match(r.detail, /RK_CARGO_BROKER_MANIFEST_PATH/);
  assert.equal(h.fx.sent(), before, "neither refusal was sent");
  // A directory the snapshot has and the checkout does not is carried (GB-16: the client starts from the nearest existing ancestor and the carrier names the lexical counterpart).
  r = h.row(await h.run([h.scan("cwd", swallow(path.join(h.a, "src", "only-in-snapshot")))]), "cwd");
  assert.doesNotMatch(String(r.detail ?? ""), /COUNTERPART_ABSENT/);
  const afterCarried = h.fx.sent();
  assert.equal(afterCarried, before + 1, "that one call was sent; the refusals around it were not");
  // A malformed declared-inputs manifest is refused before any request is made.
  fs.writeFileSync(path.join(h.fx.snapshot, ".rightkit", "compile-inputs.json"), "{ not json");
  r = h.row(await h.run([h.scan("manifest-bad", swallow())]), "manifest-bad");
  assert.match(r.detail, /RK_CARGO_BROKER_DECLARED_INPUTS/);
  assert.equal(h.fx.sent(), afterCarried, "none of the refused calls was launched at the broker");
  assert.deepEqual(carrierLeft(h.fx), [], "no carrier is left behind by any outcome");
});

test("real broker: [7.24] a cargo that bypasses the wrapper by absolute path writes into the per-scan sentinel and fails the scan RK_CARGO_UNBOUND; a wrapped call is not flagged unbound; a scan that clears its scan token files its calls where no scan owns them", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "sb-bypass" });
  t.after(() => rb.stop());
  const h = host(t, { rb });
  const bypass = path.join(h.fx.root, "toolchain", "cargo");
  fs.mkdirSync(path.dirname(bypass), { recursive: true });
  fs.writeFileSync(bypass, '#!/bin/sh\nmkdir -p "$CARGO_TARGET_DIR/debug" && echo built > "$CARGO_TARGET_DIR/debug/out-$$"\nexit 0\n', { mode: 0o755 });
  assert.ok(h.installed.sentinel, "broker mode always has a sentinel: there is no checkout target to inherit");
  assert.equal(h.scanEnv.CARGO_TARGET_DIR, undefined, "and no scan-wide CARGO_TARGET_DIR");
  const abs = (cwd) => `spawnSync(${JSON.stringify(bypass)}, ["build"], { cwd: ${JSON.stringify(cwd)}, stdio: "inherit" }).status`;
  let result = await h.run([h.scan("bypass", `process.exit(${abs(h.a)})`), h.scan("wrapped", `${h.cargo(h.a, ["build", "--locked"])}; process.exit(0)`), h.scan("own-target", `process.exit(${abs(h.a)})`, { env: { CARGO_TARGET_DIR: path.join(h.fx.root, "declared") } })]);
  assert.equal(h.row(result, "bypass").status, "fail"); assert.equal(h.row(result, "bypass").code, "RK_CARGO_UNBOUND");
  assert.match(h.row(result, "bypass").detail, /bypassed the gate's wrapper wrote debug into the unbound sentinel/);
  assert.notEqual(h.row(result, "wrapped").code, "RK_CARGO_UNBOUND", "a wrapped call writes nothing into the sentinel");
  assert.equal(h.row(result, "own-target").status, "pass", "a scan that binds its own CARGO_TARGET_DIR gets no sentinel");
  // The code under test removes the scan token from the cargo's environment: the call is recorded, and the run fails on it.
  const env = JSON.stringify(Object.fromEntries(Object.entries(h.scanEnv).filter(([k]) => k !== SCAN_TOKEN_ENV)));
  result = await h.run([h.scan("clears-token", `spawnSync("cargo", ["build", "--locked"], { cwd: ${JSON.stringify(h.a)}, env: ${env}, stdio: "inherit" }); process.exit(0)`)]);
  const ledgerRow = h.row(result, "cargo-ledger");
  assert.ok(ledgerRow, JSON.stringify(result.scans.map((x) => [x.id, x.status])));
  assert.equal(ledgerRow.status, "fail"); assert.equal(ledgerRow.code, "RK_CARGO_LEDGER"); assert.match(ledgerRow.detail, /unscoped/);
  assert.equal(result.ok, false);
});

// ---- row 7.26: the snapshot is not re-synced while a broker job may still read it ----
test("data: [7.26] a dead owner's record is reclaimed only on the broker's REAPED for exactly its handle and snapshot; ACTIVE, UNKNOWN, malformed, stale, unreachable and another handle or snapshot hold it (the answers are wire-shaped inputs to the real lease parser)", { skip: POSIX }, (t) => {
  const root = tmpRoot(t, "rk-lease-");
  // What `rustright snapshot-lease status --snapshot S --ownership H` printed, per pair: the real parser reads it through the OS seam (`spawn`); no client runs.
  const asked = []; let answers = {};
  const client = "/x/rustright";
  const spawn = (file, args) => {
    asked.push(args.join(" "));
    if (answers.fail) return { status: 1, stdout: "", stderr: "" };
    const key = `${args[3]}|${args[5]}`;
    let v = answers.byPair?.[key] ?? answers.default;
    if (v === undefined) return { status: 1, stdout: "", stderr: "" };
    if (v && typeof v === "object" && !answers.noEcho) v = { snapshot: args[3], ownership: args[5], ...v };
    return { status: 0, stdout: `${typeof v === "string" ? v : JSON.stringify(v)}\n`, stderr: "" };
  };
  const snapshot = path.join(root, "snapshots", "k"); const handle = "dead-owner-handle";
  // The answer as the broker writes it (GB-23, GB-85): the member rows the aggregate state follows, and waitedMs / waitTimedOut.
  const status = (state, extra = {}) => ({ schema: "rustright.snapshot-lease-status.v1", state, members: state === "UNKNOWN" ? [] : [{ handle, state, spawned: true }], waitedMs: 0, waitTimedOut: false, ...extra });
  const ask = (over = {}) => brokerLeaseState({ snapshot, handle, env: {}, platform: "darwin", client, spawn, ...over });
  const set = (a) => { answers = a; };
  // The states the gate accepts: only a well-formed REAPED for the asked pair is REAPED.
  set({ byPair: { [`${snapshot}|${handle}`]: status("REAPED") } }); assert.equal(ask().state, "REAPED");
  assert.equal(ask({ snapshot: path.join(root, "snapshots", "other") }).state, "UNKNOWN", "the broker knows nothing of another snapshot: held");
  assert.equal(ask({ handle: "another-handle" }).state, "UNKNOWN", "or of another handle: held");
  assert.ok(asked.some((line) => line.includes(`snapshot-lease status --snapshot ${snapshot} --ownership ${handle}`)));
  for (const [name, a, state] of [["ACTIVE", { default: status("ACTIVE") }, "ACTIVE"], ["UNKNOWN", { default: status("UNKNOWN") }, "UNKNOWN"], ["malformed", { default: "{ nope" }, "UNKNOWN"], ["stale schema", { default: { schema: "rustright.snapshot-lease-status.v0", state: "REAPED" } }, "UNKNOWN"],
    ["unknown state", { default: status("CLEAR") }, "UNKNOWN"], ["unreachable", { fail: true }, "UNKNOWN"],
    ["REAPED with no echo of the pair", { default: status("REAPED"), noEcho: true }, "UNKNOWN"], ["REAPED echoing another snapshot", { default: status("REAPED", { snapshot: "/elsewhere" }) }, "UNKNOWN"], ["REAPED echoing another handle", { default: status("REAPED", { ownership: "another" }) }, "UNKNOWN"]]) {
    set(a); assert.equal(ask().state, state, name);
  }
  assert.equal(ask({ client: null }).state, "UNKNOWN", "no RustRight client: held");
  // And through the real reclaim: a dead owner whose broker work may exist (broker.submitted) is held unless REAPED.
  const file = path.join(root, "own", "owner.json"); fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 }); fs.chmodSync(path.dirname(file), 0o700);
  const dead = { schema: OWNERSHIP_SCHEMA, handle, host: os.hostname(), platform: "darwin", pid: 999_999, startedMs: NOW - 120_000, containment: { kind: "group", group: 777 }, ancestors: [{ pid: 5000, startedMs: NOW - 500_000 }],
    stage: "compile", snapshot, commit: "c".repeat(40), acquiredAt: new Date(NOW - 100_000).toISOString(), broker: { submitted: true } };
  // The host rebooted after the record was written, so every process proof is moot and the only question left is the broker's lease.
  const rebooted = { platform: "darwin", identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW - 60_000, group: 4242 } : { alive: false, startedMs: null, group: null }), bootTime: () => NOW,
    groupSnapshot: () => { throw new Error("must not be asked"); }, tokenHolders: () => null, snapshotUsers: () => null, ancestors: () => [] };
  const tryAcquire = (lease) => () => acquireBuildOwnership({ file, platform: "darwin", probe: rebooted, lease, env: {}, exportEnv: false, stage: "compile" });
  for (const [name, a, ok] of [["REAPED for the exact pair", { byPair: { [`${snapshot}|${handle}`]: status("REAPED") } }, true], ["REAPED for another snapshot", { byPair: { [`${snapshot}-x|${handle}`]: status("REAPED") } }, false],
    ["REAPED for another handle", { byPair: { [`${snapshot}|other`]: status("REAPED") } }, false], ["ACTIVE", { default: status("ACTIVE") }, false], ["UNKNOWN", { default: status("UNKNOWN") }, false], ["unreachable", { fail: true }, false], ["REAPED with no echo", { default: status("REAPED"), noEcho: true }, false]]) {
    fs.writeFileSync(file, JSON.stringify(dead)); set(a);
    const go = tryAcquire((q) => brokerLeaseState({ ...q, env: {}, platform: "darwin", client, spawn }));
    if (ok) { const own = go(); assert.match(own.reclaimed.proofs.join("; "), /lease for dead-owner-handle is REAPED/, name); own.release(); } else assert.throws(go, /broker work may exist under this handle and its lease is (ACTIVE|UNKNOWN)/, name);
  }
});

test("real broker: [7.26] a dead owner whose broker work may exist is held on the real broker's answer: a pair the real broker holds nothing for is UNKNOWN, never REAPED", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "sb-lease" });
  t.after(() => rb.stop());
  const root = tmpRoot(t, "rk-lease-real-");
  const snapshot = path.join(rb.dataRoot, "snapshots", "k"); const handle = "dead-owner-handle";
  const real = brokerLeaseState({ snapshot, handle, env: rb.env, platform: process.platform, client: rb.clientPath });
  assert.equal(real.state, "UNKNOWN", JSON.stringify(real));
  assert.notEqual(real.state, "REAPED");
  const file = path.join(root, "own", "owner.json"); fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 }); fs.chmodSync(path.dirname(file), 0o700);
  const dead = { schema: OWNERSHIP_SCHEMA, handle, host: os.hostname(), platform: "darwin", pid: 999_999, startedMs: NOW - 120_000, containment: { kind: "group", group: 777 }, ancestors: [{ pid: 5000, startedMs: NOW - 500_000 }],
    stage: "compile", snapshot, commit: "c".repeat(40), acquiredAt: new Date(NOW - 100_000).toISOString(), broker: { submitted: true } };
  fs.writeFileSync(file, JSON.stringify(dead));
  const rebooted = { platform: "darwin", identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW - 60_000, group: 4242 } : { alive: false, startedMs: null, group: null }), bootTime: () => NOW,
    groupSnapshot: () => { throw new Error("must not be asked"); }, tokenHolders: () => null, snapshotUsers: () => null, ancestors: () => [] };
  assert.throws(() => acquireBuildOwnership({ file, platform: "darwin", probe: rebooted, lease: (q) => brokerLeaseState({ ...q, env: rb.env, platform: process.platform, client: rb.clientPath }), env: {}, exportEnv: false, stage: "compile" }), (e) => e instanceof OwnershipBusy && /broker work may exist under this handle and its lease is UNKNOWN/.test(e.message));
});

// ---- row 7.20: `deps.snapshotBroker` is wired in production on broker hosts, in the gate's snapshot-compile region ----
test("real broker: [7.20] the gate refuses before anything compiles without an enrollment or with a broker it cannot reach (the positive wiring needs a snapshot the real broker holds and is not expressed here)", { skip: POSIX }, async (t) => {
  const run = async ({ enroll = true, executor = { kind: "broker", cargo: ["rustright", "cargo"] } } = {}) => {
    const rb = await startRealBroker({ name: "sb-gate" });
    t.after(() => rb.stop());
    const root = tmpRoot(t, "rk-gate-");
    const repo = path.join(root, "checkout"); fs.mkdirSync(repo, { recursive: true });
    writeTree(repo, WORKSPACES);
    // The commit tracks its declaration and the input it names, so the snapshot (the commit's bytes) carries both.
    fs.mkdirSync(path.join(repo, ".rightkit")); fs.writeFileSync(path.join(repo, ".rightkit", "compile-inputs.json"), JSON.stringify({ inputs: ["gen/in.txt"] }));
    fs.mkdirSync(path.join(repo, "gen")); fs.writeFileSync(path.join(repo, "gen", "in.txt"), "declared input\n");
    const g = (...a) => spawnSync("git", a, { cwd: repo, encoding: "utf8" });
    g("init", "-q", "-b", "main"); g("add", "-A"); g("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "c");
    const sha = g("rev-parse", "HEAD").stdout.trim();
    const snap = path.join(root, "gate-snapshots", "gate-snapshot"); fs.mkdirSync(snap, { recursive: true });
    for (const f of g("ls-files", "-z").stdout.split("\0").filter(Boolean)) { fs.mkdirSync(path.dirname(path.join(snap, f)), { recursive: true }); fs.copyFileSync(path.join(repo, f), path.join(snap, f)); }
    if (enroll) enrollBrokerPins({ dataRoot: rb.dataRoot, platform: "darwin", ...PINS });
    const env = { ...process.env, ...rb.env, XDG_STATE_HOME: root, RUSTRIGHT_CALLING_GATE: GATE };
    for (const k of ["GITHUB_ACTIONS", "RIGHTKIT_BUILD_OWNERSHIP", "RIGHTKIT_BUILD_OWNERSHIP_FILE", "CARGO_TARGET_DIR"]) delete env[k];
    const spawn = (c, a, opts) => {
      if (c === "git" || c === rb.clientPath) return spawnSync(c, a, { ...opts, encoding: "utf8" });
      if (c === "which" && a[0] === "rustright") return { status: 0, stdout: `${rb.clientPath}\n`, stderr: "" };
      if (c === "which" && a[0] === "cargo") return { status: 0, stdout: "/opt/toolchain/bin/cargo\n", stderr: "" };
      return { status: 1, stdout: "", stderr: `not in this test: ${c} ${a.join(" ")}` };
    };
    const own = acquireBuildOwnership({ file: path.join(root, "build-ownership", "owner.json"), platform: "darwin", probe: probe(), lease: noLease, env: {}, exportEnv: false, stage: "compile", snapshot: snap });
    const seen = []; const out = [];
    const code = await main(["--all", "--commit", sha, "--snapshot", snap], { cwd: repo, platform: "darwin", env, spawn, out: (m) => out.push(m), err: (m) => out.push(m), appCheck: async () => 0, buildOwnership: own, readStdin: () => "", resolveExecutor: () => executor,
      runScans: async (plan, options) => { seen.push({ plan, options }); return { scans: [], notes: [], cacheHits: [] }; } });
    return { code, seen, out: out.join("\n") };
  };
  // An app repository with no enrollment compiles nothing through a broker.
  const none = await run({ enroll: false });
  assert.equal(none.code, 1, none.out); assert.match(none.out, /BLOCKED: RK_BROKER_PINS_MISSING: no broker enrollment/); assert.equal(none.seen.length, 0, "nothing was planned or compiled");
  // A broker-managed host whose broker cannot be reached is refused, never compiled around.
  const refused = await run({ executor: { kind: "refuse", cargo: null, problem: "the broker is not reachable" } });
  assert.equal(refused.code, 1); assert.match(refused.out, /BLOCKED: the broker is not reachable; nothing ran/); assert.equal(refused.seen.length, 0);
});

test("data: [RR status envelope] the receipt is read from `{type:\"STATUS\", state, receipt}`; a null receipt (job still live) after the cargo process exited is a clear not-ready error and never a poll; a bare receipt with no envelope is refused", () => {
  const read = (doc) => receiptSync({ client: "/x/rustright", requestId: "job-1", env: {}, spawn: () => ({ status: 0, stdout: JSON.stringify(doc), stderr: "" }) });
  assert.throws(() => read({ type: "STATUS", id: "job-1", state: "RUNNING", receipt: null }), (e) => e.code === "receipt-not-ready" && /no receipt yet, although its cargo process exited; the gate does not poll/.test(e.message));
  assert.throws(() => read({ id: "job-1", state: "SUCCEEDED", exitCode: 0 }), (e) => e.code === "receipt" && /the reply is not a valid status for this request \(the reply is not a typed object\)/.test(e.message));
  // The full terminal envelope returns the receipt it carries (its signature and evidence are judged by verifySnapshotReceipt, not here).
  const receipt = { id: "job-1", state: "SUCCEEDED", exitCode: 0 };
  assert.deepEqual(read({ type: "STATUS", id: "job-1", state: "SUCCEEDED", receipt }), receipt);
});

test("data: [B8] a carried call under a release-bound stage derives its nonce from the stage's, its directory (relative to the primary checkout) and its argv; otherwise it is fresh; the stage nonce never reaches the client", { skip: POSIX }, (t) => {
  const h = host(t);
  const records = []; const spawn = recording();
  const stageNonce = "ab".repeat(32);
  const call = (env, { cwd = path.join(h.fx.snapshot, "crates", "a"), words = ["build", "--locked"] } = {}) => {
    const r = h.carry(words, { cwd, env: { PATH: process.env.PATH, RUSTRIGHT_CALLING_GATE: GATE, ...env }, records, spawn });
    assert.notEqual(r.outcome.status, "refused", JSON.stringify(r.outcome));
    return { nonce: records.filter((e) => e.status === "pending").at(-1).nonce, sentEnv: spawn.calls.at(-1).options.env };
  };
  const inCrate = call({ RIGHTKIT_ATTEST_STAGE_NONCE: stageNonce });
  assert.equal(inCrate.nonce, carriedCallNonce({ stageNonce, cwd: "crates/a", argv: ["build", "--locked"] }));
  assert.ok(!Object.keys(inCrate.sentEnv).some((k) => k.toUpperCase() === "RIGHTKIT_ATTEST_STAGE_NONCE"), "stripped from the client's environment");
  // Another directory or another argv is another nonce.
  assert.equal(call({ RIGHTKIT_ATTEST_STAGE_NONCE: stageNonce }, { cwd: h.fx.snapshot }).nonce, carriedCallNonce({ stageNonce, cwd: ".", argv: ["build", "--locked"] }));
  assert.equal(call({ RIGHTKIT_ATTEST_STAGE_NONCE: stageNonce }, { words: ["check"] }).nonce, carriedCallNonce({ stageNonce, cwd: "crates/a", argv: ["check", "--locked"] }), "another command, another nonce");
  // Without a stage nonce, or with a malformed one, the nonce is fresh and different every call.
  const fresh = [call({}), call({}), call({ RIGHTKIT_ATTEST_STAGE_NONCE: "not-hex" }), call({ RIGHTKIT_ATTEST_STAGE_NONCE: "AB".repeat(32) })].map((c) => c.nonce);
  assert.equal(new Set(fresh).size, 4);
  for (const n of fresh) { assert.match(n, /^[0-9a-f]{64}$/); assert.notEqual(n, inCrate.nonce); }
});

test("real broker: [submit wiring] createSnapshotBroker with the REAL build ownership passes each carried call's sub-handle through: a carried metadata call and a carried cargo call are both sent and the owner record lists both sub-handles", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "sb-wiring" });
  t.after(() => rb.stop());
  const h = host(t, { rb });
  const file = path.join(h.fx.root, "real-ownership", "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(path.dirname(file)), { recursive: true, mode: 0o700 });
  const own = acquireBuildOwnership({ file, platform: "darwin", probe: probe(), lease: (q) => brokerLeaseState({ ...q, env: rb.env, platform: process.platform, client: rb.clientPath }), env: {}, exportEnv: false, stage: "compile", snapshot: h.fx.snapshot });
  t.after(() => { try { own.release(); } catch { /* the real broker holds the sub-handles: the record stays held */ } });
  const sb = createSnapshotBroker({ env: { RIGHTKIT_DATA_ROOT: rb.dataRoot, RIGHTKIT_CONTROL_ROOT: rb.controlRoot, XDG_STATE_HOME: h.fx.root }, platform: process.platform, checkout: h.fx.checkout, commit: COMMIT, snapshot: { path: h.fx.snapshot, expectedTree: COMMIT.tree }, ownership: own, client: rb.clientPath, dataRoot: rb.dataRoot, loadPins: () => ({ ...PINS, source: "test" }) });
  const records = [];
  const call = (words) => carryBrokerCargo({ words, cwd: path.join(h.fx.snapshot, "crates", "a"), env: { PATH: process.env.PATH, RUSTRIGHT_CALLING_GATE: GATE, ...rb.env }, section: brokerSection(sb), workspace: h.fx.snapshot, checkoutWorkspace: h.fx.checkout, record: (e) => records.push(e), err: () => {}, stdio: "capture" });
  const metadata = call(["metadata", "--no-deps", "--format-version", "1"]);
  assert.notEqual(metadata.outcome.status, "refused", JSON.stringify(metadata.outcome));
  const build = call(["build", "--locked"]);
  assert.notEqual(build.outcome.status, "refused", JSON.stringify(build.outcome));
  const sent = records.filter((r) => r.status === "pending").map((r) => r.ownershipHandle);
  assert.equal(sent.length, 2);
  for (const sub of sent) assert.ok(isCallHandleOf(sub, own.handle), `${sub} is a sub-handle of ${own.handle}`);
  assert.equal(new Set(sent).size, 2, "each call has its own sub-handle");
  const owner = JSON.parse(fs.readFileSync(file, "utf8"));
  assert.equal(owner.broker.submitted, true);
  assert.deepEqual(owner.broker.handles, sent, "the owner record lists both sub-handles, in send order");
});

test("data: [GB-16] a commit whose directory the checkout has removed is carried: primaryCwd is the lexical counterpart, the client is spawned from the nearest existing ancestor; only a counterpart that leaves the checkout is refused", { skip: POSIX }, (t) => {
  const h = host(t);
  const fx = h.fx;
  const records = []; const spawn = recording();
  const call = (rel) => h.carry(["build", "--locked"], { cwd: path.join(fx.snapshot, rel), records, spawn });
  // Present: maps by the snapshot-relative path, spawned from the counterpart itself.
  const ok = call("crates/a");
  assert.notEqual(ok.outcome.status, "refused", JSON.stringify(ok.outcome));
  const present = records.find((r) => r.status === "pending");
  assert.equal(present.checkoutCwd, path.join(fx.checkout, "crates", "a"));
  assert.equal(present.spawnCwd, path.join(fx.checkout, "crates", "a"));
  assert.equal(spawn.calls.at(-1).options.cwd, path.join(fx.checkout, "crates", "a"));
  // The selected (older) commit has crates/a; the newer checkout deleted it. The call is carried: no ABSENT refusal, the carrier names the lexical counterpart, and the client starts in the
  // nearest directory of it that still exists (crates/).
  fs.rmSync(path.join(fx.checkout, "crates", "a"), { recursive: true, force: true });
  const marks = fx.marks.length; records.length = 0;
  const gone = call("crates/a");
  assert.notEqual(gone.outcome.status, "refused", JSON.stringify(gone.outcome));
  assert.notEqual(gone.outcome.code, "RK_CARGO_BROKER_COUNTERPART_ABSENT");
  const pending = records.find((r) => r.status === "pending");
  assert.equal(pending.checkoutCwd, path.join(fx.checkout, "crates", "a"), "primaryCwd is the lexical counterpart (absolute, no . or ..), though it no longer exists");
  assert.equal(fs.existsSync(pending.checkoutCwd), false);
  assert.equal(pending.spawnCwd, path.join(fx.checkout, "crates"), "the client runs from the nearest existing ancestor");
  assert.equal(spawn.calls.at(-1).options.cwd, path.join(fx.checkout, "crates"));
  assert.equal(fx.marks.length, marks + 1, "one sub-handle recorded for the carried call");
  assert.equal(fx.carriers.at(-1).primaryCwd, path.join(fx.checkout, "crates", "a"), "the carrier names the lexical counterpart");
  // Several levels removed: the nearest existing ancestor is the checkout itself, and a directory that became a plain file is passed over (the nearest DIRECTORY).
  fs.rmSync(path.join(fx.checkout, "crates"), { recursive: true, force: true });
  fs.writeFileSync(path.join(fx.checkout, "crates"), "now a file\n");
  records.length = 0;
  const deep = call("crates/a");
  assert.notEqual(deep.outcome.status, "refused", JSON.stringify(deep.outcome));
  assert.equal(records.find((r) => r.status === "pending").spawnCwd, fx.checkout);
  // Negative: a counterpart whose existing ancestor is a link leaving the checkout is still refused, before any mark or send.
  fs.rmSync(path.join(fx.checkout, "crates"), { force: true });
  const outside = fs.mkdtempSync(path.join(fx.root, "outside-")); fs.mkdirSync(path.join(outside, "x"));
  fs.symlinkSync(path.join(outside, "x"), path.join(fx.checkout, "crates"));
  const calls = spawn.calls.length; const marked = fx.marks.length;
  const escaped = call("crates/a");
  assert.equal(escaped.outcome.status, "refused");
  assert.equal(escaped.outcome.code, "RK_CARGO_BROKER_COUNTERPART_ESCAPE"); assert.match(escaped.outcome.message, /outside the primary checkout/);
  assert.equal(spawn.calls.length, calls); assert.equal(fx.marks.length, marked);
  // Negative: an existing counterpart that is a link leaving the checkout is refused as before.
  fs.rmSync(path.join(fx.checkout, "crates"), { force: true });
  fs.mkdirSync(path.join(fx.checkout, "crates"));
  fs.symlinkSync(path.join(outside, "x"), path.join(fx.checkout, "crates", "a"));
  assert.equal(call("crates/a").outcome.code, "RK_CARGO_BROKER_COUNTERPART_ESCAPE");
});

test("data: [GB-16] an adapter step of a commit whose directory the checkout has removed is carried the same way (RK_ADAPTER_BROKER_COUNTERPART_ABSENT is no longer raised for it); the carrier's primaryCwd is the lexical counterpart", { skip: POSIX }, (t) => {
  const h = host(t);
  const fx = h.fx;
  fs.mkdirSync(path.join(fx.snapshot, "apps", "web"), { recursive: true });
  const records = []; const spawn = recording();
  const call = () => carryBrokerAdapter({ adapter: "rightkit-pnpm", words: ["run", "build"], cwd: path.join(fx.snapshot, "apps", "web"), env: { PATH: process.env.PATH, RUSTRIGHT_CALLING_GATE: GATE }, section: brokerSection(fx.sb), record: (e) => records.push(e), err: () => {}, stdio: "capture", spawn });
  assert.equal(fs.existsSync(path.join(fx.checkout, "apps", "web")), false, "the checkout never had apps/web");
  const r = call();
  assert.notEqual(r.outcome.status, "refused", JSON.stringify(r.outcome));
  const pending = records.find((e) => e.status === "pending");
  assert.equal(pending.checkoutCwd, path.join(fx.checkout, "apps", "web"));
  assert.equal(pending.spawnCwd, fx.checkout);
  assert.equal(spawn.calls.at(-1).options.cwd, fx.checkout);
  assert.equal(fx.carriers.at(-1).primaryCwd, path.join(fx.checkout, "apps", "web"));
});


test("data: [7.10] the ledger itself: unfinished and unreadable records fail, a later record replaces an earlier one, and a call that cannot be recorded does not run", (t) => {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-ledger-"))); t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const token = scanTokenOf("x");
  assert.deepEqual(readLedger(dir, token), { entries: [], problems: [] });
  appendLedger(dir, token, { id: "1", mode: "direct", cwd: "/w", argv: ["build"], status: "pending" });
  assert.match(readLedger(dir, token).problems[0], /never completed.*unverified/);
  appendLedger(dir, token, { id: "1", mode: "direct", cwd: "/w", argv: ["build"], status: "ran", exit: 0 });
  assert.deepEqual(readLedger(dir, token).problems, [], "completed");
  appendLedger(dir, token, { id: "2", mode: "direct", cwd: "/w", argv: ["clean"], status: "refused", code: "RK_CARGO_UNBOUND_COMMAND", message: "no" });
  assert.match(readLedger(dir, token).problems[0], /cargo clean \(from \/w\) was refused \(RK_CARGO_UNBOUND_COMMAND\): no/);
  fs.appendFileSync(path.join(dir, `${token}.jsonl`), "{ broken\n");
  assert.ok(readLedger(dir, token).problems.some((p) => /unreadable ledger record/.test(p)));
  assert.deepEqual(unclaimedLedgers(dir, new Set()), [token]); assert.deepEqual(unclaimedLedgers(dir, new Set([token])), []);
  // A map whose ledger directory cannot be written: the wrapper refuses before it runs anything.
  const shim = path.join(dir, "map");
  fs.writeFileSync(shim, JSON.stringify({ schema: "rightkit.cargo-target-shim/3", mode: "direct", realCargo: "/nonexistent/cargo", shim: "/x", ledgerDir: path.join(dir, "ledger-file", "x"), workspaces: {} }));
  fs.writeFileSync(path.join(dir, "ledger-file"), "a file, not a directory");
  const r = spawnSync(process.execPath, [fileURLToPath(new URL("./cargo-target-shim.mjs", import.meta.url)), "--map", shim, "--", "build"], { encoding: "utf8", env: { ...process.env, NODE_OPTIONS: "" } });
  assert.equal(r.status, 101); assert.match(r.stderr, /RK_CARGO_LEDGER_ERROR: the call cannot be recorded/);
});

test("data: [7.24] the broker wrapper resolves workspaces from the files alone (no cargo outside the broker) and passes nothing else through", { skip: POSIX }, (t) => {
  const h = host(t);
  assert.equal(locateWorkspaceByWalk(null, path.join(h.a, "src")), path.join(h.fx.snapshot, "Cargo.toml"), "a member resolves to its workspace root");
  assert.equal(locateWorkspaceByWalk(null, path.join(h.native, "src")), path.join(h.native, "Cargo.toml"));
  assert.equal(locateWorkspaceByWalk(path.join(h.a, "Cargo.toml"), h.fx.root), path.join(h.fx.snapshot, "Cargo.toml"));
  const lone = path.join(h.fx.root, "lone"); fs.mkdirSync(lone); fs.writeFileSync(path.join(lone, "Cargo.toml"), '[package]\nname = "x"\n');
  assert.equal(locateWorkspaceByWalk(null, lone), path.join(lone, "Cargo.toml"), "no [workspace] anywhere: the nearest manifest is its own root");
  assert.throws(() => locateWorkspaceByWalk(null, os.tmpdir()), /could not find/);
  const map = JSON.parse(fs.readFileSync(h.installed.mapFile, "utf8"));
  assert.equal(map.mode, "broker"); assert.equal(map.realCargo, null);
  assert.deepEqual(map.workspaces, { [h.fx.snapshot]: h.fx.checkout, [h.native]: path.join(h.fx.checkout, "native") }, "the gate's own snapshot-to-checkout map");
  assert.deepEqual(map.broker.pins, h.fx.pins); assert.equal(map.broker.handle, h.fx.handle); assert.equal(map.broker.sentinel, h.installed.sentinel);
  // The wrapper's identity binds the broker section: another commit, handle or pin set is another wrapper (and another scan cache key).
  const other = installCargoShim({ base: path.join(h.fx.root, "shim2"), snapshotRoot: h.fx.snapshot, realCargo: null, broker: { checkoutDirs: { ".": h.fx.checkout, native: path.join(h.fx.checkout, "native") }, section: { ...brokerSection(h.fx.sb), handle: "other" } } });
  assert.notEqual(other.identity, h.installed.identity);
  assert.throws(() => installCargoShim({ base: path.join(h.fx.root, "shim3"), snapshotRoot: h.fx.snapshot, realCargo: null, broker: {} }), /needs the gate's checkout map and broker section/);
  assert.throws(() => installCargoShim({ base: path.join(h.fx.root, "shim4"), snapshotRoot: h.fx.snapshot, realCargo: null, broker: { checkoutDirs: {}, section: {} }, platform: "win32" }), /needs a POSIX host/);
});

test("data: [7.24] broker-mode decisions: the workspace's counterpart in the checkout, no output variable, and --manifest-path, --target-dir, redirects and unbound commands refused with their codes", (t) => {
  const fx = hostDirs(t);
  const map = { mode: "broker", shim: "/gate/bin/cargo", workspaces: { [fx.snapshot]: fx.checkout, [path.join(fx.snapshot, "native")]: path.join(fx.checkout, "native") } };
  const locate = (manifest, cwd) => locateWorkspaceByWalk(manifest, cwd);
  const decide = (args, cwd) => decideCargoInvocation({ args, cwd, env: { PATH: "/bin" }, map, locate, exists: () => false });
  const a = decide(["test", "--locked", "-p", "a"], path.join(fx.snapshot, "crates", "a"));
  assert.equal(a.exec.checkoutWorkspace, fx.checkout); assert.equal(a.exec.workspace, fx.snapshot);
  assert.equal(a.exec.env.CARGO_TARGET_DIR, undefined); assert.equal(a.exec.env.CARGO_BUILD_TARGET_DIR, undefined, "the broker pins the output; the gate passes no output path");
  assert.equal(decide(["build"], path.join(fx.snapshot, "native", "src")).exec.checkoutWorkspace, path.join(fx.checkout, "native"));
  assert.equal(decide(["build", "--manifest-path", "x/Cargo.toml"], fx.snapshot).refuse?.code, "RK_CARGO_BROKER_MANIFEST_PATH");
  assert.equal(decide(["build", "--target-dir", path.join(fx.root, "t")], fx.snapshot).refuse?.code, "RK_CARGO_REDIRECT");
  assert.equal(decide(["--config", "build.target-dir='x'", "build"], fx.snapshot).refuse?.code, "RK_CARGO_REDIRECT");
  assert.equal(decide(["clean"], fx.snapshot).refuse?.code, "RK_CARGO_UNBOUND_COMMAND");
  assert.equal(decide(["tree"], fx.snapshot).refuse?.code, "RK_CARGO_UNBOUND_COMMAND", "tree needs --locked");
  assert.equal(decide(["build"], path.join(fx.root, "checkout", "crates", "a")).refuse?.code, "RK_CARGO_UNBOUND_WORKSPACE", "the checkout's own directory is not a workspace the gate's snapshot compile resolved");
  assert.equal(decide(["--version"], fx.root).exec.workspace, undefined, "a call that reads no workspace is passed through");
});

test("data: [M8-M11] broker-mode wrapper: metadata, deny, audit and fmt are carried only in the broker's exact shapes; aliases are expanded before classification and sending; help, --list and every other uncarried call inside the snapshot are refused", (t) => {
  const fx = hostDirs(t);
  const abs = (name) => path.join(fx.root, name);
  fs.writeFileSync(abs("deny.toml"), ""); fs.writeFileSync(abs("meta.json"), "{}"); fs.mkdirSync(abs("advisory-db"));
  const map = { mode: "broker", shim: "/gate/bin/cargo", workspaces: { [fx.snapshot]: fx.checkout }, broker: { snapshot: { path: fx.snapshot }, checkout: fx.checkout } };
  const locate = (manifest, cwd) => locateWorkspaceByWalk(manifest, cwd);
  const decide = (args, cwd = fx.snapshot, exists = () => true) => decideCargoInvocation({ args, cwd, env: { PATH: "/bin" }, map, locate, exists });
  const SHAPE = "RK_CARGO_BROKER_READ_ONLY_SHAPE";
  // metadata: --no-deps, --locked --offline, --frozen (and the broker's other pinned forms); nothing added, nothing else carried.
  for (const args of [["metadata", "--no-deps"], ["metadata", "--no-deps", "--format-version", "1"], ["metadata", "--locked", "--offline"], ["metadata", "--frozen"], ["metadata", "--frozen", "--format-version", "1"], ["metadata", "--locked"]]) {
    const r = decide(args);
    assert.deepEqual(r.exec?.args, args, `${args.join(" ")}: carried as given`);
  }
  for (const args of [["metadata"], ["metadata", "--format-version", "1"], ["metadata", "--offline"], ["metadata", "--format-version", "2", "--no-deps"], ["metadata", "--no-deps", "--no-deps"], ["metadata", "--locked", "--features", "x"], ["--locked", "metadata"], ["+stable", "metadata", "--no-deps"], ["metadata", "--no-deps", "--", "x"]]) {
    assert.equal(decide(args).refuse?.code, SHAPE, `${args.join(" ")} is refused`);
  }
  // deny and audit and fmt: the exact shapes snapshot-metadata.mjs carriedScanRefusal uses.
  assert.ok(decide(["deny", "--frozen", "--metadata-path", abs("meta.json"), "--config", abs("deny.toml"), "check"]).exec);
  assert.ok(decide(["deny", "check"], fx.snapshot, () => true).refuse, "deny without --locked or --frozen is still refused by the wrapper's lock rule");
  assert.equal(decide(["deny", "--frozen", "--config", "deny.toml", "check"]).refuse?.code, SHAPE, "a relative path is not carried");
  assert.equal(decide(["deny", "--frozen", "check", "--all-features"]).refuse?.code, SHAPE, "nothing after check");
  assert.equal(decide(["deny", "--frozen", "--graph", "x", "check"]).refuse?.code, SHAPE);
  assert.ok(decide(["audit", "--db", abs("advisory-db"), "--no-fetch"]).exec);
  for (const args of [["audit"], ["audit", "--no-fetch"], ["audit", "--db", abs("advisory-db")], ["audit", "--db", "advisory-db", "--no-fetch"], ["audit", "--db", abs("advisory-db"), "--no-fetch", "--deny", "warnings"]]) assert.equal(decide(args).refuse?.code, SHAPE, args.join(" "));
  assert.ok(decide(["fmt", "--all", "--check"]).exec); assert.ok(decide(["fmt", "-p", "a", "--check"]).exec);
  for (const args of [["fmt", "--check"], ["fmt", "-p", "a"], ["fmt", "--all", "--check", "--", "--edition", "2021"], ["fmt", "-p", "a", "--check", "--all"], ["fmt", "--check", "--all"]]) assert.ok(decide(args).refuse, `${args.join(" ")} is refused`);
  assert.equal(decide(["fmt", "--check", "--all"]).refuse?.code, SHAPE, "fmt --check --all is not the broker's order");
  // Aliases: b, c, t, r, d are expanded before the classification and before sending (the signed argv is the one sent); other words are untouched.
  for (const [alias, full] of [["b", "build"], ["c", "check"], ["t", "test"], ["r", "run"], ["d", "doc"]]) {
    const r = decide([alias, "--locked", "-p", "a"]);
    assert.deepEqual(r.exec?.args, [full, "--locked", "-p", "a"], `${alias} is sent as ${full}`);
    // GB-01: RustRight's normalisation refuses a `+toolchain` of a managed request (RK_REDIRECT_REFUSED), so the wrapper refuses it first and sends nothing.
    assert.equal(decide(["+stable", alias]).refuse?.code, "RK_REDIRECT_REFUSED", `+stable ${alias} is a redirect at the broker: it is not sent`);
    assert.deepEqual(decide(["--color", "always", alias]).exec?.args, ["--color", "always", full]);
  }
  assert.deepEqual(decide(["test", "--", "t"]).exec?.args, ["test", "--", "t"], "an argument after -- is not an alias");
  assert.deepEqual(decide(["build", "t"]).exec?.args, ["build", "t"], "only the subcommand is expanded");
  assert.equal(decide(["x"]).refuse?.code, "RK_CARGO_UNBOUND_COMMAND");
  // The alias is also unchanged in the direct (Linux) mode: the real cargo expands it itself.
  assert.deepEqual(decideCargoInvocation({ args: ["b"], cwd: fx.snapshot, env: {}, map: { shim: "/gate/bin/cargo", workspaces: { [fx.snapshot]: fx.checkout } }, locate, exists: () => true }).exec.args, ["b"]);
  // Uncarried calls inside the snapshot: only the broker's version probes. help, --list, no subcommand, `+toolchain --version` are refused there.
  for (const args of [["--version"], ["-V"], ["version"], ["-vV"], ["-Vv"], ["--version", "--verbose"], ["-V", "--verbose"], ["-v", "-V"]]) assert.ok(decide(args).exec && decide(args).exec.workspace === undefined, `${args.join(" ")} is a probe`);
  for (const args of [["help"], ["help", "build"], ["--list"], [], ["--help"], ["+stable", "--version"], ["-V", "-V"], ["version", "--verbose"], ["--explain", "E0001"]]) {
    assert.equal(decide(args).refuse?.code, "RK_CARGO_BROKER_UNCARRIED", `${args.join(" ") || "(no arguments)"} is refused inside the snapshot`);
  }
  assert.ok(decide(["--version", "extra"]).refuse, "a version flag with a word after it is not a probe (cargo reads `extra` as a command)");
  assert.ok(decide(["help"], fx.root).exec, "outside the snapshot nothing is in the snapshot's way: the wrapper does not refuse it");
  assert.ok(decide(["--list"], fx.root).exec);
  // The ledger/argv the wrapper sends is what it decided: nothing is added silently (the alias is the one rewrite, and it is the sent argv).
  assert.deepEqual(decide(["metadata", "--no-deps"]).exec.args, ["metadata", "--no-deps"]);
});

test("data: [7.20] production hands the door the carrier seam: a broker host takes the snapshot route with it and is refused RK_SNAPSHOT_CARRIER_NOT_WIRED without it; a host whose broker is unreachable is refused whichever", async () => {
  const { snapshotRouteRefusal } = await import("./bin/rightkit-gate.mjs");
  const { PRODUCTION_SNAPSHOT_CARRIER, createSnapshotBroker } = await import("./snapshot-broker.mjs");
  const door = (over) => snapshotRouteRefusal({ spawn: () => ({ status: 1, stdout: "", stderr: "" }), platform: "darwin", env: {}, exists: () => false, readFile: () => "", resolveExecutor: () => ({ kind: "broker", cargo: ["rustright", "cargo"] }), ...over });
  assert.equal(door({ snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER }), null);
  assert.equal(door({})?.code, "RK_SNAPSHOT_CARRIER_NOT_WIRED");
  assert.equal(door({ snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER, resolveExecutor: () => ({ kind: "refuse", cargo: null, problem: "unreachable" }) })?.code, "RK_SNAPSHOT_BROKER_UNAVAILABLE");
  assert.equal(PRODUCTION_SNAPSHOT_CARRIER.create, createSnapshotBroker); assert.ok(Object.isFrozen(PRODUCTION_SNAPSHOT_CARRIER));
  // rightkit#1301: the production entry hands the carrier to Mac and Dell (right-release's own calls and the Tauri CLI's are carried).
  const entry = fs.readFileSync(new URL("./bin/rightkit-gate.mjs", import.meta.url), "utf8");
  assert.match(entry, /productionEntryDeps\(\)\.then\(\(deps\) => main\(process\.argv\.slice\(2\), deps\)\)/);
  const { productionEntryDeps } = await import("./bin/rightkit-gate.mjs");
  assert.deepEqual(await productionEntryDeps({ platform: "darwin" }), { snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER });
  assert.deepEqual(await productionEntryDeps({ platform: "win32" }), { snapshotBroker: PRODUCTION_SNAPSHOT_CARRIER });
  assert.deepEqual(await productionEntryDeps({ platform: "linux" }), {});
});

test("data: [submit wiring] every markBrokerSubmit fake in the tests receives the call's `<handle>#<id>` sub-handle", () => {
  // A fake that ignores its argument is the shape of the defect that sent no sub-handle to the real ownership: this scan finds them.
  const dir = path.dirname(fileURLToPath(import.meta.url));
  const offenders = [];
  const stub = /markBrokerSubmit:\s*\(\s*\)\s*=>/;
  for (const base of [dir, path.join(dir, "test-support")]) {
    for (const name of fs.readdirSync(base).filter((n) => n.endsWith(".mjs"))) {
      fs.readFileSync(path.join(base, name), "utf8").split("\n").forEach((line, i) => { if (stub.test(line)) offenders.push(`${name}:${i + 1}`); });
    }
  }
  assert.deepEqual(offenders, [], "a markBrokerSubmit stub must take the call handle and assert its shape (see assertSubHandle)");
});

test("data: [GB-16] the counterpart is computed lexically for Windows and Mac spellings too: `.` and `..` never appear, the nearest existing directory is found by walking up whatever the platform's separators", () => {
  const win = { base: "C:\\Users\\dev\\repo", rel: "crates\\a\\sub" };
  assert.equal(lexicalCounterpart({ ...win, pathApi: path.win32 }), "C:\\Users\\dev\\repo\\crates\\a\\sub");
  assert.equal(lexicalCounterpart({ base: "/Users/dev/repo", rel: "crates/a", pathApi: path.posix }), "/Users/dev/repo/crates/a");
  assert.equal(lexicalCounterpart({ base: "/Users/dev/repo", rel: "", pathApi: path.posix }), "/Users/dev/repo");
  const exists = (set, api) => (p) => set.has(api.normalize(p));
  const winDirs = new Set(["C:\\", "C:\\Users", "C:\\Users\\dev", "C:\\Users\\dev\\repo", "C:\\Users\\dev\\repo\\crates"]);
  assert.equal(nearestExistingDirectory("C:\\Users\\dev\\repo\\crates\\a\\sub", { isDirectory: exists(winDirs, path.win32), pathApi: path.win32 }), "C:\\Users\\dev\\repo\\crates");
  assert.equal(nearestExistingDirectory("C:\\Users\\dev\\repo\\crates", { isDirectory: exists(winDirs, path.win32), pathApi: path.win32 }), "C:\\Users\\dev\\repo\\crates");
  const macDirs = new Set(["/", "/Users", "/Users/dev", "/Users/dev/repo"]);
  assert.equal(nearestExistingDirectory("/Users/dev/repo/crates/a", { isDirectory: exists(macDirs, path.posix), pathApi: path.posix }), "/Users/dev/repo");
  assert.equal(nearestExistingDirectory("/nowhere/at/all", { isDirectory: () => false, pathApi: path.posix }), null, "no ancestor at all is no answer, never a guess");
});

test("data: [GB-16] the output pin reads the selected commit's snapshot, not the checkout: a directory the newer checkout deleted, a dirty or added manifest there, change nothing; an absolute target outside the snapshot stands; several workspaces still refuse", async (t) => {
  const { cargoOutputPin } = await import("./bin/rightkit-gate.mjs");
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-pin-"))); t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  const root = path.join(base, "checkout"); const snapshot = path.join(base, "snap");
  for (const d of [root, path.join(snapshot, "a"), path.join(snapshot, "b")]) fs.mkdirSync(d, { recursive: true });
  for (const m of ["Cargo.toml", "a/Cargo.toml", "b/Cargo.toml"]) fs.writeFileSync(path.join(snapshot, m), "[package]\nname = \"x\"\n");
  // The newer checkout: a/ is gone, b/ has a dirty manifest, an untracked c/ exists.
  fs.mkdirSync(path.join(root, "b"), { recursive: true }); fs.writeFileSync(path.join(root, "b", "Cargo.toml"), "DIRTY garbage that is not a manifest");
  fs.mkdirSync(path.join(root, "c"), { recursive: true }); fs.writeFileSync(path.join(root, "c", "Cargo.toml"), "[package]\nname=\"c\"\n");
  const asked = [];
  const run = (manifests, meta) => cargoOutputPin({
    o: { env: {}, exists: () => { throw new Error("the checkout's files are never asked about"); }, spawn: (cmd, args, opts) => { const file = args[args.indexOf("--manifest-path") + 1]; asked.push({ file, cwd: opts.cwd }); return { status: 0, stdout: JSON.stringify(meta(file)), stderr: "" }; } },
    git: () => ({ status: 0, stdout: manifests.join("\0"), stderr: "" }), root, commit: "c".repeat(40), executor: { cargo: ["cargo"] }, snapshot,
  });
  const one = run(["a/Cargo.toml"], (file) => ({ workspace_root: path.dirname(file), target_directory: path.join(path.dirname(file), "target") }));
  assert.deepEqual({ dir: one.dir, dirs: one.dirs }, { dir: path.join(root, "a", "target"), dirs: [path.join(root, "a", "target")] }, "the snapshot's target maps to the same relative path under the checkout, which does not have a/");
  assert.ok(asked.every((q) => q.file.startsWith(snapshot) && q.cwd.startsWith(snapshot)), "metadata was asked in the snapshot only");
  assert.equal(run(["b/Cargo.toml"], (file) => ({ workspace_root: path.dirname(file), target_directory: path.join(path.dirname(file), "target") })).dir, path.join(root, "b", "target"), "a dirty manifest in the checkout is not read");
  assert.equal(run(["Cargo.toml"], () => ({ workspace_root: snapshot, target_directory: path.join(base, "shared-target") })).dir, path.join(base, "shared-target"), "an absolute target outside the snapshot stands");
  assert.equal(run([], () => assert.fail("no manifest")).dir, null);
  const two = run(["a/Cargo.toml", "b/Cargo.toml"], (file) => ({ workspace_root: path.dirname(file), target_directory: path.join(path.dirname(file), "target") }));
  // GB-17: two workspaces are two independent targets, resolved from the snapshot and mapped to the checkout: no refusal, no shared directory.
  assert.equal(two.refusal, undefined); assert.equal(two.dir, null);
  assert.deepEqual(two.dirs, [path.join(root, "a", "target"), path.join(root, "b", "target")]);
  assert.deepEqual([...two.workspaces.keys()], [path.join(root, "a"), path.join(root, "b")], "keyed by the checkout-relative workspace");
  assert.match(run(["a/Cargo.toml"], () => ({})).refusal, /metadata in the snapshot reported no absolute target_directory for a\/Cargo\.toml/);
});
