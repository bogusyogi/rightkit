// Test support for the broker snapshot carrier (item 7): a fixture host, receipts signed in-test with a real P-256 key the way
// rustright-protocol attestation.rs signs them, and a fake `rustright` client that applies the reader's carrier rules and serves
// `rustright status <id>`. It also models a snapshot-bound ADAPTER job (`rustright run <adapter> -- <args>`, rustright#190) the way the real
// broker runs one: the job's PATH is the broker's own adapter-job PATH (never the caller's, so no wrapper of the caller's is on it), its
// environment holds only what the broker passes (RUSTRIGHT_PARENT_REQUEST_ID and RUSTRIGHT_PARENT_CAPABILITY_TOKEN set, every other variable
// of the caller dropped: the carrier map, a stage grant, the stage nonce), its compiles run NESTED under it (a `cargo` on that PATH is the
// broker's nested request), and the receipt signs the list of those children. No broker, no cargo. NOT the dev or installed broker: native Mac and Dell runs against RustRight's
// item-4 dev install are separate evidence and are reported BLOCKED until that install lands.
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { OWNERSHIP_ENV, OWNERSHIP_FILE_ENV, markCarriedSend } from "../build-ownership.mjs";
import { ATTEST_STAGE_NONCE_ENV, canonicalJson, signedBytes } from "../provenance-verify.mjs";
import { SNAPSHOT_MUST_COMPILE, acceptedAfterFix, classifyCargo } from "./cargo-classification.mjs";
import { MANAGED_RUST_ENVIRONMENT_KEYS, argvSha256, cargoSubcommandOf, declaredInputDigestsOf, isCallHandleOf, observedEnvOf, readDeclaredInputs, requestDigestOf, snapshotRequestCore } from "../snapshot-request.mjs";

/**
 * A `markBrokerSubmit` fake that takes the call's sub-handle and asserts its shape (`<handle>#<id>`, the build ownership's own rule), so a
 * wiring that drops the argument fails here instead of being refused only by the real ownership. `onMark` sees each valid sub-handle.
 */
export const subHandleStub = (handle, onMark = () => {}) => (callHandle) => {
  assert.ok(isCallHandleOf(callHandle, typeof handle === "function" ? handle() : handle), `a broker submit is recorded for a carried request's sub-handle <handle>#<id>, not ${JSON.stringify(callHandle ?? null)}`);
  onMark(callHandle);
  const h = typeof handle === "function" ? handle() : handle;
  return { handle: h, broker: { submitted: true, handles: [callHandle] } };
};
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
  otherSubHandle: { evidence: (e) => { e.ownership = { handle: `${String(e.ownership.handle).split("#")[0]}#not-the-one-sent` }; } },
  bareHandle: { evidence: (e) => { e.ownership = { handle: String(e.ownership.handle).split("#")[0] }; } },
  otherHandle: { evidence: (e) => { e.ownership = { handle: "someone-else" }; } },
  otherCommit: { evidence: (e) => { e.commit = { sha: "d".repeat(40), tree: TREE }; } },
  otherSnapshotPath: { evidence: (e, fx) => { e.snapshot.path = path.join(fx.dataRoot, "snapshots", "ffffffffffffffff"); } },
  otherRequestId: { evidence: (e) => { e.requestId = "33333333-3333-4333-8333-333333333333"; } },
  noSnapshotEvidence: { payload: (p) => { delete p.snapshotEvidence; } },
  failed: { payload: (p) => { p.result = { status: "FAILED", exitCode: 0, signal: null, reap: { reaped: true } }; } },
  // RustRight refuses a tracked write at exit: the job's terminal receipt is FAILED and its message starts `snapshot exit bind:` (not `changed during execution`).
  exitBindRefused: { payload: (p) => { p.result = { status: "FAILED", exitCode: 1, signal: null, message: "snapshot exit bind: tracked path src/main.rs was written during the job", reap: { reaped: true } }; } },
  // The signed request core carries a variable the client did not forward (a broker that digested a different environment).
  adapterEnvChanged: { adapterEnv: (env) => ({ ...env, EXTRA: "1" }) },
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
  /** Retired fields (rustright bc24d55): a receipt that carries either is refused. */
  retiredVerdict: { receipt: (r) => { r.verdict = { status: "passed" }; } },
  retiredRefreshed: { receipt: (r) => { r.refreshed = []; } },
  exitNullNoSignal: { top: (x) => { x.exit = null; } },
  noAttribution: { payload: (p) => { delete p.stageEvidence.attribution; }, receipt: (r) => { delete r.attribution; } },
  noToolchain: { payload: (p) => { delete p.stageEvidence.toolchain; }, receipt: (r) => { delete r.toolchain; } },
  /** What a broker emits for a non-compile command: freshness only. */
  freshnessOnly: { payload: (p) => { delete p.stageEvidence.attribution; delete p.stageEvidence.toolchain; }, receipt: (r) => { delete r.attribution; delete r.toolchain; } },
  attributionReceiptOnly: { payload: (p) => { delete p.stageEvidence.attribution; } },
  attributionSignedOnly: { receipt: (r) => { delete r.attribution; } },
  noStageCensus: { payload: (p) => { for (const f of ["freshness", "attribution", "toolchain"]) delete p.stageEvidence[f]; }, receipt: (r) => { for (const f of ["freshness", "attribution", "toolchain"]) delete r[f]; } },
  freshnessReceiptOnly: { receipt: (r) => { delete r.freshness; } },
  freshnessSignedOnly: { payload: (p) => { delete p.stageEvidence.freshness; } },
  freshnessDiffers: { receipt: (r) => { r.freshness = { unitsAtStart: 9, fresh: 9, notFresh: 0, invalidationReasons: [] }; } },
  noStageEvidence: { payload: (p) => { delete p.stageEvidence; } },
  /** resultEvidence (attestation.rs RESULT_EVIDENCE_FIELDS): absent, partial, or disagreeing with the receipt's copy. */
  noResultEvidence: { payload: (p) => { delete p.resultEvidence; } },
  resultEvidenceTopLevelOnly: { payload: (p) => { Object.assign(p, structuredClone(p.resultEvidence)); delete p.resultEvidence; } },
  resultEvidenceEmpty: { payload: (p) => { p.resultEvidence = {}; } },
  resultEvidenceNoExit: { payload: (p) => { delete p.resultEvidence.exit; } },
  resultEvidenceNoSignal: { payload: (p) => { delete p.resultEvidence.signal; } },
  resultEvidenceNoTimings: { payload: (p) => { delete p.resultEvidence.timingsMs; } },
  resultEvidenceNoDiagnostics: { payload: (p) => { delete p.resultEvidence.diagnosticsPath; } },
  resultEvidenceExitDiffers: { payload: (p) => { p.resultEvidence.exit = 1; } },
  resultEvidenceSignalDiffers: { payload: (p) => { p.resultEvidence.signal = "SIGKILL"; } },
  resultEvidenceTimingsDiffer: { payload: (p) => { p.resultEvidence.timingsMs = { ...p.resultEvidence.timingsMs, cargo: 999 }; } },
  resultEvidenceDiagnosticsDiffers: { payload: (p) => { p.resultEvidence.diagnosticsPath = "/diag/other.json"; } },
  resultEvidenceAddedField: { payload: (p) => { p.resultEvidence.extra = 1; } },
  resultEvidenceTimingsAddedKey: { payload: (p) => { p.resultEvidence.timingsMs = { ...p.resultEvidence.timingsMs, bogus: 1 }; }, receipt: (r) => { r.timingsMs = { ...r.timingsMs, bogus: 1 }; } },
  resultEvidenceRetiredVerdict: { payload: (p) => { p.resultEvidence.verdict = { status: "passed" }; } },
  noTimings: { receipt: (r) => { delete r.timingsMs; } },
  noExit: { receipt: (r) => { delete r.exit; } },
  noDiagnostics: { receipt: (r) => { delete r.diagnosticsPath; } },
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
  /** The signed list of an adapter job's nested children (rustright#190, final), as a hostile or faulty broker could sign it. */
  noNestedChildren: { evidence: (e) => { delete e.nestedChildren; }, receipt: (r) => { delete r.nestedChildren; } },
  nestedChildrenNotSigned: { payload: (p) => { delete p.snapshotEvidence.nestedChildren; } },
  nestedChildrenNotMirrored: { receipt: (r) => { delete r.nestedChildren; } },
  nestedCopyDiffers: { receipt: (r) => { r.nestedChildren = []; } },
  childFailed: { evidence: (e) => { e.nestedChildren[0].exitCode = 1; e.nestedChildren[0].status = "FAILED"; } },
  childNullExit: { evidence: (e) => { e.nestedChildren[0].exitCode = null; } },
  childUnfinished: { evidence: (e) => { e.nestedChildren[0].exitCode = null; e.nestedChildren[0].status = "UNFINISHED"; } },
  childOtherParent: { evidence: (e) => { e.nestedChildren[0].parentRequestId = "44444444-4444-4444-8444-444444444444"; } },
  childOutsideSnapshot: { evidence: (e, fx) => { e.nestedChildren[0].cwd = path.join(fx.checkout, "native"); } },
  childOutputInsideSnapshot: { evidence: (e, fx) => { e.nestedChildren[0].outputDir = path.join(fx.snapshot, "target"); } },
  childOutputUnmanaged: { evidence: (e, fx) => { e.nestedChildren[0].outputDir = path.join(fx.root, "elsewhere", "nested", "x"); } },
  childOutputRelative: { evidence: (e) => { e.nestedChildren[0].outputDir = "target/nested/x"; } },
  childExtraField: { evidence: (e) => { e.nestedChildren[0].extra = true; } },
  childDuplicated: { evidence: (e) => { e.nestedChildren.push(structuredClone(e.nestedChildren[0])); } },
  childrenOverCap: { evidence: (e) => { const c = e.nestedChildren[0]; e.nestedChildren = Array.from({ length: 8193 }, (_, i) => ({ ...c, requestId: `child-${i}` })); } },
};

