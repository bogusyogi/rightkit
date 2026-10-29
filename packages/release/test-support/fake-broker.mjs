// Test support for the broker snapshot carrier (item 7): a fixture host, receipts signed in-test with a real P-256 key the way
// rustright-protocol attestation.rs signs them, and a fake `rustright` client that applies the reader's carrier rules and serves
// `rustright status <id>`. No broker, no cargo. NOT the dev or installed broker: native Mac and Dell runs against RustRight's
// item-4 dev install are separate evidence and are reported BLOCKED until that install lands.
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { canonicalJson, signedBytes } from "../provenance-verify.mjs";
import { argvSha256, declaredInputDigestsOf, observedEnvOf, readDeclaredInputs, requestDigestOf, snapshotRequestCore } from "../snapshot-request.mjs";

export const COMMIT = "c".repeat(40);
export const TREE = "e".repeat(40);
export const HANDLE = "own-7";
/** `<toolchain>|cargo:<sha256>|rustc:<sha256>` */
export const TOOLCHAIN = `stable|cargo:${"1".repeat(64)}|rustc:${"2".repeat(64)}`;
const sha = (s) => crypto.createHash("sha256").update(s).digest("hex");

export function keyPair() {
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ec", { namedCurve: "P-256" });
  const jwk = publicKey.export({ format: "jwk" });
  const sec1 = Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x, "base64url"), Buffer.from(jwk.y, "base64url")]).toString("hex");
  return { privateKey, pins: { publicKey: sec1, keyId: sha(Buffer.from(sec1, "hex")), host: "host-1", brokerDigest: "b".repeat(64) } };
}

/** Receipt tampers, by name: each returns what a hostile or faulty broker could do. `payload` is signed after; `receipt` edits are applied after signing. */
export const TAMPERS = {
  none: {},
  treeAtExit: { evidence: (e) => { e.snapshot.treeAtExit = "f".repeat(40); } },
  treeAtStart: { evidence: (e) => { e.snapshot.treeAtStart = "f".repeat(40); } },
  otherHandle: { evidence: (e) => { e.ownership = { handle: "someone-else" }; } },
  otherCommit: { evidence: (e) => { e.commit = { sha: "d".repeat(40), tree: TREE }; } },
  otherSnapshotPath: { evidence: (e, fx) => { e.snapshot.path = path.join(fx.dataRoot, "snapshots", "ffffffffffffffff"); } },
  otherRequestId: { evidence: (e) => { e.requestId = "33333333-3333-4333-8333-333333333333"; } },
  noSnapshotEvidence: { payload: (p) => { delete p.snapshotEvidence; } },
  failed: { payload: (p) => { p.result = { status: "FAILED", exitCode: 0, signal: null, reap: { reaped: true } }; } },
  noResult: { payload: (p) => { delete p.result; } },
  outputInsideSnapshot: { evidence: (e, fx) => { e.outputDir = path.join(fx.snapshot, "target"); } },
  outputInsideCheckout: { evidence: (e, fx) => { e.outputDir = path.join(fx.checkout, "target"); } },
  outputOutsideWs: { evidence: (e, fx) => { e.outputDir = path.join(fx.root, "elsewhere", "target"); } },
  outputNotTarget: { evidence: (e, fx) => { e.outputDir = path.join(fx.wsRoot, "0123456789ab", "g", "002", "debug"); } },
  outputRelative: { evidence: (e) => { e.outputDir = "target"; } },
  digestWrong: { evidence: (e) => { e.declaredInputDigests = [{ path: "gen/in.txt", sha256: "0".repeat(64) }]; } },
  digestsDropped: { evidence: (e) => { e.declaredInputDigests = []; } },
  noFreshness: { payload: (p) => { delete p.stageEvidence.freshness; }, receipt: (r) => { delete r.freshness; } },
  attributionUnavailable: { stage: (g) => { g.attribution = { available: false, reason: "cargo-json-message-format-not-requested" }; } },
  attributionNoReason: { stage: (g) => { g.attribution = { available: false, reason: "" }; } },
  attributionNotObject: { stage: (g) => { g.attribution = "none"; } },
  toolchainObject: { stage: (g) => { g.toolchain = { rustc: "1.98.0" }; } },
  toolchainNoHashes: { stage: (g) => { g.toolchain = "stable|cargo:abc|rustc:def"; } },
  diagnosticsNull: { top: (x) => { x.diagnosticsPath = null; } },
  timingsMissingKey: { top: (x) => { delete x.timingsMs.queue; } },
  timingsNulls: { top: (x) => { x.timingsMs = { admission: null, queue: null, bind: 1, cargo: 2, afterExitBind: null, diagnostics: null }; } },
  /** Receipt copy of a top-level field that differs from the signed one. */
  verdictCopyDiffers: { receipt: (r) => { r.verdict = { status: "failed" }; } },
  exitNullNoSignal: { top: (x) => { x.exit = null; } },
  noAttribution: { payload: (p) => { delete p.stageEvidence.attribution; }, receipt: (r) => { delete r.attribution; } },
  noToolchain: { payload: (p) => { delete p.stageEvidence.toolchain; }, receipt: (r) => { delete r.toolchain; } },
  /** What a broker emits for a non-compile command: freshness only. */
  freshnessOnly: { payload: (p) => { delete p.stageEvidence.attribution; delete p.stageEvidence.toolchain; }, receipt: (r) => { delete r.attribution; delete r.toolchain; } },
  attributionReceiptOnly: { payload: (p) => { delete p.stageEvidence.attribution; } },
  attributionSignedOnly: { receipt: (r) => { delete r.attribution; } },
  freshnessReceiptOnly: { receipt: (r) => { delete r.freshness; } },
  freshnessSignedOnly: { payload: (p) => { delete p.stageEvidence.freshness; } },
  freshnessDiffers: { receipt: (r) => { r.freshness = { unitsAtStart: 9, fresh: 9, notFresh: 0, invalidationReasons: [] }; } },
  noStageEvidence: { payload: (p) => { delete p.stageEvidence; } },
  noVerdict: { receipt: (r) => { delete r.verdict; } },
  noTimings: { receipt: (r) => { delete r.timingsMs; } },
  noExit: { receipt: (r) => { delete r.exit; } },
  noDiagnostics: { receipt: (r) => { delete r.diagnosticsPath; } },
  noRefreshed: { receipt: (r) => { delete r.refreshed; } },
  noBrokerVersion: { evidence: (e) => { delete e.brokerVersion; } },
  noExecutor: { evidence: (e) => { delete e.executor; } },
  noProvenance: { receipt: (r) => { delete r.provenance; } },
  otherNonce: { payload: (p) => { p.nonce = "9".repeat(64); } },
  otherJob: { payload: (p) => { p.jobId = "22222222-2222-4222-8222-222222222222"; } },
  otherHost: { payload: (p) => { p.host = "host-2"; } },
  otherBrokerDigest: { payload: (p) => { p.broker = { brokerDigest: "a".repeat(64) }; } },
  signedByOtherKey: { signer: "other" },
  requestOtherArgv: { argv: (argv) => [...argv, "--other"] },
  /** A signed payload edited after signing. */
  editedAfterSigning: { receipt: (r) => { r.provenance.payload.snapshotEvidence.snapshot.treeAtExit = "f".repeat(40); } },
  receiptCopyDiffers: { receipt: (r) => { r.outputDir = "/tampered"; } },
};

