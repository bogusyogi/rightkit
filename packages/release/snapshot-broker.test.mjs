// Item 7 rows 7.10, 7.20 and 7.24 (decision C) on a broker host: cargo reached by tests goes through the runtime wrapper's broker
// mode, one carried request per call, every call in the gate-owned ledger, and the scan fails on any refused or unverified call
// even when the test ignores cargo's exit status. The broker here is a FIXTURE (a fake `rustright` client that applies the reader's
// carrier rules and serves receipts signed in-test with a real P-256 key); native runs against RustRight's item-4 dev install
// are separate evidence and BLOCKED until it exists. Row 7.26 (lease lifetime) is exercised through the real `brokerLeaseState`
// and the real ownership reclaim with a fake client.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";
import { SCAN_TOKEN_ENV, appendLedger, readLedger, scanTokenOf, unclaimedLedgers } from "./cargo-ledger.mjs";
import { decideCargoInvocation, installCargoShim, locateWorkspaceByWalk, shimScanEnv } from "./cargo-target-shim.mjs";
import { runScans } from "./deterministic-scans.mjs";
import { acquireBuildOwnership } from "./build-ownership.mjs";
import { brokerSection, carryBrokerCargo, createSnapshotBroker, verifyBrokerLedger } from "./snapshot-broker.mjs";
import { carriedCallNonce } from "./provenance-verify.mjs";
import { argvSha256, isCallHandleOf, writeSnapshotRequest } from "./snapshot-request.mjs";
import { makeFakeBroker, subHandleStub } from "./test-support/fake-broker.mjs";

const POSIX = process.platform === "win32" ? "the runtime wrapper is POSIX only: a Windows child finds only an .exe on PATH; the Dell refuses by the static walk" : false;
const GATE = "/gate/bin/rightkit-gate.mjs";

/** A fixture broker host with the broker-mode wrapper installed, and a runner for `node -e` scans that reach cargo through it. */
function host(t, { inputs = null } = {}) {
  const fx = makeFakeBroker(t, { inputs });
  const installed = installCargoShim({
    base: path.join(fx.root, "shim"), snapshotRoot: fx.snapshot, realCargo: null,
    broker: { checkoutDirs: { ".": fx.checkout, native: path.join(fx.checkout, "native") }, section: brokerSection(fx.sb) },
  });
  t.after(() => fs.rmSync(installed.dir, { recursive: true, force: true }));
  const clean = { ...process.env, NODE_OPTIONS: "", RUSTRIGHT_CALLING_GATE: GATE, ...fx.ownershipEnv };
  for (const k of ["CARGO_TARGET_DIR", "CARGO", "NODE_TEST_CONTEXT", SCAN_TOKEN_ENV]) delete clean[k];
  const scanEnv = shimScanEnv(clean, installed);
  const cargo = (cwd, args) => `spawnSync("cargo", ${JSON.stringify(args)}, { cwd: ${JSON.stringify(cwd)}, stdio: "inherit" }).status`;
  const scan = (id, body, extra = {}) => ({ id, class: "A", tool: "node", args: ["-e", `const { spawnSync } = require("node:child" + "_process"); const fs = require("node:fs"); ${body}`], env: {}, root: "content", command: id, ...extra });
  const run = (scans, over = {}) => {
    const plan = { scans, roots: { contentRoot: fx.snapshot, liveRoot: fx.snapshot }, notes: [], deferred: [], inputDigest: "d", classDigests: {} };
    return runScans(plan, { classes: ["A"], env: scanEnv, cargoSentinel: installed.sentinel, cargoLedger: { dir: installed.ledgerDir }, snapshotBroker: fx.sb, platform: process.platform, ...over });
  };
  const row = (result, id) => result.scans.find((r) => r.id === id);
  return { fx, installed, scanEnv, cargo, scan, run, row, a: path.join(fx.snapshot, "crates", "a"), native: path.join(fx.snapshot, "native"), ledger: (id) => readLedger(installed.ledgerDir, scanTokenOf(id)) };
}

test("[7.24] a node unit spawning cargo in two workspaces gets two verified receipts with different argv digests; each request carries the checkout cwd from the gate's map and a fresh nonce", { skip: POSIX }, async (t) => {
  const h = host(t);
  const result = await h.run([h.scan("two", `process.exit(${h.cargo(h.a, ["build", "--locked"])} || ${h.cargo(h.native, ["test", "--locked", "-p", "b"])})`)]);
  const r = h.row(result, "two");
  assert.equal(r.status, "pass", JSON.stringify(r));
  const calls = h.fx.calls();
  assert.deepEqual(calls.map((c) => c.argv), [["build", "--locked"], ["test", "--locked", "-p", "b"]]);
  // primaryCwd comes only from the gate's snapshot-to-checkout map: the client ran in the checkout, never in the snapshot.
  assert.deepEqual(calls.map((c) => c.cwd), [path.join(h.fx.checkout, "crates", "a"), path.join(h.fx.checkout, "native")]);
  assert.notEqual(argvSha256(calls[0].argv), argvSha256(calls[1].argv), "two different argv digests");
  const { entries, problems } = h.ledger("two");
  assert.deepEqual(problems, []);
  assert.deepEqual(entries.map((e) => e.status), ["verified", "verified"]);
  assert.equal(new Set(entries.map((e) => e.nonce)).size, 2, "a fresh nonce per call");
  assert.equal(new Set(entries.map((e) => e.ownershipHandle)).size, 2, "and its own lease sub-handle");
  for (const e of entries) assert.equal(isCallHandleOf(e.ownershipHandle, h.fx.handle), true, e.ownershipHandle);
  assert.equal(new Set(entries.map((e) => e.requestId)).size, 2);
  assert.deepEqual(entries.map((e) => e.outputDir), [h.fx.outputDir, h.fx.outputDir]);
  // The broker never sees the gate's sentinel as an output directory (it pins the output itself) and a carrier is used once and removed.
  assert.deepEqual(calls.map((c) => c.env), [{ CARGO_TARGET_DIR: null, CARGO_BUILD_TARGET_DIR: null }, { CARGO_TARGET_DIR: null, CARGO_BUILD_TARGET_DIR: null }]);
  assert.deepEqual(fs.readdirSync(path.join(h.fx.controlRoot, "snapshot-requests")), [], "no carrier is left behind");
  assert.equal(fs.existsSync(path.join(h.fx.snapshot, "target")), false, "the snapshot holds no target/");
  assert.deepEqual(unclaimedLedgers(h.installed.ledgerDir, new Set([scanTokenOf("two")])), []);
});