/**
 * One receipt, signed. `carrier` is the carrier the gate wrote; `argv`, `workspace` and `observedEnv` are what the client submitted.
 * `fx` is the fixture (paths, keys, pins).
 */
export function signedReceipt(fx, { id, carrier, argv, workspace, observedEnv = {}, adapterEnv = {}, tamper = "none", mutate = null, adapter = null, children = null }) {
  const t = TAMPERS[tamper] ?? fail(`unknown tamper ${tamper}`);
  const inputs = readDeclaredInputs(carrier.snapshot.path);
  const evidence = {
    requestId: id, brokerVersion: "rustright-test", executor: fx.pins.host, commit: carrier.commit,
    snapshot: { path: carrier.snapshot.path, treeAtStart: carrier.snapshot.expectedTree, treeAtExit: carrier.snapshot.expectedTree }, declaredInputDigests: declaredInputDigestsOf(carrier.snapshot.path, inputs),
    ownership: carrier.ownership, outputDir: fx.outputDir,
    // rustright#190 (final): an adapter job's nested children are signed INSIDE snapshotEvidence and mirrored on the receipt (the evidence copy below).
    ...(adapter ? { nestedChildren: structuredClone(children ?? []).map((c) => ({ ...c, parentRequestId: id })) } : {}),
  };
  t.evidence?.(evidence, fx);
  const stage = { freshness: { unitsAtStart: 3, fresh: 3, notFresh: 0, invalidationReasons: [] }, attribution: { available: true, rows: [] }, toolchain: TOOLCHAIN };
  t.stage?.(stage);
  // The contract-named top-level fields every carried command's receipt has (item 4 row 4.18), signed with the rest.
  const top = { exit: 0, signal: null, timingsMs: { admission: 1, queue: 0, bind: 1, cargo: 2, afterExitBind: 1, diagnostics: 1 }, diagnosticsPath: "/diag/x.json" };
  t.top?.(top);
  const payload = {
    host: fx.pins.host, broker: { brokerDigest: fx.pins.brokerDigest }, keyId: fx.pins.keyId, jobId: id, nonce: carrier.nonce,
    requestDigest: requestDigestOf(snapshotRequestCore({ argv: t.argv ? t.argv(argv) : argv, workspace, observedEnv, carrier, adapter, adapterEnv: t.adapterEnv ? t.adapterEnv(adapterEnv) : adapterEnv })),
    result: { status: "SUCCEEDED", exitCode: 0, signal: null, reap: { reaped: true } }, snapshotEvidence: structuredClone(evidence), stageEvidence: structuredClone(stage),
    // As the real broker signs them (service.rs provenance_payload): the result fields ride under resultEvidence, never at the payload's top level.
    resultEvidence: structuredClone(top),
  };
  t.payload?.(payload, fx);
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
 * A fixture broker host under a fresh temp root (`readOnlySnapshots: true` models the post-fix broker that accepts read-only snapshot requests): a checkout and the gate's snapshot (two Cargo workspaces, the root and `native`), a
 * data root with the managed `ws` generation target, keys and pins, a control root, and a fake `rustright` client script whose
 * behaviour a test sets with `setBehavior({ tamper, exit, refuse })` (see client below). `sb` is the `deps.snapshotBroker` the gate builds.
 */
export function makeFakeBroker(t, { inputs = null, readOnlySnapshots = false, inputsAt = ".rightkit/compile-inputs.json" } = {}) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-fake-broker-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checkout = path.join(root, "checkout"); const dataRoot = path.join(root, "data"); const controlRoot = path.join(root, "control");
  const snapshot = path.join(dataRoot, "snapshots", "0123456789abcdef");
  const wsRoot = path.join(dataRoot, "rightkit-build-control", "ws");
  const outputDir = path.join(wsRoot, "0123456789ab", "g", "002", "target");
  fs.mkdirSync(controlRoot, { recursive: true }); fs.mkdirSync(outputDir, { recursive: true });
  const scratchRoot = path.join(root, "scratch"); fs.mkdirSync(scratchRoot, { recursive: true });
  const adapterBin = path.join(root, "adapter-bin"); fs.mkdirSync(adapterBin, { recursive: true });
  write(checkout, WORKSPACES); write(snapshot, WORKSPACES);
  if (inputs) { write(snapshot, { [inputsAt]: JSON.stringify({ inputs }), "gen/in.txt": "declared input\n" }); }
  const { privateKey, pins } = keyPair();
  const fx = { root, checkout, dataRoot, controlRoot, snapshot, wsRoot, outputDir, privateKey, pins, handle: HANDLE, scratchRoot, adapterBin };
  const state = path.join(root, "client-state"); fs.mkdirSync(path.join(state, "db"), { recursive: true });
  fs.writeFileSync(path.join(state, "key.pem"), privateKey.export({ type: "pkcs8", format: "pem" }));
  // RK_FAKE_BROKER_LOG (opt-in): the client appends one JSON line per request it receives to this path; read here, once, so the child needs no env.
  fs.writeFileSync(path.join(state, "fixture.json"), JSON.stringify({ root, checkout, dataRoot, controlRoot, snapshot, wsRoot, outputDir, pins, readOnlySnapshots, scratchRoot, adapterBin, requestLog: process.env.RK_FAKE_BROKER_LOG || null, testName: t?.name ?? null, testFile: callerTestFile() }));
  const client = path.join(root, "rustright");
  fs.writeFileSync(client, `#!/bin/sh\nexec ${JSON.stringify(process.execPath)} ${JSON.stringify(fileURLToPath(import.meta.url))} --client ${JSON.stringify(state)} "$@"\n`, { mode: 0o755 });
  // The broker's adapter-job PATH holds the broker's own `cargo`: a compile the job's child starts is a nested request of this job.
  fs.writeFileSync(path.join(adapterBin, "cargo"), `#!/bin/sh\nexec ${JSON.stringify(process.execPath)} ${JSON.stringify(fileURLToPath(import.meta.url))} --client ${JSON.stringify(state)} nested-cargo "$@"\n`, { mode: 0o755 });
  const setBehavior = (b) => fs.writeFileSync(path.join(state, "behavior.json"), JSON.stringify(b));
  setBehavior({});
  const adapterCalls = () => (fs.existsSync(path.join(state, "adaptercalls.jsonl")) ? fs.readFileSync(path.join(state, "adaptercalls.jsonl"), "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)) : []);
  const calls = () => (fs.existsSync(path.join(state, "calls.jsonl")) ? fs.readFileSync(path.join(state, "calls.jsonl"), "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)) : []);
  const rootRequests = () => (fs.existsSync(path.join(state, "rootrequests.jsonl")) ? fs.readFileSync(path.join(state, "rootrequests.jsonl"), "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)) : []);
  const submits = [];
  // The gate's build-ownership record, as a right-release child inherits it (`ownershipEnv`: RIGHTKIT_BUILD_OWNERSHIP and _FILE, which a test
  // adds to the environment it hands a child): every carried call records its sub-handle in it just before it is sent, like the real record.
  const ownership = path.join(root, "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(ownership), { recursive: true });
  fs.writeFileSync(ownership, JSON.stringify({ handle: HANDLE, broker: { submitted: false } }));
  const ownershipEnv = { [OWNERSHIP_ENV]: HANDLE, [OWNERSHIP_FILE_ENV]: ownership };
  const owner = () => JSON.parse(fs.readFileSync(ownership, "utf8"));
  const sb = {
    controlRoot, commit: { sha: COMMIT, tree: TREE }, snapshot: { path: snapshot, expectedTree: TREE }, declaredInputs: inputs ?? [], handle: HANDLE, pins,
    markBrokerSubmit: (callHandle) => { submits.push(callHandle); return markCarriedSend({ file: ownership, handle: HANDLE, callHandle }); },
    checkout, outputRoots: [wsRoot], platform: process.platform, client,
    receiptOf: async (id) => JSON.parse(spawnStatus(client, id)).receipt,
  };
  // Models a broker host whose checkout and snapshot are a door fixture's (its own snapshots root holds the one snapshot): the client reads these at each call.
  const rebase = ({ checkout: checkoutRoot, snapshotsRoot }) => {
    const doc = JSON.parse(fs.readFileSync(path.join(state, "fixture.json"), "utf8"));
    fs.writeFileSync(path.join(state, "fixture.json"), JSON.stringify({ ...doc, ...(checkoutRoot ? { checkout: checkoutRoot } : {}), ...(snapshotsRoot ? { snapshotsRoot } : {}) }));
  };
  return { ...fx, client, state, setBehavior, rebase, calls, adapterCalls, rootRequests, sb, submits, ownership, ownershipEnv, owner };
}
import { spawnSync } from "../background-process.mjs";
const spawnStatus = (client, id) => { const r = spawnSync(client, ["status", id], { encoding: "utf8" }); if (r.status !== 0) throw new Error(`status ${id}: ${r.stderr}`); return r.stdout; };

// The test file that built this fake, read from the caller's stack (null when it is not found). Used only for RK_FAKE_BROKER_LOG.
function callerTestFile() {
  const hit = (new Error().stack ?? "").split("\n").map((l) => l.match(/((?:file:\/\/)?\/[^\s()]+?\.test\.mjs)/)).find(Boolean);
  return hit ? path.basename(hit[1].replace(/^file:\/\//, "")) : null;
}

// RK_FAKE_BROKER_LOG: one JSON line per request this fake receives (shim, argv, cwd, cwdInSnapshot, carried, grant, adapter, test, file).
// shim is "cargo", "run <adapter>", "nested-cargo" (a compile an adapter job's child starts) or the bare command ("status", ...).
// argv of "run" is the adapter's own arguments (after `--`). carried = a snapshot-request carrier env is set; grant = RIGHTKIT_ATTEST_STAGE_NONCE is set.
// Logging never changes the response; a failure to log is ignored.
function logRequest(fx, command, args) {
  try {
    const env = process.env;
    const envVar = (name) => env[`RUSTRIGHT_${name}`] ?? env[`RIGHTKIT_${name}`];
    let shim = command; let argv = args; let adapter = null; let carried = false;
    if (command === "run") { adapter = args[0] ?? null; shim = `run ${adapter}`; const sep = args.indexOf("--"); argv = sep >= 0 ? args.slice(sep + 1) : []; carried = envVar("SNAPSHOT_REQUEST") !== undefined; }
    else if (command === "cargo") { carried = envVar("SNAPSHOT_REQUEST") !== undefined; }
    const here = fs.realpathSync(process.cwd());
    const snap = fx.snapshot ? (() => { try { return fs.realpathSync(fx.snapshot); } catch { return fx.snapshot; } })() : null;
    const cwdInSnapshot = snap !== null && (here === snap || here.startsWith(`${snap}${path.sep}`));
    const line = { shim, argv, cwd: here, cwdInSnapshot, carried, grant: env[ATTEST_STAGE_NONCE_ENV] !== undefined, adapter, test: fx.testName ?? null, file: fx.testFile ?? null };
    fs.appendFileSync(fx.requestLog, `${JSON.stringify(line)}\n`);
  } catch { /* logging is best-effort; the fake's behavior never depends on it */ }
}

/**
 * The broker's one-completion-per-(snapshot, lease handle) rule (rustright snapshot.rs:330-343): a second carried request under a handle the
 * snapshot already has a completion for is refused ("snapshot ownership handle has a prior or unreadable completion").
 */
function claimHandle(stateDir, carrier, bad) {
  const dir = path.join(stateDir, "handles"); fs.mkdirSync(dir, { recursive: true });
  const file = path.join(dir, sha(`${carrier.snapshot.path}\0${carrier.ownership.handle}`));
  try { fs.writeFileSync(file, "", { flag: "wx" }); } catch { bad("snapshot ownership handle has a prior or unreadable completion"); }
}

// ---- the fake client: `node fake-broker.mjs --client <state dir> cargo <args>` | `status <id>` ----
async function client(stateDir, [command, ...args]) {
  const read = (f, d = null) => { try { return JSON.parse(fs.readFileSync(path.join(stateDir, f), "utf8")); } catch { return d; } };
  const fx = { ...read("fixture.json"), privateKey: crypto.createPrivateKey(fs.readFileSync(path.join(stateDir, "key.pem"))) };
  if (fx.snapshotsRoot) fx.snapshot = path.join(fx.snapshotsRoot, fs.readdirSync(fx.snapshotsRoot).find((n) => !n.includes(".") && fs.statSync(path.join(fx.snapshotsRoot, n)).isDirectory()));
  const behavior = read("behavior.json", {});
  if (fx.requestLog) logRequest(fx, command, args);
  if (command === "run") return adapterRun(stateDir, fx, behavior, args);
  if (command === "nested-cargo") return nestedCargo(stateDir, fx, behavior, args);
  if (command === "cargo") {
    const env = process.env;
    // The reader's own rules (rustright-cli shims.rs snapshot_carrier and the attested-run check): a current RUSTRIGHT_* name wins over the
    // legacy RIGHTKIT_* spelling, the descriptor is at most 4096 bytes with exactly `path` and `sha256`, the request file is a regular
    // file (never a link) directly in <control root>/snapshot-requests of at most 4 MiB whose fields are exactly the carrier's, cargo
    // only, no parent request, no admission-check call, a calling gate, and an attested call never rewrites its invocation.
    const envVar = (name) => env[`RUSTRIGHT_${name}`] ?? env[`RIGHTKIT_${name}`];
    const bad = (why) => { process.stderr.write(`invalid snapshot request carrier (${why})\n`); process.exit(2); };
    const raw = envVar("SNAPSHOT_REQUEST");
    // The real broker refuses an uncarried ROOT request whose workspace is inside a managed snapshot (service.rs: "a root request cannot run
    // inside a managed snapshot"). Every uncarried request is logged, so a test can count them (`rootRequests()`).
    if (raw === undefined && !args.includes("--version") && !args.includes("-V")) {
      const here = fs.realpathSync(process.cwd());
      const roots = [fx.snapshotsRoot, path.join(fx.dataRoot, "snapshots"), path.dirname(fx.snapshot)].filter(Boolean).map((r) => { try { return fs.realpathSync(r); } catch { return r; } });
      const inside = roots.some((r) => here === r || here.startsWith(`${r}${path.sep}`));
      fs.appendFileSync(path.join(stateDir, "rootrequests.jsonl"), `${JSON.stringify({ argv: args, cwd: here, refused: inside })}\n`);
      if (inside) { process.stderr.write("rustright: a root request cannot run inside a managed snapshot\n"); process.exit(2); }
      // A root `metadata` in the primary checkout is answered with the checkout's own paths (what the gate compares the snapshot's members with).
      if (args.includes("metadata")) { process.stdout.write(synthMetadata(fx, here, false)); process.exit(0); }
    }
    // The client's own version probe is a root request in a workspace of the primary checkout (the gate's tool probe); it reads no file.
    if (raw === undefined && (args.includes("--version") || args.includes("-V"))) { process.stdout.write("cargo 1.90.0 (fake)\n"); process.exit(0); }
    if (raw === undefined) bad("no descriptor");
    if (Buffer.byteLength(raw) > 4096) bad("descriptor exceeds 4096 bytes");
    let descriptor; try { descriptor = JSON.parse(raw); } catch { bad("descriptor is not JSON"); }
    if (!descriptor || typeof descriptor !== "object" || Object.keys(descriptor).sort().join() !== "path,sha256" || !/^[0-9a-f]{64}$/.test(descriptor.sha256) || !path.isAbsolute(descriptor.path)) bad("descriptor fields");
    if (behavior.refuse) bad("refused by the broker");
    if (envVar("PARENT_REQUEST_ID") !== undefined) bad("a parent request is set");
    if (String(envVar("CHECK_ADMISSION") ?? "").trim() === "1") bad("an admission check is not a snapshot compile");
    if (!String(envVar("CALLING_GATE") ?? "").trim()) bad("no calling gate");
    // rustright-cli shims.rs normalized_invocation: the client resolves the first `--manifest-path` before `--` against the cwd and runs from its directory; an attested call may not be rewritten,
    // so it passes only when that path is absolute and its directory IS the cwd (a tool's own `--manifest-path`, `cargo about`, is no exception).
    const separator = args.indexOf("--");
    const manifestAt = args.findIndex((a, k) => (separator < 0 || k < separator) && (a === "--manifest-path" || a.startsWith("--manifest-path=")));
    if (manifestAt >= 0) {
      const given = args[manifestAt] === "--manifest-path" ? args[manifestAt + 1] : args[manifestAt].slice("--manifest-path=".length);
      if (!given || !path.isAbsolute(given) || (() => { try { return fs.realpathSync(path.dirname(given)) !== fs.realpathSync(process.cwd()); } catch { return true; } })()) bad("attestation: the client would rewrite this invocation (--manifest-path); run from the manifest's directory without it");
    }
    // The broker admits a snapshot request only when it compiles (job_service/service.rs: "a snapshot request must compile"); read-only
    // subcommands (metadata, locate-project, --version, ...) are refused today. `readOnlySnapshots` models the broker after RustRight's change (the lock-bound allowlist, acceptedAfterFix).
    if (!(fx.readOnlySnapshots ? acceptedAfterFix(args, { checkout: fx.checkout }) : classifyCargo(args) === "compile")) { process.stderr.write(`${SNAPSHOT_MUST_COMPILE}\n`); process.exit(2); }
    const dir = fs.realpathSync(path.join(fx.controlRoot, "snapshot-requests"));
    if (fs.lstatSync(path.join(fx.controlRoot, "snapshot-requests")).isSymbolicLink()) bad("snapshot-requests is a link");
    if (fs.realpathSync(path.dirname(descriptor.path)) !== dir) bad("not in snapshot-requests");
    const meta = fs.lstatSync(descriptor.path);
    if (!meta.isFile() || meta.isSymbolicLink()) bad("the request file is not a regular file");
    if (meta.size > 4 * 1024 * 1024) bad("the request file exceeds 4 MiB");
    const bytes = fs.readFileSync(descriptor.path);
    if (sha(bytes) !== descriptor.sha256) bad("digest");
    const carrier = JSON.parse(bytes);
    const exactly = (value, keys, what) => { if (!value || typeof value !== "object" || Object.keys(value).sort().join() !== [...keys].sort().join()) bad(`unknown or missing fields in ${what}`); };
    exactly(carrier, ["schema", "commit", "snapshot", "declaredInputs", "ownership", "primaryCwd", "argvSha256", "nonce"], "the carrier");
    exactly(carrier.commit, ["sha", "tree"], "commit"); exactly(carrier.snapshot, ["path", "expectedTree"], "snapshot"); exactly(carrier.declaredInputs, ["inputs"], "declaredInputs"); exactly(carrier.ownership, ["handle"], "ownership");
    if (carrier.nonce !== envVar("ATTEST_NONCE")) bad("nonce");
    if (carrier.argvSha256 !== argvSha256(args)) bad("argv");
    if (fs.realpathSync(carrier.primaryCwd) !== fs.realpathSync(process.cwd())) bad("primaryCwd");
    // A used carrier is a replay: the broker accepts each nonce once.
    const used = path.join(stateDir, "nonces"); fs.mkdirSync(used, { recursive: true });
    if (fs.existsSync(path.join(used, carrier.nonce))) bad("nonce replayed");
    claimHandle(stateDir, carrier, bad);
    fs.writeFileSync(path.join(used, carrier.nonce), "");
    const id = crypto.randomUUID();
    const workspace = fs.realpathSync(process.cwd());
    fs.writeFileSync(envVar("REQUEST_ID_FILE"), JSON.stringify({ schema: "rustright.request-ref.v1", requestId: id, submittedAt: new Date().toISOString(), workspace }));
    fs.writeFileSync(path.join(stateDir, "db", `${id}.json`), JSON.stringify({ id, carrier, argv: args, workspace, observedEnv: observedEnvOf(env, process.platform), tamper: behavior.tamper ?? "none" }));
    fs.appendFileSync(path.join(stateDir, "calls.jsonl"), `${JSON.stringify({ id, carrierEnvNames: Object.keys(env).filter((k) => /^(RUSTRIGHT|RIGHTKIT)_(SNAPSHOT_REQUEST|ATTEST_NONCE|REQUEST_ID_FILE|CALLING_GATE|PARENT_REQUEST_ID)$/.test(k)).sort(), argv: args, cwd: workspace, stageNonceSeen: env.RIGHTKIT_ATTEST_STAGE_NONCE !== undefined, managedEnvNames: Object.keys(env).filter((k) => MANAGED_RUST_ENVIRONMENT_KEYS.includes(k.toUpperCase())).sort(), env: { CARGO_TARGET_DIR: env.CARGO_TARGET_DIR ?? null, CARGO_BUILD_TARGET_DIR: env.CARGO_BUILD_TARGET_DIR ?? null } })}\n`);
    if (behavior.stdout) process.stdout.write(behavior.stdout);
    else if (args.includes("metadata")) process.stdout.write(synthMetadata(fx, workspace));
    process.exit(behavior.exit ?? 0);
  }
  if (command === "snapshot-lease" && args[0] === "status") {
    // rustright snapshot.rs:469: the status of a (bare) ownership handle covers every held handle equal to it or starting with `<it>#`.
    const flag = (name) => args[args.indexOf(name) + 1];
    const snapshot = flag("--snapshot"); const wanted = flag("--ownership");
    const held = (behavior.heldHandles ?? []).some((h) => h === wanted || h.startsWith(`${wanted}#`));
    process.stdout.write(`${JSON.stringify({ schema: "rustright.snapshot-lease-status.v1", state: held ? "ACTIVE" : "REAPED", snapshot, ownership: wanted })}\n`);
    process.exit(0);
  }
  if (command === "status") {
    if (behavior.statusFail) { process.stderr.write("broker unreachable\n"); process.exit(1); }
    const entry = read(path.join("db", `${args[0]}.json`));
    if (!entry) { process.stderr.write(`no such request ${args[0]}\n`); process.exit(1); }
    // RustRight's envelope: {type:"STATUS", id, state, ..., receipt}; receipt is null while the job is live.
    if (read("behavior.json", {}).statusLive) { process.stdout.write(`${JSON.stringify({ type: "STATUS", id: entry.id, state: "RUNNING", receipt: null })}\n`); process.exit(0); }
    const children = entry.adapter ? readChildren(stateDir, entry.id) : null;
    if (read("behavior.json", {}).statusBare) { process.stdout.write(`${JSON.stringify(signedReceipt(fx, { id: entry.id, carrier: entry.carrier, argv: entry.argv, workspace: entry.workspace, observedEnv: entry.observedEnv, adapterEnv: entry.adapterEnv ?? {}, adapter: entry.adapter ?? null, children }))}\n`); process.exit(0); }
    const receipt = signedReceipt(fx, { id: entry.id, carrier: entry.carrier, argv: entry.argv, workspace: entry.workspace, observedEnv: entry.observedEnv, adapterEnv: entry.adapterEnv ?? {}, tamper: read("behavior.json", {}).statusTamper ?? entry.tamper, adapter: entry.adapter ?? null, children });
    process.stdout.write(`${JSON.stringify({ type: "STATUS", id: entry.id, state: "SUCCEEDED", receipt })}\n`);
    process.exit(0);
  }
  process.stderr.write(`unsupported command ${command}\n`); process.exit(2);
}

/**
 * The metadata (no dependencies) of the SNAPSHOT counterpart of a checkout directory (the job runs in the snapshot, so its paths are the
 * snapshot's): the workspace root is the nearest ancestor manifest with a [workspace] table (else the directory), the packages are its own
 * [package] and its `members`. Read from the fixture's manifests with plain patterns, enough for the gate's discovery.
 */
function synthMetadata(fx, checkoutCwd, inSnapshot = true) {
  const real = (p) => { try { return fs.realpathSync(p); } catch { return p; } };
  const checkout = real(fx.checkout); const snapshot = inSnapshot ? real(fx.snapshot) : checkout;
  const dir = path.join(snapshot, path.relative(checkout, checkoutCwd));
  const manifestIn = (d) => path.join(d, "Cargo.toml");
  let workspace = dir;
  for (let d = dir; d.startsWith(snapshot); d = path.dirname(d)) {
    if (fs.existsSync(manifestIn(d)) && /^\[workspace\]/m.test(fs.readFileSync(manifestIn(d), "utf8"))) { workspace = d; break; }
    if (d === snapshot) break;
  }
  const text = fs.readFileSync(manifestIn(workspace), "utf8");
  const packages = [];
  const pushPackage = (d) => { const m = /^\[package\][^[]*?name\s*=\s*"([^"]+)"/ms.exec(fs.readFileSync(manifestIn(d), "utf8")); if (m) packages.push({ name: m[1], manifest_path: manifestIn(d), publish: null }); };
  if (/^\[package\]/m.test(text)) pushPackage(workspace);
  const members = /members\s*=\s*\[([^\]]*)\]/s.exec(text);
  // A `dir/*` member is every directory below `dir` (cargo's rule); one without a Cargo.toml is an error, as in cargo.
  const listed = members ? [...members[1].matchAll(/"([^"]+)"/g)].map((x) => x[1]) : [];
  const expanded = listed.flatMap((m) => (m.endsWith("/*") ? (fs.existsSync(path.join(workspace, m.slice(0, -2))) ? fs.readdirSync(path.join(workspace, m.slice(0, -2)), { withFileTypes: true }).filter((e) => e.isDirectory()).map((e) => `${m.slice(0, -2)}/${e.name}`) : []) : [m]));
  for (const m of expanded) {
    if (!fs.existsSync(manifestIn(path.join(workspace, m)))) { process.stderr.write(`error: failed to load manifest for workspace member \`${path.join(workspace, m)}\`\n`); process.exit(101); }
    pushPackage(path.join(workspace, m));
  }
  return `${JSON.stringify({ packages, workspace_root: workspace, target_directory: fx.outputDir, version: 1 })}\n`;
}

// ---- a snapshot-bound adapter job, as the broker runs it (rustright#190) ----
// `rustright run <adapter> -- <args>` with a snapshot carrier: the reader's own rules (the carrier file, the nonce, no parent request, a calling
// gate, a primaryCwd equal to the cwd), the adapter's registered grammar in snapshot mode (D1b: pnpm `exec`, `run` and `pack` only; D3: no
// absolute path but inside the snapshot, a relative --dir, a --pack-destination that is an empty child of a scratch `rk-local-publish-*`
// directory), then the job runs in the SNAPSHOT counterpart of the cwd with the broker's own environment and PATH. A compile its child starts
// is nested under it (nestedCargo); the job's signed receipt lists those children.
const ADAPTERS = ["rightkit-pnpm", "rightkit-node", "rightkit-qa", "rightkit-swift"];
const PNPM_VALUE_OPTIONS = new Set(["--dir", "-C", "--filter", "-F", "--prefix"]);
const readChildren = (stateDir, id) => { try { return fs.readFileSync(path.join(stateDir, "db", `${id}.children.jsonl`), "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)); } catch { return []; } };

function snapshotArgvProblem(fx, adapter, args) {
  const insideOf = (child, parent) => { const rel = path.relative(parent, child); return rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel)); };
  const real = (p) => { try { return fs.realpathSync(p); } catch { return path.resolve(p); } };
  const own = args.includes("--") ? args.slice(0, args.indexOf("--")) : args;
  if (adapter === "rightkit-pnpm") {
    let command = null;
    for (let i = 0; i < own.length; i += 1) { const w = own[i]; if (w.startsWith("-")) { if (PNPM_VALUE_OPTIONS.has(w)) i += 1; continue; } command = w; break; }
    if (!["exec", "run", "pack"].includes(command)) return `the pnpm command ${command} is not allowed in a snapshot call (exec, run and pack only)`;
  }
  // rightkit-swift: `build` only, with one relative --package-path (policy check_swift_package; the registered subcommands are [build]).
  if (adapter === "rightkit-swift") {
    if (args[0] !== "build") return `the swift command ${args[0]} is not allowed (build only)`;
    const at = args.indexOf("--package-path");
    if (at < 0 || !args[at + 1] || path.isAbsolute(args[at + 1]) || args[at + 1].split("/").includes("..")) return "swift needs exactly one relative --package-path";
  }
  for (let i = 0; i < args.length; i += 1) {
    const w = args[i];
    const flag = /^(--[a-z-]+)=(.*)$/.exec(w);
    const name = flag ? flag[1] : w;
    const value = flag ? flag[2] : args[i + 1];
    if (name === "--pack-destination") {
      if (!value || !path.isAbsolute(value)) return "--pack-destination must be an absolute scratch directory";
      if (!/^rk-local-publish-/.test(path.basename(path.dirname(value))) || !insideOf(real(path.dirname(value)), real(fx.scratchRoot)) || real(path.dirname(path.dirname(value))) !== real(fx.scratchRoot)) return `--pack-destination ${value} is not a child of a scratch rk-local-publish-* directory`;
      let names = null; try { names = fs.readdirSync(value); } catch { /* checked below */ }
      if (names === null || names.some((n) => n.endsWith(".tgz"))) return `--pack-destination ${value} is not an existing directory without a tarball`;
      if (!flag) i += 1;
      continue;
    }
    if ((name === "--dir" || name === "-C" || name === "--prefix") && value !== undefined && path.isAbsolute(value)) return `${name} ${value} is absolute: a snapshot call names its directory relative to the snapshot`;
    for (const token of [flag ? flag[2] : w]) if (path.isAbsolute(token) && !insideOf(real(token), real(fx.snapshot))) return `the absolute path ${token} is outside the bound snapshot`;
  }
  return null;
}

function adapterRun(stateDir, fx, behavior, argv) {
  const [adapter, ...rest] = argv;
  const env = process.env;
  const envVar = (name) => env[`RUSTRIGHT_${name}`] ?? env[`RIGHTKIT_${name}`];
  const bad = (why) => { process.stderr.write(`invalid snapshot request carrier (${why})\n`); process.exit(2); };
  if (!ADAPTERS.includes(adapter)) bad(`${adapter} is not a registered snapshot adapter`);
  if (rest[0] !== "--") bad("the adapter's arguments follow `--`");
  const args = rest.slice(1);
  const raw = envVar("SNAPSHOT_REQUEST");
  if (raw === undefined) bad("no descriptor (an adapter job in a snapshot is refused without its carrier)");
  if (Buffer.byteLength(raw) > 4096) bad("descriptor exceeds 4096 bytes");
  let descriptor; try { descriptor = JSON.parse(raw); } catch { bad("descriptor is not JSON"); }
  if (!descriptor || typeof descriptor !== "object" || Object.keys(descriptor).sort().join() !== "path,sha256" || !/^[0-9a-f]{64}$/.test(descriptor.sha256) || !path.isAbsolute(descriptor.path)) bad("descriptor fields");
  if (behavior.refuse) bad("refused by the broker");
  if (envVar("PARENT_REQUEST_ID") !== undefined || envVar("PARENT_CAPABILITY_TOKEN") !== undefined) bad("a parent request is set: a root snapshot request is never nested");
  if (!String(envVar("CALLING_GATE") ?? "").trim()) bad("no calling gate");
  const dirPath = path.join(fx.controlRoot, "snapshot-requests");
  if (fs.lstatSync(dirPath).isSymbolicLink() || fs.realpathSync(path.dirname(descriptor.path)) !== fs.realpathSync(dirPath)) bad("not directly in snapshot-requests");
  const meta = fs.lstatSync(descriptor.path);
  if (!meta.isFile() || meta.isSymbolicLink() || meta.size > 4 * 1024 * 1024) bad("the request file is not a regular file of at most 4 MiB");
  const bytes = fs.readFileSync(descriptor.path);
  if (sha(bytes) !== descriptor.sha256) bad("digest");
  const carrier = JSON.parse(bytes);
  const exactly = (value, keys, what) => { if (!value || typeof value !== "object" || Object.keys(value).sort().join() !== [...keys].sort().join()) bad(`unknown or missing fields in ${what}`); };
  exactly(carrier, ["schema", "commit", "snapshot", "declaredInputs", "ownership", "primaryCwd", "argvSha256", "nonce"], "the carrier");
  if (carrier.nonce !== envVar("ATTEST_NONCE")) bad("nonce");
  if (carrier.argvSha256 !== argvSha256(args)) bad("argv");
  if (fs.realpathSync(carrier.primaryCwd) !== fs.realpathSync(process.cwd())) bad("primaryCwd");
  if (fs.realpathSync(process.cwd()) !== fs.realpathSync(fx.checkout) && !fs.realpathSync(process.cwd()).startsWith(`${fs.realpathSync(fx.checkout)}${path.sep}`)) bad("the request's workspace is not inside the primary checkout");
  // The request environment: what the client forwards (shims.rs adapter_environment) and the broker's check_environment (a reserved or
  // unregistered name is refused; the registration is `behavior.registeredEnv[adapter]`, empty by default).
  const forwarded = {};
  for (const key of String(envVar("ADAPTER_ENV") ?? "").split(",").map((k) => k.trim()).filter(Boolean)) {
    if (/^(PATH|HOME|NODE_OPTIONS)$/i.test(key) || /^(RUSTRIGHT_|RIGHTKIT_|LD_|DYLD_|CARGO|RUST)/i.test(key)) { process.stderr.write(`RUSTRIGHT_ADAPTER_ENV names ${key}, which is broker-controlled and cannot be forwarded to an adapter\n`); process.exit(2); }
    if (env[key] === undefined) { process.stderr.write(`RUSTRIGHT_ADAPTER_ENV names ${key}, which is not set\n`); process.exit(2); }
    forwarded[key] = env[key];
  }
  for (const key of Object.keys(forwarded)) if (!(behavior.registeredEnv?.[adapter] ?? []).includes(key)) { process.stderr.write(`RK_ADAPTER_ENV_DENIED: environment key ${JSON.stringify(key)} is not registered for this adapter\n`); process.exit(2); }
  const denied = snapshotArgvProblem(fx, adapter, args);
  if (denied) { process.stderr.write(`RK_ADAPTER_ARGV_DENIED: ${denied}\n`); process.exit(2); }
  const used = path.join(stateDir, "nonces"); fs.mkdirSync(used, { recursive: true });
  if (fs.existsSync(path.join(used, carrier.nonce))) bad("nonce replayed");
  claimHandle(stateDir, carrier, bad);
  fs.writeFileSync(path.join(used, carrier.nonce), "");
  const id = crypto.randomUUID();
  // The request workspace of a snapshot-bound adapter job is the cwd the client ran in: the primary counterpart of the step's snapshot directory.
  const clientCwd = fs.realpathSync(process.cwd());
  const workspace = behavior.workspaceIsToplevel ? fs.realpathSync(fx.checkout) : behavior.workspaceOutside ? fs.realpathSync(os.tmpdir()) : clientCwd; // the broker: request.workspace is the cwd the client ran in; workspaceIsToplevel models the old answer, workspaceOutside a workspace outside the checkout
  fs.writeFileSync(envVar("REQUEST_ID_FILE"), JSON.stringify({ schema: "rustright.request-ref.v1", requestId: id, submittedAt: new Date().toISOString(), workspace }));
  fs.writeFileSync(path.join(stateDir, "db", `${id}.json`), JSON.stringify({ id, carrier, argv: args, adapter, workspace, observedEnv: {}, adapterEnv: forwarded, tamper: behavior.tamper ?? "none" }));
  // The job runs in the snapshot's counterpart of the checkout directory, with the broker's environment: only what the broker passes, its own PATH.
  const token = crypto.randomBytes(16).toString("hex");
  fs.mkdirSync(path.join(stateDir, "tokens"), { recursive: true });
  fs.writeFileSync(path.join(stateDir, "tokens", id), token);
  const jobCwd = path.join(fx.snapshot, path.relative(fs.realpathSync(fx.checkout), clientCwd));
  const jobEnv = { ...forwarded, PATH: fx.adapterBin, HOME: env.HOME ?? "/", RUSTRIGHT_PARENT_REQUEST_ID: id, RUSTRIGHT_PARENT_CAPABILITY_TOKEN: token };
  const children = behavior.adapter?.children ?? [];
  let exit = behavior.exit ?? 0;
  for (const child of children) {
    // The child finds `cargo` the way a real child does: by the job's PATH (a shell resolves it; nothing is named by path here).
    const r = spawnSync("/bin/sh", ["-c", 'exec cargo "$@"', "sh", ...(child.argv ?? ["build", "--locked"])], { cwd: path.join(jobCwd, child.cwd ?? "."), env: jobEnv, encoding: "utf8" });
    if (r.status !== 0 && !behavior.adapter?.swallow) exit = exit || 1;
  }
  fs.appendFileSync(path.join(stateDir, "adaptercalls.jsonl"), `${JSON.stringify({ id, adapter, argv: args, cwd: clientCwd, workspace, primaryCwd: carrier.primaryCwd, jobCwd, jobEnvNames: Object.keys(jobEnv).sort(), forwardedEnv: forwarded, jobPath: jobEnv.PATH, callerEnvNamesDropped: Object.keys(env).filter((k) => !(k in jobEnv) && /^(RUSTRIGHT|RIGHTKIT)_/.test(k)).sort(), stageNonceSeen: env.RIGHTKIT_ATTEST_STAGE_NONCE !== undefined })}\n`);
  if (behavior.stdout) process.stdout.write(behavior.stdout);
  process.exit(exit);
}

// A compile started by an adapter job's child: the broker attributes it to the job by the parent's own capability token and nests it (no lease,
// no ledger, no evidence of its own; rustright#190 D7 and D8). A compile with no parent is a root request, which a snapshot stage never makes.
function nestedCargo(stateDir, fx, behavior, args) {
  const env = process.env;
  const parent = env.RUSTRIGHT_PARENT_REQUEST_ID;
  const fail = (why) => { process.stderr.write(`${why}\n`); process.exit(2); };
  if (!parent) fail("RK_SNAPSHOT_CARRIER_REQUIRED: a compile outside a broker job inside a snapshot has no carrier");
  let expected = null; try { expected = fs.readFileSync(path.join(stateDir, "tokens", parent), "utf8"); } catch { /* unknown parent */ }
  if (!expected || env.RUSTRIGHT_PARENT_CAPABILITY_TOKEN !== expected) fail("RK_NESTED_PARENT: the parent capability token is not the job's");
  const cwd = fs.realpathSync(process.cwd());
  const requestId = crypto.randomUUID();
  const inSnapshot = cwd === fs.realpathSync(fx.snapshot) || cwd.startsWith(`${fs.realpathSync(fx.snapshot)}${path.sep}`);
  // A refused child (outside the parent's snapshot) is NOT listed (rustright#190): it only fails the call that made it.
  if (!inSnapshot) fail("RK_NESTED_OUTSIDE_SNAPSHOT: a nested compile runs inside the parent's snapshot");
  // A child that compiles nothing (`rustc -vV`, `metadata`) has no output directory.
  const compiles = !(args[0] === "metadata" || args[0] === "-vV" || args[0] === "--version" || args.includes("-vV"));
  const outputDir = compiles ? path.join(fx.outputDir, "nested", path.basename(cwd)) : null;
  if (outputDir) fs.mkdirSync(outputDir, { recursive: true });
  const exitCode = behavior.adapter?.childExit ?? 0;
  fs.appendFileSync(path.join(stateDir, "db", `${parent}.children.jsonl`), `${JSON.stringify({ requestId, parentRequestId: parent, shim: "cargo", argv: args, cwd, exitCode, status: exitCode === 0 ? "SUCCEEDED" : "FAILED", outputDir })}\n`);
  process.exit(exitCode);
}
if (process.argv[1] && (() => { try { return fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; } })() && process.argv[2] === "--client") {
  client(process.argv[3], process.argv.slice(4)).catch((e) => { process.stderr.write(`${e.stack}\n`); process.exit(3); });
}
export { canonicalJson };