/**
 * One receipt, signed. `carrier` is the carrier the gate wrote; `argv`, `workspace` and `observedEnv` are what the client submitted.
 * `fx` is the fixture (paths, keys, pins).
 */
export function signedReceipt(fx, { id, carrier, argv, workspace, observedEnv = {}, tamper = "none", mutate = null }) {
  const t = TAMPERS[tamper] ?? fail(`unknown tamper ${tamper}`);
  const inputs = readDeclaredInputs(carrier.snapshot.path);
  const evidence = {
    requestId: id, brokerVersion: "rustright-test", executor: fx.pins.host, commit: carrier.commit,
    snapshot: { path: carrier.snapshot.path, treeAtStart: TREE, treeAtExit: TREE }, declaredInputDigests: declaredInputDigestsOf(carrier.snapshot.path, inputs),
    ownership: carrier.ownership, outputDir: fx.outputDir,
  };
  t.evidence?.(evidence, fx);
  const stage = { freshness: { unitsAtStart: 3, fresh: 3, notFresh: 0, invalidationReasons: [] }, attribution: { available: true, rows: [] }, toolchain: TOOLCHAIN };
  t.stage?.(stage);
  // The contract-named top-level fields every carried command's receipt has (item 4 row 4.18), signed with the rest.
  const top = { verdict: { status: "passed" }, refreshed: [], exit: 0, signal: null, timingsMs: { admission: 1, queue: 0, bind: 1, cargo: 2, afterExitBind: 1, diagnostics: 1 }, diagnosticsPath: "/diag/x.json" };
  t.top?.(top);
  const payload = {
    host: fx.pins.host, broker: { brokerDigest: fx.pins.brokerDigest }, keyId: fx.pins.keyId, jobId: id, nonce: carrier.nonce,
    requestDigest: requestDigestOf(snapshotRequestCore({ argv: t.argv ? t.argv(argv) : argv, workspace, observedEnv, carrier })),
    result: { status: "SUCCEEDED", exitCode: 0, signal: null, reap: { reaped: true } }, snapshotEvidence: structuredClone(evidence), stageEvidence: structuredClone(stage), ...structuredClone(top),
  };
  t.payload?.(payload);
  mutate?.(payload);
  const signer = t.signer === "other" ? crypto.generateKeyPairSync("ec", { namedCurve: "P-256" }).privateKey : fx.privateKey;
  const value = crypto.sign("sha256", signedBytes(payload), { key: signer, dsaEncoding: "ieee-p1363" }).toString("hex");
  const receipt = {
    id, ...structuredClone(evidence), ...structuredClone(stage), ...structuredClone(top),
    provenance: { schema: "rustright.provenance.v1", payload, signature: { alg: "ES256", keyId: fx.pins.keyId, value } },
  };
  t.receipt?.(receipt);
  return receipt;
}
const fail = (m) => { throw new Error(m); };