test("[7.20] the declared inputs of a carried request are the snapshot's manifest, never the checkout's; a malformed manifest is refused before the request", { skip: POSIX }, async (t) => {
  const h = host(t, { inputs: ["gen/in.txt"] });
  fs.mkdirSync(path.join(h.fx.checkout, ".rightkit"), { recursive: true });
  fs.writeFileSync(path.join(h.fx.checkout, ".rightkit", "compile-inputs.json"), JSON.stringify({ inputs: ["Cargo.toml"] }));
  const r = h.row(await h.run([h.scan("inputs", `process.exit(${h.cargo(h.a, ["check", "--locked"])})`)]), "inputs");
  assert.equal(r.status, "pass", JSON.stringify(r));
  const [entry] = fs.readdirSync(path.join(h.fx.state, "db")).map((f) => JSON.parse(fs.readFileSync(path.join(h.fx.state, "db", f), "utf8")));
  assert.deepEqual(entry.carrier.declaredInputs, { inputs: ["gen/in.txt"] }, "the request carries the snapshot's declared inputs");
  assert.equal(isCallHandleOf(entry.carrier.ownership.handle, h.fx.handle), true, "the request names a sub-handle of the gate's ownership"); assert.deepEqual(Object.keys(entry.carrier.ownership), ["handle"]); assert.deepEqual(entry.carrier.commit, h.fx.sb.commit);
  assert.deepEqual(entry.carrier.snapshot, h.fx.sb.snapshot); assert.equal(entry.carrier.primaryCwd, path.join(h.fx.checkout, "crates", "a"));
});

test("[7.10] a refused wrapper call fails its unit even when the test swallows cargo's exit status; the unit that does not reach cargo passes", { skip: POSIX }, async (t) => {
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
  }
  assert.match(h.row(result, "swallows-unknown").detail, /RK_CARGO_UNBOUND_WORKSPACE/);
  assert.match(h.row(result, "swallows-redirect").detail, /RK_CARGO_REDIRECT/);
  assert.match(h.row(result, "swallows-unbound-command").detail, /RK_CARGO_UNBOUND_COMMAND/);
  assert.equal(h.row(result, "quiet").status, "pass");
  assert.deepEqual(h.fx.calls(), [], "no refused call ever reached the broker");
});