const write = (root, files) => { for (const [rel, body] of Object.entries(files)) { const f = path.join(root, rel); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, body); } };
const WORKSPACES = {
  "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n', "crates/a/src/lib.rs": "\n",
  "native/Cargo.toml": '[workspace]\nmembers = []\n[package]\nname = "b"\nversion = "0.1.0"\n', "native/src/lib.rs": "\n",
};

/**
 * A fixture broker host under a fresh temp root: a checkout and the gate's snapshot (two Cargo workspaces, the root and `native`), a
 * data root with the managed `ws` generation target, keys and pins, a control root, and a fake `rustright` client script whose
 * behaviour a test sets with `setBehavior({ tamper, exit, refuse })` (see client below). `sb` is the `deps.snapshotBroker` the gate builds.
 */
export function makeFakeBroker(t, { inputs = null } = {}) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-fake-broker-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checkout = path.join(root, "checkout"); const dataRoot = path.join(root, "data"); const controlRoot = path.join(root, "control");
  const snapshot = path.join(dataRoot, "snapshots", "0123456789abcdef");
  const wsRoot = path.join(dataRoot, "rightkit-build-control", "ws");
  const outputDir = path.join(wsRoot, "0123456789ab", "g", "002", "target");
  fs.mkdirSync(controlRoot, { recursive: true }); fs.mkdirSync(outputDir, { recursive: true });
  write(checkout, WORKSPACES); write(snapshot, WORKSPACES);
  if (inputs) { write(snapshot, { ".rightkit/compile-inputs.json": JSON.stringify({ inputs }), "gen/in.txt": "declared input\n" }); }
  const { privateKey, pins } = keyPair();
  const fx = { root, checkout, dataRoot, controlRoot, snapshot, wsRoot, outputDir, privateKey, pins, handle: HANDLE };
  const state = path.join(root, "client-state"); fs.mkdirSync(path.join(state, "db"), { recursive: true });
  fs.writeFileSync(path.join(state, "key.pem"), privateKey.export({ type: "pkcs8", format: "pem" }));
  fs.writeFileSync(path.join(state, "fixture.json"), JSON.stringify({ root, checkout, dataRoot, controlRoot, snapshot, wsRoot, outputDir, pins }));
  const client = path.join(root, "rustright");
  fs.writeFileSync(client, `#!/bin/sh\nexec ${JSON.stringify(process.execPath)} ${JSON.stringify(fileURLToPath(import.meta.url))} --client ${JSON.stringify(state)} "$@"\n`, { mode: 0o755 });
  const setBehavior = (b) => fs.writeFileSync(path.join(state, "behavior.json"), JSON.stringify(b));
  setBehavior({});
  const calls = () => (fs.existsSync(path.join(state, "calls.jsonl")) ? fs.readFileSync(path.join(state, "calls.jsonl"), "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)) : []);
  const submits = [];
  const sb = {
    controlRoot, commit: { sha: COMMIT, tree: TREE }, snapshot: { path: snapshot, expectedTree: TREE }, declaredInputs: inputs ?? [], handle: HANDLE, pins,
    markBrokerSubmit: () => { submits.push(Date.now()); return { handle: HANDLE, broker: { submitted: true } }; },
    checkout, outputRoots: [wsRoot], platform: process.platform, client,
    receiptOf: async (id) => JSON.parse(spawnStatus(client, id)).receipt,
  };
  return { ...fx, client, state, setBehavior, calls, sb, submits };
}
import { spawnSync } from "../background-process.mjs";
const spawnStatus = (client, id) => { const r = spawnSync(client, ["status", id], { encoding: "utf8" }); if (r.status !== 0) throw new Error(`status ${id}: ${r.stderr}`); return r.stdout; };

// ---- the fake client: `node fake-broker.mjs --client <state dir> cargo <args>` | `status <id>` ----
async function client(stateDir, [command, ...args]) {
  const read = (f, d = null) => { try { return JSON.parse(fs.readFileSync(path.join(stateDir, f), "utf8")); } catch { return d; } };
  const fx = { ...read("fixture.json"), privateKey: crypto.createPrivateKey(fs.readFileSync(path.join(stateDir, "key.pem"))) };
  const behavior = read("behavior.json", {});
  if (command === "cargo") {
    const env = process.env;
    const descriptor = JSON.parse(env.RIGHTKIT_SNAPSHOT_REQUEST ?? "null");
    const bad = (why) => { process.stderr.write(`invalid snapshot request carrier (${why})\n`); process.exit(2); };
    if (!descriptor) bad("no descriptor");
    if (behavior.refuse) bad("refused by the broker");
    if (!env.RIGHTKIT_CALLING_GATE) bad("no calling gate");
    const dir = fs.realpathSync(path.join(fx.controlRoot, "snapshot-requests"));
    if (fs.realpathSync(path.dirname(descriptor.path)) !== dir) bad("not in snapshot-requests");
    const bytes = fs.readFileSync(descriptor.path);
    if (sha(bytes) !== descriptor.sha256) bad("digest");
    const carrier = JSON.parse(bytes);
    if (carrier.nonce !== env.RIGHTKIT_ATTEST_NONCE) bad("nonce");
    if (carrier.argvSha256 !== argvSha256(args)) bad("argv");
    if (fs.realpathSync(carrier.primaryCwd) !== fs.realpathSync(process.cwd())) bad("primaryCwd");
    // A used carrier is a replay: the broker accepts each nonce once.
    const used = path.join(stateDir, "nonces"); fs.mkdirSync(used, { recursive: true });
    if (fs.existsSync(path.join(used, carrier.nonce))) bad("nonce replayed");
    fs.writeFileSync(path.join(used, carrier.nonce), "");
    const id = crypto.randomUUID();
    const workspace = fs.realpathSync(process.cwd());
    fs.writeFileSync(env.RIGHTKIT_REQUEST_ID_FILE, JSON.stringify({ schema: "rustright.request-ref.v1", requestId: id, submittedAt: new Date().toISOString(), workspace }));
    fs.writeFileSync(path.join(stateDir, "db", `${id}.json`), JSON.stringify({ id, carrier, argv: args, workspace, observedEnv: observedEnvOf(env, process.platform), tamper: behavior.tamper ?? "none" }));
    fs.appendFileSync(path.join(stateDir, "calls.jsonl"), `${JSON.stringify({ id, argv: args, cwd: workspace, env: { CARGO_TARGET_DIR: env.CARGO_TARGET_DIR ?? null, CARGO_BUILD_TARGET_DIR: env.CARGO_BUILD_TARGET_DIR ?? null } })}\n`);
    if (behavior.stdout) process.stdout.write(behavior.stdout);
    process.exit(behavior.exit ?? 0);
  }
  if (command === "status") {
    if (behavior.statusFail) { process.stderr.write("broker unreachable\n"); process.exit(1); }
    const entry = read(path.join("db", `${args[0]}.json`));
    if (!entry) { process.stderr.write(`no such request ${args[0]}\n`); process.exit(1); }
    // RustRight's envelope: {type:"STATUS", id, state, ..., receipt}; receipt is null while the job is live.
    if (read("behavior.json", {}).statusLive) { process.stdout.write(`${JSON.stringify({ type: "STATUS", id: entry.id, state: "RUNNING", receipt: null })}\n`); process.exit(0); }
    if (read("behavior.json", {}).statusBare) { process.stdout.write(`${JSON.stringify(signedReceipt(fx, { id: entry.id, carrier: entry.carrier, argv: entry.argv, workspace: entry.workspace, observedEnv: entry.observedEnv }))}\n`); process.exit(0); }
    const receipt = signedReceipt(fx, { id: entry.id, carrier: entry.carrier, argv: entry.argv, workspace: entry.workspace, observedEnv: entry.observedEnv, tamper: read("behavior.json", {}).statusTamper ?? entry.tamper });
    process.stdout.write(`${JSON.stringify({ type: "STATUS", id: entry.id, state: "SUCCEEDED", receipt })}\n`);
    process.exit(0);
  }
  process.stderr.write(`unsupported command ${command}\n`); process.exit(2);
}

if (process.argv[1] && (() => { try { return fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; } })() && process.argv[2] === "--client") {
  client(process.argv[3], process.argv.slice(4)).catch((e) => { process.stderr.write(`${e.stack}\n`); process.exit(3); });
}
export { canonicalJson };