test("[7.24] one completion per (snapshot, lease handle): N carried requests in one stage succeed under distinct sub-handles; reusing a sub-handle is refused by the broker; a receipt naming another sub-handle fails; the bare handle's lease status is ACTIVE while any sub-handle is held", { skip: POSIX }, async (t) => {
  const h = host(t);
  const result = await h.run([h.scan("many", `process.exit(${[0, 1, 2, 3].map((i) => h.cargo(i % 2 ? h.native : h.a, ["build", "--locked", ...(i > 1 ? ["-q"] : [])])).join(" || ")})`)]);
  const r = h.row(result, "many");
  assert.equal(r.status, "pass", JSON.stringify(r));
  const { entries, problems } = h.ledger("many");
  assert.deepEqual(problems, []);
  assert.equal(entries.filter((e) => e.status === "verified").length, 4);
  const handles = entries.map((e) => e.ownershipHandle);
  assert.equal(new Set(handles).size, 4, "four calls, four sub-handles");
  assert.ok(handles.every((x) => isCallHandleOf(x, h.fx.handle)));
  // The gate's own re-verification binds the exact sub-handle each call sent, once each: a ledger that names the same sub-handle twice, the bare handle or another's is a problem.
  assert.deepEqual(await verifyBrokerLedger(entries, h.fx.sb), []);
  const verified = entries.filter((e) => e.status === "verified");
  assert.match((await verifyBrokerLedger([verified[0], { ...verified[1], ownershipHandle: verified[0].ownershipHandle }], h.fx.sb)).join("\n"), /(replay|lease handle|handle is not this gate's)/);
  assert.match((await verifyBrokerLedger([{ ...verified[0], ownershipHandle: h.fx.handle }], h.fx.sb)).join("\n"), /names no sub-handle/);
  assert.match((await verifyBrokerLedger([{ ...verified[0], ownershipHandle: `${h.fx.handle}#not-the-one-sent` }], h.fx.sb)).join("\n"), /request digest|handle is not this gate's/, "a ledger whose sub-handle is not the one sent fails (the request digest binds the carrier's handle, the evidence binds it again)");
  // The broker refuses a second carried request under a handle the snapshot already completed (snapshot.rs:330-343).
  const sendTwice = () => {
    const cwd = h.fx.checkout; const argv = ["build", "--locked"];
    return [1, 2].map(() => {
      const made = writeSnapshotRequest({ controlRoot: h.fx.controlRoot, commit: h.fx.sb.commit, snapshot: h.fx.sb.snapshot, declaredInputs: [], handle: h.fx.handle, callHandle: `${h.fx.handle}#reused`, primaryCwd: cwd, argv, requestIdFile: path.join(h.fx.root, "ref.json") });
      const res = spawnSync(h.fx.client, ["cargo", ...argv], { cwd, env: { ...process.env, ...made.env, RUSTRIGHT_CALLING_GATE: GATE }, encoding: "utf8" });
      fs.rmSync(made.file, { force: true });
      return res;
    });
  };
  const [first, second] = sendTwice();
  assert.equal(first.status, 0, first.stderr);
  assert.notEqual(second.status, 0); assert.match(second.stderr, /prior or unreadable completion/);
  // `snapshot-lease status --ownership <bare>` aggregates its `#` sub-handles: ACTIVE while any is held, REAPED when none.
  const status = (held) => { h.fx.setBehavior({ heldHandles: held }); return JSON.parse(spawnSync(h.fx.client, ["snapshot-lease", "status", "--snapshot", h.fx.snapshot, "--ownership", h.fx.handle], { encoding: "utf8" }).stdout).state; };
  assert.equal(status([`${h.fx.handle}#a-1`, `${h.fx.handle}#a-2`]), "ACTIVE");
  assert.equal(status([`${h.fx.handle}#a-2`]), "ACTIVE");
  assert.equal(status([`${h.fx.handle}x#a-1`, "other#1"]), "REAPED", "another ownership's sub-handles do not count");
  assert.equal(status([]), "REAPED");
});

test("[7.24] sub-handles are per invocation: two sequential calls on the same workspace, two independently launched callers, and the parent's lease is ACTIVE while the last child runs and REAPED only after the final completion with the ledger drained", { skip: POSIX }, async (t) => {
  const h = host(t);
  // Two calls from one workspace (a check, then a test: the same directory, as metadata then check are) draw two different handles.
  const same = h.row(await h.run([h.scan("same-ws", `process.exit(${h.cargo(h.a, ["build", "--locked"])} || ${h.cargo(h.a, ["test", "--locked"])})`)]), "same-ws");
  assert.equal(same.status, "pass", JSON.stringify(same));
  const entries = h.ledger("same-ws").entries.filter((e) => e.status === "verified");
  assert.equal(entries.length, 2);
  assert.notEqual(entries[0].ownershipHandle, entries[1].ownershipHandle);
  for (const e of entries) assert.match(e.ownershipHandle, new RegExp(`^${h.fx.handle}#[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$`), "a random UUID, never an index");
  // Two independently launched processes (wrapper children) never collide: each draws its own id.
  const draw = () => spawnSync(process.execPath, ["--input-type=module", "-e", `import { nextCallHandle } from ${JSON.stringify(new URL("./snapshot-request.mjs", import.meta.url).href)}; process.stdout.write(nextCallHandle(${JSON.stringify(h.fx.handle)}))`], { encoding: "utf8" }).stdout;
  const [x, y] = [draw(), draw()];
  assert.ok(isCallHandleOf(x, h.fx.handle) && isCallHandleOf(y, h.fx.handle)); assert.notEqual(x, y);
  // A receipt for the wrong child is rejected: the ledger names child 2's handle on child 1's receipt.
  const swapped = [{ ...entries[0], ownershipHandle: entries[1].ownershipHandle }];
  assert.match((await verifyBrokerLedger(swapped, h.fx.sb)).join("\n"), /request digest|handle is not this gate's/);
  assert.deepEqual(await verifyBrokerLedger(entries, h.fx.sb), [], "the exact recorded children verify");
  assert.match((await verifyBrokerLedger(entries, h.fx.sb, { seen: { nonces: new Set(), ids: new Set(), handles: new Set([entries[0].ownershipHandle]) } })).join("\n"), /used by two calls/, "a handle used twice is a replay");
  // Lease status for the bare parent: ACTIVE while child 2 runs; REAPED only once none is held, and only then does the gate drain.
  const lease = (held) => { h.fx.setBehavior({ heldHandles: held }); return JSON.parse(spawnSync(h.fx.client, ["snapshot-lease", "status", "--snapshot", h.fx.snapshot, "--ownership", h.fx.handle], { encoding: "utf8" }).stdout).state; };
  assert.equal(lease([entries[1].ownershipHandle]), "ACTIVE", "child 2 still running");
  assert.equal(lease([]), "REAPED", "after the final completion");
  assert.equal(h.ledger("same-ws").problems.length, 0, "the gate ledger is drained: no pending call");
});

test("[7.24] negatives: an unverified receipt, a broker that fails or cannot be asked, no calling gate, and a call the checkout cannot host each fail the scan; nothing launches when the request cannot be built", { skip: POSIX }, async (t) => {
  const h = host(t, { inputs: ["gen/in.txt"] });
  const swallow = (cwd = h.a, args = ["build", "--locked"]) => `${h.cargo(cwd, args)}; process.exit(0)`;
  // The broker authentically signed a failure, tampered evidence or the wrong request: the wrapper's own verification fails the call.
  for (const [tamper, why] of [["failed", /signed FAILED/], ["treeAtExit", /throughout/], ["requestOtherArgv", /request digest/], ["otherHandle", /handle is not this gate's/], ["outputInsideSnapshot", /inside the snapshot/], ["digestWrong", /declaredInputDigests/], ["noFreshness", /freshness/], ["signedByOtherKey", /does not verify/], ["noProvenance", /no provenance/]]) {
    h.fx.setBehavior({ tamper });
    const id = `t-${tamper}`;
    const r = h.row(await h.run([h.scan(id, swallow())]), id);
    assert.equal(r.status, "fail", `${tamper}: ${JSON.stringify(r)}`); assert.equal(r.code, "RK_CARGO_LEDGER", tamper);
    assert.match(r.detail, why, tamper);
    assert.equal(h.ledger(id).entries.at(-1).status, "unverified", tamper);
  }
  // The client refuses the carrier, or the broker cannot be asked for the receipt: unverified, never passed.
  h.fx.setBehavior({ refuse: true });
  let r = h.row(await h.run([h.scan("refused-by-client", swallow())]), "refused-by-client");
  assert.equal(r.code, "RK_CARGO_LEDGER"); assert.match(r.detail, /RK_CARGO_BROKER_UNVERIFIED|no request reference/);
  h.fx.setBehavior({ statusFail: true });
  r = h.row(await h.run([h.scan("unreachable", swallow())]), "unreachable");
  assert.equal(r.code, "RK_CARGO_LEDGER"); assert.match(r.detail, /rustright status .* failed/);
  h.fx.setBehavior({});
  const before = h.fx.calls().length;
  // No calling gate in the environment: the wrapper never forges one.
  r = h.row(await h.run([h.scan("no-gate", `${h.cargo(h.a, ["build", "--locked"])} ; process.exit(0)`, { env: {} })], { env: { ...h.scanEnv, RUSTRIGHT_CALLING_GATE: "" } }), "no-gate");
  assert.equal(r.code, "RK_CARGO_LEDGER"); assert.match(r.detail, /RK_CARGO_BROKER_NO_GATE/);
  // --manifest-path (the client refuses to rewrite the invocation) and a working directory the checkout does not have.
  fs.mkdirSync(path.join(h.a, "src", "only-in-snapshot"), { recursive: true });
  r = h.row(await h.run([h.scan("manifest", swallow(h.a, ["build", "--manifest-path", "Cargo.toml", "--locked"]))]), "manifest");
  assert.match(r.detail, /RK_CARGO_BROKER_MANIFEST_PATH/);
  r = h.row(await h.run([h.scan("cwd", swallow(path.join(h.a, "src", "only-in-snapshot")))]), "cwd");
  assert.match(r.detail, /RK_CARGO_BROKER_COUNTERPART_ABSENT/);
  // A malformed declared-inputs manifest is refused before any request is made.
  fs.writeFileSync(path.join(h.fx.snapshot, ".rightkit", "compile-inputs.json"), "{ not json");
  r = h.row(await h.run([h.scan("manifest-bad", swallow())]), "manifest-bad");
  assert.match(r.detail, /RK_CARGO_BROKER_DECLARED_INPUTS/);
  assert.equal(h.fx.calls().length, before, "none of the refused calls was launched at the broker");
  assert.deepEqual(fs.readdirSync(path.join(h.fx.controlRoot, "snapshot-requests")), [], "no carrier is left behind by any outcome");
});

test("[7.24] the gate verifies every ledgered receipt again itself: a receipt that was fine for the wrapper and is not for the gate fails the scan; a carrier or request used twice is a replay", { skip: POSIX }, async (t) => {
  const h = host(t);
  const behavior = path.join(h.fx.state, "behavior.json");
  // The broker's answer changes after the wrapper accepted it (or the wrapper lied): the gate's own check fails the scan.
  let r = h.row(await h.run([h.scan("lied", `${h.cargo(h.a, ["build", "--locked"])}; fs.writeFileSync(${JSON.stringify(behavior)}, JSON.stringify({ statusTamper: "treeAtExit" })); process.exit(0)`)]), "lied");
  h.fx.setBehavior({});
  assert.equal(r.code, "RK_CARGO_LEDGER"); assert.match(r.detail, /the gate's own verification of the broker's receipt failed \(evidence: .*throughout/);
  assert.equal(h.ledger("lied").entries[0].status, "verified", "the wrapper had accepted it");
  // A ledger line copied with a new call id but the same nonce and request id: the carrier would have been used twice.
  const ledgerFile = path.join(h.installed.ledgerDir, `${scanTokenOf("replay")}.jsonl`);
  r = h.row(await h.run([h.scan("replay", `${h.cargo(h.a, ["build", "--locked"])}; const l = fs.readFileSync(${JSON.stringify(ledgerFile)}, "utf8").trim().split("\\n").map(JSON.parse).at(-1); fs.appendFileSync(${JSON.stringify(ledgerFile)}, JSON.stringify({ ...l, id: "copy" }) + "\\n"); process.exit(0)`)]), "replay");
  assert.equal(r.code, "RK_CARGO_LEDGER"); assert.match(r.detail, /used twice/);
  // The run-wide record spans scans: the same request id in two scans is the same replay.
  const seen = { nonces: new Set(), ids: new Set() };
  const one = h.ledger("replay").entries.find((e) => e.id !== "copy");
  assert.deepEqual(await verifyBrokerLedger([one], h.fx.sb, { seen }), []);
  assert.match((await verifyBrokerLedger([one], h.fx.sb, { seen }))[0], /used twice/);
  // Entries the gate cannot tie to its own checkout or to a verified receipt are refused whatever the ledger claims.
  const forged = { ...one, workspace: h.fx.root, checkoutCwd: h.fx.root };
  assert.match((await verifyBrokerLedger([forged], h.fx.sb))[0], /outside the primary checkout/);
  assert.match((await verifyBrokerLedger([{ ...one, requestId: "no-such-request" }], h.fx.sb))[0], /status no-such-request|rustright status|failed/);
  assert.match((await verifyBrokerLedger([{ ...one, nonce: undefined }], h.fx.sb))[0], /record is incomplete/);
});

test("[7.24] a cargo that bypasses the wrapper by absolute path writes into the per-scan sentinel and fails the scan RK_CARGO_UNBOUND; a scan that clears its scan token files its calls where no scan owns them", { skip: POSIX }, async (t) => {
  const h = host(t);
  const bypass = path.join(h.fx.root, "toolchain", "cargo");
  fs.mkdirSync(path.dirname(bypass), { recursive: true });
  fs.writeFileSync(bypass, '#!/bin/sh\nmkdir -p "$CARGO_TARGET_DIR/debug" && echo built > "$CARGO_TARGET_DIR/debug/out-$$"\nexit 0\n', { mode: 0o755 });
  assert.ok(h.installed.sentinel, "broker mode always has a sentinel: there is no checkout target to inherit");
  assert.equal(h.scanEnv.CARGO_TARGET_DIR, undefined, "and no scan-wide CARGO_TARGET_DIR");
  const abs = (cwd) => `spawnSync(${JSON.stringify(bypass)}, ["build"], { cwd: ${JSON.stringify(cwd)}, stdio: "inherit" }).status`;
  let result = await h.run([h.scan("bypass", `process.exit(${abs(h.a)})`), h.scan("wrapped", `process.exit(${h.cargo(h.a, ["build", "--locked"])})`), h.scan("own-target", `process.exit(${abs(h.a)})`, { env: { CARGO_TARGET_DIR: path.join(h.fx.root, "declared") } })]);
  assert.equal(h.row(result, "bypass").status, "fail"); assert.equal(h.row(result, "bypass").code, "RK_CARGO_UNBOUND");
  assert.match(h.row(result, "bypass").detail, /bypassed the gate's wrapper wrote debug into the unbound sentinel/);
  assert.equal(h.row(result, "wrapped").status, "pass");
  assert.equal(h.row(result, "own-target").status, "pass", "a scan that binds its own CARGO_TARGET_DIR gets no sentinel");
  // The code under test removes the scan token from the cargo's environment: the call is recorded, and the run fails on it.
  const env = JSON.stringify(Object.fromEntries(Object.entries(h.scanEnv).filter(([k]) => k !== SCAN_TOKEN_ENV)));
  result = await h.run([h.scan("clears-token", `spawnSync("cargo", ["build", "--locked"], { cwd: ${JSON.stringify(h.a)}, env: ${env}, stdio: "inherit" }); process.exit(0)`)]);
  const ledgerRow = h.row(result, "cargo-ledger");
  assert.ok(ledgerRow, JSON.stringify(result.scans.map((x) => [x.id, x.status])));
  assert.equal(ledgerRow.status, "fail"); assert.equal(ledgerRow.code, "RK_CARGO_LEDGER"); assert.match(ledgerRow.detail, /unscoped/);
  assert.equal(result.ok, false);
});

test("[7.10] the ledger itself: unfinished and unreadable records fail, a later record replaces an earlier one, and a call that cannot be recorded does not run", (t) => {
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

test("[7.24] the broker wrapper resolves workspaces from the files alone (no cargo outside the broker) and passes nothing else through", { skip: POSIX }, (t) => {
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

// ---- row 7.26: the snapshot is not re-synced while a broker job may still read it ----
test("[7.26] a dead owner's record is reclaimed only on the broker's REAPED for exactly its handle and snapshot; ACTIVE, UNKNOWN, malformed, stale, unreachable and another handle or snapshot hold it", { skip: POSIX }, async (t) => {
  const { OWNERSHIP_SCHEMA, acquireBuildOwnership, brokerLeaseState } = await import("./build-ownership.mjs");
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-lease-"))); t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  // A fake `rustright snapshot-lease status --snapshot S --ownership H` that answers per behavior file for the pair it is asked about.
  const answers = path.join(root, "answers.json"); const asked = path.join(root, "asked.log");
  const client = path.join(root, "rustright");
  fs.writeFileSync(client, `#!/bin/sh\necho "$@" >> '${asked}'\nexec ${JSON.stringify(process.execPath)} -e 'const a = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")); const args = process.argv.slice(2); const key = args[3] + "|" + args[5]; if (a.fail) process.exit(1); let v = a.byPair?.[key] ?? a.default; if (v === undefined) process.exit(1); if (v && typeof v === "object" && !a.noEcho) v = { snapshot: args[3], ownership: args[5], ...v }; console.log(typeof v === "string" ? v : JSON.stringify(v));' ${JSON.stringify(answers)} "$@"\n`, { mode: 0o755 });
  const status = (state, extra = {}) => ({ schema: "rustright.snapshot-lease-status.v1", state, ...extra });
  const snapshot = path.join(root, "snapshots", "k"); const handle = "dead-owner-handle";
  const ask = (over = {}) => brokerLeaseState({ snapshot, handle, env: {}, platform: "darwin", client, ...over });
  const set = (a) => fs.writeFileSync(answers, JSON.stringify(a));
  // The states the gate accepts: only a well-formed REAPED for the asked pair is REAPED.
  set({ byPair: { [`${snapshot}|${handle}`]: status("REAPED") } }); assert.equal(ask().state, "REAPED");
  assert.equal(ask({ snapshot: path.join(root, "snapshots", "other") }).state, "UNKNOWN", "the broker knows nothing of another snapshot: held");
  assert.equal(ask({ handle: "another-handle" }).state, "UNKNOWN", "or of another handle: held");
  assert.match(fs.readFileSync(asked, "utf8"), new RegExp(`snapshot-lease status --snapshot ${snapshot} --ownership ${handle}`));
  for (const [name, a, state] of [["ACTIVE", { default: status("ACTIVE") }, "ACTIVE"], ["UNKNOWN", { default: status("UNKNOWN") }, "UNKNOWN"], ["malformed", { default: "{ nope" }, "UNKNOWN"], ["stale schema", { default: { schema: "rustright.snapshot-lease-status.v0", state: "REAPED" } }, "UNKNOWN"],
    ["unknown state", { default: status("CLEAR") }, "UNKNOWN"], ["unreachable", { fail: true }, "UNKNOWN"],
    ["REAPED with no echo of the pair", { default: status("REAPED"), noEcho: true }, "UNKNOWN"], ["REAPED echoing another snapshot", { default: status("REAPED", { snapshot: "/elsewhere" }) }, "UNKNOWN"], ["REAPED echoing another handle", { default: status("REAPED", { ownership: "another" }) }, "UNKNOWN"]]) {
    set(a); assert.equal(ask().state, state, name);
  }
  assert.equal(ask({ client: null }).state, "UNKNOWN", "no RustRight client: held");
  // And through the real reclaim: a dead owner whose broker work may exist (broker.submitted) is held unless REAPED.
  const file = path.join(root, "own", "owner.json"); fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 }); fs.chmodSync(path.dirname(file), 0o700);
  const NOW = Date.now();
  const dead = { schema: OWNERSHIP_SCHEMA, handle, host: os.hostname(), platform: "darwin", pid: 999_999, startedMs: NOW - 120_000, containment: { kind: "group", group: 777 }, ancestors: [{ pid: 5000, startedMs: NOW - 500_000 }],
    stage: "compile", snapshot, commit: "c".repeat(40), acquiredAt: new Date(NOW - 100_000).toISOString(), broker: { submitted: true } };
  // The host rebooted after the record was written, so every process proof is moot and the only question left is the broker's lease.
  const probe = { platform: "darwin", identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW - 60_000, group: 4242 } : { alive: false, startedMs: null, group: null }), bootTime: () => NOW,
    groupSnapshot: () => { throw new Error("must not be asked"); }, tokenHolders: () => null, snapshotUsers: () => null, ancestors: () => [] };
  const tryAcquire = (lease) => () => acquireBuildOwnership({ file, platform: "darwin", probe, lease, env: {}, exportEnv: false, stage: "compile" });
  for (const [name, a, ok] of [["REAPED for the exact pair", { byPair: { [`${snapshot}|${handle}`]: status("REAPED") } }, true], ["REAPED for another snapshot", { byPair: { [`${snapshot}-x|${handle}`]: status("REAPED") } }, false],
    ["REAPED for another handle", { byPair: { [`${snapshot}|other`]: status("REAPED") } }, false], ["ACTIVE", { default: status("ACTIVE") }, false], ["UNKNOWN", { default: status("UNKNOWN") }, false], ["unreachable", { fail: true }, false], ["REAPED with no echo", { default: status("REAPED"), noEcho: true }, false]]) {
    fs.writeFileSync(file, JSON.stringify(dead)); set(a);
    const go = tryAcquire((q) => brokerLeaseState({ ...q, env: {}, platform: "darwin", client }));
    if (ok) { const own = go(); assert.match(own.reclaimed.proofs.join("; "), /lease for dead-owner-handle is REAPED/, name); own.release(); } else assert.throws(go, /broker work may exist under this handle and its lease is (ACTIVE|UNKNOWN)/, name);
  }
});

// ---- row 7.20: `deps.snapshotBroker` is wired in production on broker hosts, in the gate's snapshot-compile region ----
import { main } from "./bin/rightkit-gate.mjs";
import { enrollBrokerPins } from "./broker-pins.mjs";

test("[7.20] the gate builds deps.snapshotBroker on a broker host (enrolled pins, the snapshot's declared inputs, the ownership handle), runs scans in the checkout with the broker-mode wrapper, and refuses before anything compiles without an enrollment or with a broker it cannot reach", { skip: POSIX }, async (t) => {
  const run = async ({ enroll = true, executor = { kind: "broker", cargo: ["rustright", "cargo"] } } = {}) => {
    const fx = makeFakeBroker(t, { inputs: ["gen/in.txt"], readOnlySnapshots: true });
    const repo = fx.checkout;
    // The commit tracks its declaration and the input it names, so the snapshot (the commit's bytes) carries both.
    fs.mkdirSync(path.join(repo, ".rightkit")); fs.writeFileSync(path.join(repo, ".rightkit", "compile-inputs.json"), JSON.stringify({ inputs: ["gen/in.txt"] }));
    fs.mkdirSync(path.join(repo, "gen")); fs.writeFileSync(path.join(repo, "gen", "in.txt"), "declared input\n");
    const g = (...a) => spawnSync("git", a, { cwd: repo, encoding: "utf8" });
    g("init", "-q", "-b", "main"); g("add", "-A"); g("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "c");
    const sha = g("rev-parse", "HEAD").stdout.trim();
    // The snapshot is the commit's bytes (plus the declared input it digests in place), outside the checkout.
    const snap = path.join(fx.root, "gate-snapshots", "gate-snapshot"); fs.mkdirSync(snap, { recursive: true });
    // The gate reads the snapshot's metadata as a carried read-only request (rightkit#1311): the fixture broker serves it from this snapshot.
    fx.rebase({ snapshotsRoot: path.dirname(snap) });
    for (const f of g("ls-files", "-z").stdout.split("\0").filter(Boolean)) { fs.mkdirSync(path.dirname(path.join(snap, f)), { recursive: true }); fs.copyFileSync(path.join(repo, f), path.join(snap, f)); }
    if (enroll) enrollBrokerPins({ dataRoot: fx.dataRoot, platform: "darwin", ...fx.pins });
    const env = { ...process.env, RIGHTKIT_DATA_ROOT: fx.dataRoot, RIGHTKIT_CONTROL_ROOT: fx.controlRoot, XDG_STATE_HOME: fx.root, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs" };
    for (const k of ["GITHUB_ACTIONS", "RIGHTKIT_BUILD_OWNERSHIP", "RIGHTKIT_BUILD_OWNERSHIP_FILE", "CARGO_TARGET_DIR"]) delete env[k];
    const spawn = (c, a, opts) => {
      if (c === "git" || c === fx.client) return spawnSync(c, a, { ...opts, encoding: "utf8" });
      if (c === "which" && a[0] === "rustright") return { status: 0, stdout: `${fx.client}\n`, stderr: "" };
      if (c === "which" && a[0] === "cargo") return { status: 0, stdout: "/opt/toolchain/bin/cargo\n", stderr: "" };
      if (c === "rustright" && a.includes("metadata")) return { status: 0, stdout: JSON.stringify({ workspace_root: opts.cwd, target_directory: "/never", packages: [{ name: "a", manifest_path: path.join(opts.cwd, "crates", "a", "Cargo.toml") }] }), stderr: "" };
      return { status: 1, stdout: "", stderr: `not in this fixture: ${c} ${a.join(" ")}` };
    };
    const seen = []; const out = [];
    const ownership = { handle: fx.handle, markBrokerSubmit: subHandleStub(fx.handle, (h) => fx.submits.push(h)) };
    const code = await main(["--all", "--commit", sha, "--snapshot", snap], { cwd: repo, platform: "darwin", env, spawn, out: (m) => out.push(m), err: (m) => out.push(m), appCheck: async () => 0, buildOwnership: ownership, readStdin: () => "", resolveExecutor: () => executor,
      runScans: async (plan, options) => {
        const bin = options.env?.PATH?.split(path.delimiter)[0];
        seen.push({ plan, options, map: bin ? JSON.parse(fs.readFileSync(path.join(path.dirname(bin), "cargo-target-map.json"), "utf8")) : null, wrapperDir: bin ? path.dirname(bin) : null });
        return { scans: [], notes: [], cacheHits: [] };
      } });
    return { code, seen, out: out.join("\n"), fx, repo, snap, sha };
  };
  const ok = await run();
  assert.equal(ok.code, 0, ok.out);
  const [call] = ok.seen; const sb = call.options.snapshotBroker;
  assert.ok(sb, "runScans received deps.snapshotBroker");
  assert.equal(sb.pinsSource, "enrolled"); assert.deepEqual(sb.pins, ok.fx.pins); assert.equal(sb.handle, ok.fx.handle);
  assert.deepEqual(sb.declaredInputs, ["gen/in.txt"], "the declared inputs come from the snapshot's manifest");
  assert.equal(sb.snapshot.path, ok.snap); assert.equal(sb.commit.sha, ok.sha); assert.equal(sb.snapshot.expectedTree, sb.commit.tree);
  assert.equal(sb.checkout, ok.repo); assert.deepEqual(sb.outputRoots, [path.join(ok.fx.dataRoot, "rightkit-build-control", "ws")]); assert.equal(sb.client, ok.fx.client);
  assert.ok(ok.fx.submits.length >= 1, "the ownership recorded the broker submit before the gate's first carried request (discovery) and before the wrapper could carry one");
  // The wrapper is the broker's: the gate's own snapshot-to-checkout map, the sentinel, the ledger, and no scan-wide target directory.
  assert.equal(call.map.mode, "broker"); assert.deepEqual(call.map.workspaces, { [ok.snap]: ok.repo }); assert.equal(call.options.env.CARGO_TARGET_DIR, undefined);
  assert.ok(call.options.cargoSentinel); assert.equal(call.options.cargoLedger.dir, path.join(call.wrapperDir, "ledger")); assert.deepEqual(call.map.broker.pins, ok.fx.pins);
  assert.equal(call.plan.roots.cargoLiveRoot, ok.repo, "cargo scans are carried requests that run from the primary checkout");
  const cargoScans = call.plan.scans.filter((s) => s.tool === "rustright"); assert.ok(cargoScans.length > 0, "cargo scans go through the client");
  assert.ok(cargoScans.every((s) => s.root === (s.class === "B" ? "cargoLive" : "live")), "class B compiles start in the checkout; class A cargo scans read the snapshot");
  assert.match(ok.out, /broker snapshot compile: pins [0-9a-f]{12} from enrolled, 1 declared input\(s\) read from the snapshot/);
  assert.doesNotMatch(ok.out, /direct receipt \{/, "a broker host writes no receipt that says executor direct");
  assert.equal(fs.existsSync(call.wrapperDir), false, "the wrapper directory is removed after the compile");
  // An app repository with no enrollment compiles nothing through a broker.
  const none = await run({ enroll: false });
  assert.equal(none.code, 1, none.out); assert.match(none.out, /BLOCKED: RK_BROKER_PINS_MISSING: no broker enrollment/); assert.equal(none.seen.length, 0, "nothing was planned or compiled");
  // A broker-managed host whose broker cannot be reached is refused, never compiled around.
  const refused = await run({ executor: { kind: "refuse", cargo: null, problem: "the broker is not reachable" } });
  assert.equal(refused.code, 1); assert.match(refused.out, /BLOCKED: the broker is not reachable; nothing ran/); assert.equal(refused.seen.length, 0);
});

test("[RR status envelope] the receipt is read from `{type:\"STATUS\", state, receipt}`; a null receipt (job still live) after the cargo process exited is a clear not-ready error and never a poll; a bare receipt with no envelope is refused", { skip: POSIX }, async (t) => {
  const h = host(t);
  const unit = (id) => h.scan(id, `${h.cargo(h.a, ["build", "--locked"])}; process.exit(0)`);
  h.fx.setBehavior({ statusLive: true });
  let r = h.row(await h.run([unit("live")]), "live");
  assert.equal(r.status, "fail"); assert.equal(r.code, "RK_CARGO_LEDGER"); assert.match(r.detail, /receipt-not-ready|no receipt yet, although its cargo process exited; the gate does not poll/);
  assert.equal(h.ledger("live").entries[0].status, "unverified");
  h.fx.setBehavior({ statusBare: true });
  r = h.row(await h.run([unit("bare")]), "bare");
  assert.equal(r.code, "RK_CARGO_LEDGER"); assert.match(r.detail, /no receipt field \(not a STATUS envelope\)/);
  h.fx.setBehavior({});
  assert.equal(h.row(await h.run([unit("envelope")]), "envelope").status, "pass", "the full envelope of a terminal job verifies");
  const entry = h.ledger("envelope").entries[0];
  assert.deepEqual(entry.attribution, { available: true, rows: [] }); assert.match(entry.toolchain, /^stable\|cargo:1{64}\|rustc:2{64}$/, "attribution and toolchain are recorded in the ledger");
});

test("[7.07 M/D, gate half] a generator that writes an ignored path inside the snapshot passes; one that rewrites a tracked file is refused at the after-exit bind with the app owner's fix named, and its result is discarded (the broker's own bind is item 2: dev-broker run BLOCKED-on-item-4)", { skip: POSIX }, async (t) => {
  const h = host(t);
  const unit = (id) => h.scan(id, `process.exit(${h.cargo(h.a, ["build", "--locked"])})`);
  // Ignored path written: the broker's tree at exit is the commit's tree, so the signed evidence is clean and the unit passes.
  h.fx.setBehavior({});
  assert.equal(h.row(await h.run([unit("ignored-output")]), "ignored-output").status, "pass");
  // A tracked file rewritten (or one added or removed) during the build: the broker signs the changed tree at exit and the unit fails.
  h.fx.setBehavior({ tamper: "treeAtExit" });
  const r = h.row(await h.run([unit("tracked-rewrite")]), "tracked-rewrite");
  assert.equal(r.status, "fail"); assert.ok(["EXIT", "RK_CARGO_LEDGER"].includes(r.code), r.code);
  assert.match(r.detail, /at exit, not [0-9a-f]{40} throughout.*the app owner's defect to fix/);
  assert.equal(h.ledger("tracked-rewrite").entries[0].status, "unverified", "the result is not accepted");
});

test("[7.24] broker-mode decisions: the workspace's counterpart in the checkout, no output variable, and --manifest-path, --target-dir, redirects and unbound commands refused with their codes", (t) => {
  const fx = makeFakeBroker(t);
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

test("[M8-M11] broker-mode wrapper: metadata, deny, audit and fmt are carried only in the broker's exact shapes; aliases are expanded before classification and sending; help, --list and every other uncarried call inside the snapshot are refused", (t) => {
  const fx = makeFakeBroker(t);
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

test("[7.20] production hands the door the carrier seam: a broker host takes the snapshot route with it and is refused RK_SNAPSHOT_CARRIER_NOT_WIRED without it; a host whose broker is unreachable is refused whichever", async () => {
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

test("[B8] a carried call under a release-bound stage derives its nonce from the stage's, its directory (relative to the primary checkout) and its argv; otherwise it is fresh; the stage nonce never reaches the client", { skip: POSIX }, async (t) => {
  const fx = makeFakeBroker(t, {});
  const records = [];
  const snapshotRoot = fs.realpathSync.native(fx.snapshot);
  const stageNonce = "ab".repeat(32);
  const call = (env, { cwd = path.join(snapshotRoot, "crates", "a"), words = ["build", "--locked"] } = {}) => {
    const before = fx.calls().length;
    const r = carryBrokerCargo({ words, cwd, env: { PATH: process.env.PATH, RUSTRIGHT_CALLING_GATE: GATE, ...fx.ownershipEnv, ...env }, section: brokerSection(fx.sb), workspace: snapshotRoot, checkoutWorkspace: fx.checkout, record: (e) => records.push(e), err: () => {}, stdio: "capture" });
    assert.equal(r.outcome.status, "verified", JSON.stringify(r.outcome));
    const sent = fx.calls()[before];
    return { nonce: records.filter((e) => e.status === "pending").at(-1).nonce, sent };
  };
  const inCrate = call({ RIGHTKIT_ATTEST_STAGE_NONCE: stageNonce });
  assert.equal(inCrate.nonce, carriedCallNonce({ stageNonce, cwd: "crates/a", argv: ["build", "--locked"] }));
  assert.equal(inCrate.sent.stageNonceSeen, false, "stripped from the client's environment");
  // Another directory or another argv is another nonce; a stage runs each command once (the broker refuses a nonce used twice, so a repeated identical call in one stage is refused).
  assert.equal(call({ RIGHTKIT_ATTEST_STAGE_NONCE: stageNonce }, { cwd: snapshotRoot }).nonce, carriedCallNonce({ stageNonce, cwd: ".", argv: ["build", "--locked"] }));
  assert.equal(call({ RIGHTKIT_ATTEST_STAGE_NONCE: stageNonce }, { words: ["check"] }).nonce, carriedCallNonce({ stageNonce, cwd: "crates/a", argv: ["check", "--locked"] }), "another command, another nonce");
  // Without a stage nonce, or with a malformed one, the nonce is fresh and different every call.
  const fresh = [call({}), call({}), call({ RIGHTKIT_ATTEST_STAGE_NONCE: "not-hex" }), call({ RIGHTKIT_ATTEST_STAGE_NONCE: "AB".repeat(32) })].map((c) => c.nonce);
  assert.equal(new Set(fresh).size, 4);
  for (const n of fresh) { assert.match(n, /^[0-9a-f]{64}$/); assert.notEqual(n, inCrate.nonce); }
  // The receipt the broker signed carries the nonce the call sent, so the hosted admission can recompute it.
  assert.equal((await fx.sb.receiptOf(inCrate.sent.id)).provenance.payload.nonce, inCrate.nonce);
});

test("[submit wiring] createSnapshotBroker with the REAL build ownership passes each carried call's sub-handle through: a carried metadata call and a carried cargo call both succeed and the owner record lists both sub-handles", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t, { readOnlySnapshots: true });
  const file = path.join(fx.root, "real-ownership", "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(path.dirname(file)), { recursive: true, mode: 0o700 });
  const NOW = Date.now();
  const probe = { platform: "darwin", identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW - 60_000, group: 4242 } : { alive: false, startedMs: null, group: null }), bootTime: () => NOW - 86_400_000, ancestors: () => [], groupSnapshot: () => ({ members: [], byPid: new Map(), own: new Set([process.pid]) }), tokenHolders: () => null, snapshotUsers: () => null };
  const own = acquireBuildOwnership({ file, platform: "darwin", probe, lease: () => { throw new Error("no lease query expected"); }, env: {}, exportEnv: false, stage: "compile", snapshot: fx.snapshot });
  t.after(() => own.release());
  const env = { RIGHTKIT_DATA_ROOT: fx.dataRoot, RIGHTKIT_CONTROL_ROOT: fx.controlRoot, XDG_STATE_HOME: fx.root };
  const sb = createSnapshotBroker({ env, platform: process.platform, checkout: fx.checkout, commit: { sha: fx.sb.commit.sha, tree: fx.sb.commit.tree }, snapshot: { path: fx.snapshot, expectedTree: fx.sb.snapshot.expectedTree }, ownership: own, client: fx.client, dataRoot: fx.dataRoot, loadPins: () => ({ ...fx.pins, source: "test" }) });
  const records = [];
  const snapshotRoot = fs.realpathSync.native(fx.snapshot);
  const call = (words) => carryBrokerCargo({ words, cwd: path.join(snapshotRoot, "crates", "a"), env: { PATH: process.env.PATH, RUSTRIGHT_CALLING_GATE: GATE }, section: brokerSection(sb), workspace: snapshotRoot, checkoutWorkspace: fx.checkout, record: (e) => records.push(e), err: () => {}, stdio: "capture" });
  const metadata = call(["metadata", "--no-deps", "--format-version", "1"]);
  assert.equal(metadata.outcome.status, "verified", JSON.stringify(metadata.outcome));
  const build = call(["build", "--locked"]);
  assert.equal(build.outcome.status, "verified", JSON.stringify(build.outcome));
  const sent = records.filter((r) => r.status === "pending").map((r) => r.ownershipHandle);
  assert.equal(sent.length, 2);
  for (const sub of sent) assert.ok(isCallHandleOf(sub, own.handle), `${sub} is a sub-handle of ${own.handle}`);
  assert.equal(new Set(sent).size, 2, "each call has its own sub-handle");
  const owner = JSON.parse(fs.readFileSync(file, "utf8"));
  assert.equal(owner.broker.submitted, true);
  assert.deepEqual(owner.broker.handles, sent, "the owner record lists both sub-handles, in send order");
});

test("[submit wiring] every markBrokerSubmit fake in the tests receives the call's `<handle>#<id>` sub-handle", () => {
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

test("[GB-16] the checkout counterpart of the snapshot cwd is computed lexically: a directory the newer checkout no longer has is a typed broker-side refusal naming it (nothing sent, nothing marked), an existing one maps by snapshot-relative path, a link out of the checkout is refused", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t, {});
  const snapshotRoot = fs.realpathSync.native(fx.snapshot);
  const records = [];
  const call = (rel, words = ["build", "--locked"]) => carryBrokerCargo({ words, cwd: path.join(snapshotRoot, rel), env: { PATH: process.env.PATH, RUSTRIGHT_CALLING_GATE: GATE, ...fx.ownershipEnv }, section: brokerSection(fx.sb), workspace: snapshotRoot, checkoutWorkspace: fx.checkout, record: (e) => records.push(e), err: () => {}, stdio: "capture" });
  // Present: maps by the snapshot-relative path.
  const ok = call("crates/a");
  assert.equal(ok.outcome.status, "verified", JSON.stringify(ok.outcome));
  assert.equal(records.find((r) => r.status === "pending").checkoutCwd, path.join(fx.checkout, "crates", "a"));
  const calls = fx.calls().length; const marks = fx.submits.length;
  // The selected (older) commit has crates/a; the newer checkout deleted it: refused with a typed, named, broker-side reason, before any mark or send.
  fs.rmSync(path.join(fx.checkout, "crates", "a"), { recursive: true, force: true });
  const gone = call("crates/a");
  assert.equal(gone.outcome.status, "refused");
  assert.equal(gone.outcome.code, "RK_CARGO_BROKER_COUNTERPART_ABSENT");
  assert.match(gone.outcome.message, /crates\/a has no counterpart .* in the primary checkout/); assert.match(gone.outcome.message, /broker-side/); assert.match(gone.outcome.message, /shims\.rs:241/);
  assert.equal(fx.calls().length, calls, "nothing was sent to the broker"); assert.equal(fx.submits.length, marks, "nothing was marked in the ownership");
  // A counterpart that is a link leaving the checkout is still refused.
  const outside = fs.mkdtempSync(path.join(fx.root, "outside-")); fs.mkdirSync(path.join(outside, "x"));
  fs.symlinkSync(path.join(outside, "x"), path.join(fx.checkout, "crates", "a"));
  const escaped = call("crates/a");
  assert.equal(escaped.outcome.code, "RK_CARGO_BROKER_COUNTERPART_ESCAPE"); assert.match(escaped.outcome.message, /outside the primary checkout/);
  assert.equal(fx.calls().length, calls); assert.equal(fx.submits.length, marks);
});

test("[GB-16] the output pin reads the selected commit's snapshot, not the checkout: a directory the newer checkout deleted, a dirty or added manifest there, change nothing; an absolute target outside the snapshot stands; several workspaces still refuse", async (t) => {
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
  assert.deepEqual(one, { dir: path.join(root, "a", "target") }, "the snapshot's target maps to the same relative path under the checkout, which does not have a/");
  assert.ok(asked.every((q) => q.file.startsWith(snapshot) && q.cwd.startsWith(snapshot)), "metadata was asked in the snapshot only");
  assert.deepEqual(run(["b/Cargo.toml"], (file) => ({ workspace_root: path.dirname(file), target_directory: path.join(path.dirname(file), "target") })), { dir: path.join(root, "b", "target") }, "a dirty manifest in the checkout is not read");
  assert.deepEqual(run(["Cargo.toml"], () => ({ workspace_root: snapshot, target_directory: path.join(base, "shared-target") })), { dir: path.join(base, "shared-target") }, "an absolute target outside the snapshot stands");
  assert.deepEqual(run([], () => assert.fail("no manifest")), { dir: null });
  const two = run(["a/Cargo.toml", "b/Cargo.toml"], (file) => ({ workspace_root: path.dirname(file), target_directory: path.join(path.dirname(file), "target") }));
  assert.match(two.refusal, /holds 2 Cargo workspaces \(a, b\)/, "workspaces are named by their checkout-relative path");
  assert.match(run(["a/Cargo.toml"], () => ({})).refusal, /metadata in the snapshot reported no absolute target_directory for a\/Cargo\.toml/);
});
