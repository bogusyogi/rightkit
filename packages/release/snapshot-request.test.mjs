// The gate's half of a broker snapshot build (snapshot-request.mjs; joint plan step 4d). The carrier is checked against every rule
// RustRight's reader applies (rustright-cli shims.rs snapshot_carrier at 5c59b9b); the receipt verifier against a receipt signed with
// a real P-256 key the way rustright-protocol attestation.rs signs and checks it.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { canonicalJson, CAUSE_EVIDENCE_SCHEMA, CAUSE_UNEXPLAINED, CAUSE_UNKNOWN, signedBytes, verifyAttributionDetail } from "./provenance-verify.mjs";
import {
  ATTEST_NONCE_ENV, DESCRIPTOR_MAX_BYTES, REQUEST_ID_FILE_ENV, RESULT_EVIDENCE_FIELDS, SNAPSHOT_REQUEST_ENV, SNAPSHOT_REQUEST_SCHEMA, SnapshotRequestError, argvSha256, declaredInputDigestsOf, freshNonce,
  BROKER_COMPILE_COMMANDS, CARGO_COMMAND_ALIASES, cargoSubcommandOf, isCallHandleOf, isCompileClass, isLockBound, isPackageObservation, lockedArgv, isGenerationTarget, isInsideGenerationTarget, GENERATION_LEAVES, nextCallHandle, ownershipOfHandle, observedEnvOf, readDeclaredInputs, readRequestRef, requestDigestOf, snapshotRequestCore, verifySnapshotEvidenceIdentity, verifySnapshotReceipt, writeSnapshotRequest,
} from "./snapshot-request.mjs";
import { startRealBroker } from "./test-support/real-broker.mjs";
import { loadVector } from "./test-support/vectors.mjs";

const tmp = (t) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-snapreq-"))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
const sha = (s) => crypto.createHash("sha256").update(s).digest("hex");
const C = "c".repeat(40); const T = "e".repeat(40);

// ---- Data fixtures: receipts signed in-test with a REAL P-256 key, the way rustright-protocol attestation.rs signs them, judged by the production verifier. ----
// These are plain data (a key, constructed inputs, named mutations of a signed payload). No broker, client or process is involved, so no test that uses them is broker evidence;
// every test that does is named "data:". The behaviour of the real broker is asserted only in the tests that start it (startRealBroker).
const COMMIT = "c".repeat(40);
const TREE = "e".repeat(40);
const HANDLE = "own-7";
/** `<toolchain>|cargo:<sha256>|rustc:<sha256>` */
const TOOLCHAIN = `stable|cargo:${"1".repeat(64)}|rustc:${"2".repeat(64)}`;


function keyPair() {
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ec", { namedCurve: "P-256" });
  const jwk = publicKey.export({ format: "jwk" });
  const sec1 = Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x, "base64url"), Buffer.from(jwk.y, "base64url")]).toString("hex");
  return { privateKey, pins: { publicKey: sec1, keyId: sha(Buffer.from(sec1, "hex")), host: "host-1", brokerDigest: "b".repeat(64) } };
}

/**
 * RA-03: the cause evidence a real broker signs inside a compile's compact freshness (rustright-broker cargo_evidence.rs `Classification::compact`, schema
 * rustright.cause-evidence.v1). The default is the whole, explained evidence of a census that rebuilt nothing; `over` replaces fields of the evidence.
 */
const causeEvidence = (over = {}) => ({
  schema: "rustright.cause-evidence.v1", state: "complete", verdict: "explained", basis: "reasons", unavailable: null, gaps: [], historyMigrationNeeded: false,
  counts: { rows: 0, rowsDropped: 0, units: 0, unexplained: 0, uncovered: 0 }, unexplainedUnits: [], unexplainedUnitsTruncated: 0, uncoveredUnits: [], uncoveredUnitsTruncated: 0, ...over,
});
/** A compile's compact freshness as `compact_freshness` signs it: the census, the per-cause counts, the unexplained count (null when unknown) and the cause evidence. */
const explainedFreshness = (over = {}) => ({
  available: true, unitsAtStart: 3, fresh: 3, notFresh: 0, invalidationCounts: {}, causeCounts: {}, rebuiltUnits: [], rebuiltUnitsTotal: 0, unexplainedCount: 0, causeEvidence: causeEvidence(), ...over,
});

/** Receipt tampers, by name: each returns what a hostile or faulty broker could do. `payload` is signed after; `receipt` edits are applied after signing. */
const TAMPERS = {
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
  /** An absent entry for an input the snapshot holds (and, on a compile request, any absent entry): a forged absent. */
  absentForged: { evidence: (e) => { e.declaredInputDigests = e.declaredInputDigests.map((d) => ({ path: d.path, absent: true })); } },
  digestsDropped: { evidence: (e) => { e.declaredInputDigests = []; } },
  noFreshness: { payload: (p) => { delete p.stageEvidence.freshness; }, receipt: (r) => { delete r.freshness; } },
  /** GB-19/GB-82: a census that could not be taken is {available:false, reason} and FAILS a compile that would otherwise have succeeded (RR_FRESHNESS_UNAVAILABLE). */
  censusUnavailable: { stage: (g) => { g.freshness = { available: false, reason: "timeout" }; }, payload: (p) => { p.result = { status: "FAILED", exitCode: 1, signal: null, code: "RR_FRESHNESS_UNAVAILABLE", reap: { reaped: true } }; } },
  censusUnavailableNoReason: { stage: (g) => { g.freshness = { available: false }; } },
  freshnessNotObject: { stage: (g) => { g.freshness = "fresh"; } },
  attributionUnavailable: { stage: (g) => { g.attribution = { available: false, reason: "cargo-json-message-format-not-requested" }; } },
  attributionNoReason: { stage: (g) => { g.attribution = { available: false, reason: "" }; } },
  attributionNotObject: { stage: (g) => { g.attribution = "none"; } },
  toolchainObject: { stage: (g) => { g.toolchain = { rustc: "1.98.0" }; } },
  toolchainNoHashes: { stage: (g) => { g.toolchain = "stable|cargo:abc|rustc:def"; } },
  /** GB-66: the toolchain identity the broker could not build is exactly {state:"UNKNOWN",reason}; a failed job names the code. */
  toolchainUnknown: { stage: (g) => { g.toolchain = { state: "UNKNOWN", reason: "cargo is not registered" }; } },
  toolchainUnknownMalformed: { stage: (g) => { g.toolchain = { state: "UNKNOWN" }; } },
  toolchainEmptyName: { stage: (g) => { g.toolchain = `|cargo:${"1".repeat(64)}|rustc:${"2".repeat(64)}`; } },
  toolchainUnavailableFailed: { stage: (g) => { g.toolchain = { state: "UNKNOWN", reason: "rustc changed since registration" }; }, payload: (p) => { p.result = { status: "FAILED", exitCode: 1, signal: null, code: "RK_TOOLCHAIN_IDENTITY_UNAVAILABLE", reap: { reaped: true } }; } },
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
  /** RA-03: cause evidence a gate must refuse. Each edits freshness on both sides (signed and receipt), as a broker that signed it would. */
  causeUnexplained: { stage: (g) => { g.freshness = explainedFreshness({ notFresh: 1, unexplainedCount: 1, causeCounts: { "fingerprint-missing": 1 }, causeEvidence: causeEvidence({ verdict: "unexplained", counts: { rows: 1, rowsDropped: 0, units: 1, unexplained: 1, uncovered: 0 }, unexplainedUnits: ["a v0.1.0 (lib:a) [build]"] }) }); } },
  causeUnknownQuiet: { stage: (g) => { g.freshness = explainedFreshness({ notFresh: 1, unexplainedCount: null, causeCounts: undefined, invalidationReasonsUnavailable: "quiet-root", causeEvidence: causeEvidence({ state: "unavailable", basis: "none", verdict: "unknown", unavailable: "quiet-root", gaps: ["log-not-read"] }) }); delete g.freshness.causeCounts; } },
  causeMissing: { stage: (g) => { delete g.freshness.causeEvidence; delete g.freshness.unexplainedCount; delete g.freshness.causeCounts; } },
  causeForeignSchema: { stage: (g) => { g.freshness.causeEvidence.schema = "rustright.cause-evidence.v2"; } },
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
  /** BR19 evidence (GB-11): absent, one-sided or malformed. */
  noBr19Evidence: { payload: (p) => { delete p.br19Evidence; } },
  br19ReceiptOnly: { payload: (p) => { delete p.br19Evidence; }, receipt: (r) => { r.profile = "release-iterate"; } },
  br19SignedOnly: { receipt: (r) => { delete r.profile; } },
  br19ProfileDiffers: { receipt: (r) => { r.profile = "development"; } },
  br19ResourcesNoPriority: { br19: (b) => { delete b.resources.priority; } },
  br19InventedZero: { br19: (b) => { b.resources.grantedJobs = { state: "UNKNOWN" }; } },
  br19DigestNotHex: { br19: (b) => { b.environment.digest = "xyz"; } },
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
function signedReceipt(fx, { id, carrier, argv, workspace, observedEnv = {}, adapterEnv = {}, tamper = "none", mutate = null, adapter = null, children = null, exit = 0 }) {
  const t = TAMPERS[tamper] ?? fail(`unknown tamper ${tamper}`);
  const inputs = readDeclaredInputs(carrier.snapshot.path);
  const evidence = {
    requestId: id, brokerVersion: "rustright-test", executor: fx.pins.host, commit: carrier.commit,
    snapshot: { path: carrier.snapshot.path, treeAtStart: carrier.snapshot.expectedTree, treeAtExit: carrier.snapshot.expectedTree }, declaredInputDigests: declaredInputDigestsOf(carrier.snapshot.path, inputs, { allowAbsent: adapter === null && !isCompileClass(argv) }),
    ownership: carrier.ownership, outputDir: fx.outputDir,
    // rustright#190 (final): an adapter job's nested children are signed INSIDE snapshotEvidence and mirrored on the receipt (the evidence copy below).
    // The lineage is kept: a child names its own parent (a depth-2 child names an earlier child), and only a child with no parent is the root's.
    ...(adapter ? { nestedChildren: structuredClone(children ?? []).map((c) => ({ ...c, parentRequestId: c.parentRequestId && c.parentRequestId !== "root" ? c.parentRequestId : id })) } : {}),
  };
  t.evidence?.(evidence, fx);
  // GB-82: the broker signs COMPACT attribution (counts and a `detail` descriptor {path, sha256}); the per package and target rows live in the evidence file
  // `<id>.evidence.json` (schema rustright.stage-evidence/1, carrying the request id and the toolchain). Freshness states whether it is available.
  const rows = [{ package: "a", target: "a", kind: "lib", errors: 0, warnings: 0, fresh: true }];
  const detailBytes = Buffer.from(JSON.stringify({ schema: "rustright.stage-evidence/1", id, toolchain: TOOLCHAIN, attribution: { available: true, truncated: 0, units: rows } }));
  const evidenceDir = path.join(fx.root, "evidence"); fs.mkdirSync(evidenceDir, { recursive: true });
  const detailPath = path.join(evidenceDir, `${id}.evidence.json`); fs.writeFileSync(detailPath, detailBytes);
  const stage = {
    freshness: explainedFreshness(),
    attribution: { available: true, source: "cargo-json", packages: 1, units: rows.length, errors: 0, warnings: 0, otherMessages: 0, freshUnits: 1, truncated: 0, detail: { path: detailPath, sha256: sha(detailBytes) } },
    toolchain: TOOLCHAIN,
  };
  t.stage?.(stage);
  // The contract-named top-level fields every carried command's receipt has (item 4 row 4.18), signed with the rest.
  const top = { exit, signal: null, timingsMs: { admission: 1, queue: 0, bind: 1, cargo: 2, afterExitBind: 1, diagnostics: 1 }, diagnosticsPath: "/diag/x.json" };
  t.top?.(top);
  // GB-11: every receipt carries, and an attested receipt signs, the BR19 profile, stage, resources and environment (BROKER-CONTRACT section 14).
  const br19 = { profile: "release-iterate", stage: { name: argv.find((w) => !w.startsWith("-") && !w.startsWith("+")) ?? "check", source: adapter ? "adapter" : "command" }, resources: { grantedJobs: 4, memoryReservation: 4294967296, peakRssBytes: 1, samples: 1, sampledEveryMs: 100, priority: "SECOND" }, environment: { digest: "e".repeat(64), forwarded: Object.keys(adapterEnv).sort(), dropped: [] } };
  t.br19?.(br19);
  const payload = {
    host: fx.pins.host, broker: { brokerDigest: fx.pins.brokerDigest }, keyId: fx.pins.keyId, jobId: id, nonce: carrier.nonce,
    requestDigest: requestDigestOf(snapshotRequestCore({ argv: t.argv ? t.argv(argv) : argv, workspace, observedEnv, carrier, adapter, adapterEnv: t.adapterEnv ? t.adapterEnv(adapterEnv) : adapterEnv })),
    // GB-82: a nonzero exit is a signed FAILED carrying that exit, never a signed success (rustright-broker a_nonzero_exit_is_signed_as_a_failure_never_as_success).
    result: exit === 0 ? { status: "SUCCEEDED", exitCode: 0, signal: null, reap: { reaped: true } } : { status: "FAILED", exitCode: exit, signal: null, reap: { reaped: true } }, snapshotEvidence: structuredClone(evidence), stageEvidence: structuredClone(stage),
    // As the real broker signs them (service.rs provenance_payload): the result fields ride under resultEvidence, never at the payload's top level.
    resultEvidence: structuredClone(top),
    br19Evidence: structuredClone(br19),
  };
  t.payload?.(payload, fx);
  mutate?.(payload);
  const signer = t.signer === "other" ? crypto.generateKeyPairSync("ec", { namedCurve: "P-256" }).privateKey : fx.privateKey;
  const value = crypto.sign("sha256", signedBytes(payload), { key: signer, dsaEncoding: "ieee-p1363" }).toString("hex");
  const receipt = {
    id, ...structuredClone(evidence), ...structuredClone(stage), ...structuredClone(top), ...structuredClone(br19),
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

/** A host's directories for a data test: a checkout and a snapshot (two Cargo workspaces), a managed `ws` generation target, a control root, and a fresh real key with its pins. */
function dataHost(t, { inputs = null } = {}) {
  const root = tmp(t);
  const checkout = path.join(root, "checkout"); const dataRoot = path.join(root, "data"); const controlRoot = path.join(root, "control");
  const snapshot = path.join(dataRoot, "snapshots", "0123456789abcdef");
  const wsRoot = path.join(dataRoot, "rightkit-build-control", "ws");
  const outputDir = path.join(wsRoot, "0123456789ab", "g", "002", "target");
  fs.mkdirSync(controlRoot, { recursive: true }); fs.mkdirSync(outputDir, { recursive: true });
  write(checkout, WORKSPACES); write(snapshot, WORKSPACES);
  if (inputs) write(snapshot, { ".rightkit/compile-inputs.json": JSON.stringify({ inputs }), "gen/in.txt": "declared input\n" });
  const { privateKey, pins } = keyPair();
  return { root, checkout, dataRoot, controlRoot, snapshot, wsRoot, outputDir, privateKey, pins, handle: HANDLE, sb: { commit: { sha: COMMIT, tree: TREE }, snapshot: { path: snapshot, expectedTree: TREE } } };
}

function request(t, over = {}) {
  const root = tmp(t);
  const snapshot = path.join(root, "data", "snapshots", "0123456789abcdef"); fs.mkdirSync(snapshot, { recursive: true });
  const cwd = path.join(root, "checkout"); fs.mkdirSync(cwd);
  const argv = ["test", "-p", "core", "--locked"];
  const made = writeSnapshotRequest({ controlRoot: path.join(root, "control"), commit: { sha: C, tree: T }, snapshot: { path: snapshot, expectedTree: T }, declaredInputs: ["Cargo.lock", "crates/core"], handle: "own-1", primaryCwd: cwd, argv, nonce: "a".repeat(64), requestIdFile: path.join(root, "req.json"), ...over });
  return { root, snapshot, cwd, argv, made };
}

test("data: canonical JSON is RustRight's: its own test vector (attestation.rs canonical_json_sorts_keys_by_bytes_and_escapes_minimally)", () => {
  assert.equal(canonicalJson({ b: 1, a: [true, null, 'x"y\\z\n'], B: { d: 2, c: "é" } }), '{"B":{"c":"é","d":2},"a":[true,null,"x\\"y\\\\z\\u000a"],"b":1}');
});

test("data: carrier: the file and descriptor pass every rule RustRight's reader applies", (t) => {
  const { root, snapshot, cwd, argv, made } = request(t);
  const dir = path.join(root, "control", "snapshot-requests");
  // Under <control root>/snapshot-requests, a real directory and a regular file, owner-only on POSIX.
  assert.equal(path.dirname(made.file), dir);
  assert.ok(fs.lstatSync(dir).isDirectory() && !fs.lstatSync(dir).isSymbolicLink());
  if (process.platform !== "win32") { assert.equal(fs.statSync(made.file).mode & 0o777, 0o600); assert.equal(fs.statSync(dir).mode & 0o777, 0o700); }
  // Descriptor: {path, sha256} only, at most 4096 bytes, sha256 of the exact bytes.
  const d = JSON.parse(made.descriptor);
  assert.deepEqual(Object.keys(d).sort(), ["path", "sha256"]);
  assert.ok(Buffer.byteLength(made.descriptor) <= DESCRIPTOR_MAX_BYTES);
  assert.equal(d.sha256, sha(fs.readFileSync(made.file)));
  // Carrier: exactly the reader's fields (deny_unknown_fields, camelCase), the commit tree equal to the expected tree, nonce = the env's.
  const c = JSON.parse(fs.readFileSync(made.file, "utf8"));
  assert.deepEqual(Object.keys(c).sort(), ["argvSha256", "commit", "declaredInputs", "nonce", "ownership", "primaryCwd", "schema", "snapshot"]);
  assert.equal(c.schema, SNAPSHOT_REQUEST_SCHEMA);
  assert.deepEqual(c.commit, { sha: C, tree: T }); assert.deepEqual(c.snapshot, { path: snapshot, expectedTree: T });
  assert.deepEqual(c.declaredInputs, { inputs: ["Cargo.lock", "crates/core"] }); assert.match(c.ownership.handle, /^own-1#[A-Za-z0-9._-]+$/, "the carrier names a per-call sub-handle of the ownership"); assert.equal(isCallHandleOf(c.ownership.handle, "own-1"), true);
  assert.equal(c.primaryCwd, cwd);
  assert.equal(c.argvSha256, sha(canonicalJson(argv)), "sha256 of the canonical cargo argv, without cargo");
  assert.equal(c.nonce, made.env[ATTEST_NONCE_ENV]);
  assert.deepEqual(Object.keys(made.env).sort(), [ATTEST_NONCE_ENV, REQUEST_ID_FILE_ENV, SNAPSHOT_REQUEST_ENV].sort());
  assert.equal(made.env[SNAPSHOT_REQUEST_ENV], made.descriptor);
});

test("data: carrier: anything the reader would refuse is refused before a file is written", (t) => {
  const bad = [
    [{ commit: { sha: "abc", tree: T } }, /full lowercase object ids/],
    [{ snapshot: { path: "/x", expectedTree: "f".repeat(40) } }, /expectedTree/],
    [{ snapshot: { path: "relative", expectedTree: T } }, /absolute path/],
    [{ handle: " " }, /handle/],
    [{ primaryCwd: "rel" }, /primaryCwd/],
    [{ nonce: "short" }, /nonce/],
    [{ nonce: "A".repeat(64) }, /lowercase/],
    [{ declaredInputs: ["/abs"] }, /relative/],
  ];
  for (const [over, why] of bad) {
    const root = tmp(t);
    assert.throws(() => writeSnapshotRequest({ controlRoot: path.join(root, "control"), commit: { sha: C, tree: T }, snapshot: { path: "/snap", expectedTree: T }, handle: "h", primaryCwd: root, argv: ["build"], nonce: "a".repeat(64), ...over }),
      (e) => e instanceof SnapshotRequestError && why.test(e.message), JSON.stringify(over));
    assert.equal(fs.existsSync(path.join(root, "control", "snapshot-requests")) && fs.readdirSync(path.join(root, "control", "snapshot-requests")).length > 0, false);
  }
  // A snapshot-requests that is a link is never written through.
  const root = tmp(t); fs.mkdirSync(path.join(root, "control")); fs.mkdirSync(path.join(root, "elsewhere"));
  fs.symlinkSync(path.join(root, "elsewhere"), path.join(root, "control", "snapshot-requests"));
  assert.throws(() => writeSnapshotRequest({ controlRoot: path.join(root, "control"), commit: { sha: C, tree: T }, snapshot: { path: "/snap", expectedTree: T }, handle: "h", primaryCwd: root, argv: ["build"] }), /not a real directory/);
  assert.equal(freshNonce().length, 64); assert.match(freshNonce(), /^[0-9a-f]{64}$/); assert.notEqual(freshNonce(), freshNonce());
});

test("data: observedEnv: the managed Rust variables as the client records them, matched case-insensitively, spelling kept", (t) => {
  assert.deepEqual(observedEnvOf({ PATH: "/x", CARGO_TARGET_DIR: "/t", RUSTFLAGS: "-D warnings", HOME: "/h" }, "darwin"), { CARGO_TARGET_DIR: "/t", RUSTFLAGS: "-D warnings" });
  // RustRight keeps the key as captured (shims.rs capture_managed_rust_environment: key.clone()), on every platform.
  assert.deepEqual(observedEnvOf({ Cargo_Home: "C:\\c", Path: "x" }, "win32"), { Cargo_Home: "C:\\c" });
  assert.deepEqual(observedEnvOf({ cargo_target_dir: "/t" }, "linux"), { cargo_target_dir: "/t" });
  assert.deepEqual(observedEnvOf({ CARGO_HOME: "/a", cargo_home: "/b" }, "linux"), { CARGO_HOME: "/a", cargo_home: "/b" }, "POSIX names that differ by case are two variables");
  // Windows hands the child one of several case variants, the first in sorted order; only that one is observed.
  assert.deepEqual(observedEnvOf({ Cargo_Home: "/b", CARGO_HOME: "/a" }, "win32"), { CARGO_HOME: "/a" });
  // The spelling is bound by the request digest: an uppercased copy would not match what the broker signed.
  const { made, argv, cwd } = request(t);
  assert.notEqual(requestDigestOf(snapshotRequestCore({ argv, workspace: cwd, observedEnv: observedEnvOf({ Cargo_Home: "C:\\c" }, "win32"), carrier: made.carrier })),
    requestDigestOf(snapshotRequestCore({ argv, workspace: cwd, observedEnv: { CARGO_HOME: "C:\\c" }, carrier: made.carrier })));
});

test("data: request digest: the extended core binds every field the broker adds for a snapshot request", (t) => {
  const { made, argv, cwd } = request(t);
  const base = requestDigestOf(snapshotRequestCore({ argv, workspace: cwd, carrier: made.carrier }));
  assert.match(base, /^[0-9a-f]{64}$/);
  const core = snapshotRequestCore({ argv, workspace: cwd, carrier: made.carrier });
  assert.deepEqual(Object.keys(core).sort(), ["adapter", "commit", "declaredInputs", "env", "inputManifest", "nonce", "observedEnv", "outputs", "ownership", "requestedArgv", "shim", "snapshot", "workspace"]);
  for (const change of [
    (c) => ({ ...c, commit: { ...c.commit, sha: "d".repeat(40) } }), (c) => ({ ...c, snapshot: { ...c.snapshot, path: "/other" } }),
    (c) => ({ ...c, declaredInputs: { inputs: [] } }), (c) => ({ ...c, ownership: { handle: "other" } }), (c) => ({ ...c, nonce: "b".repeat(64) }),
  ]) assert.notEqual(requestDigestOf(snapshotRequestCore({ argv, workspace: cwd, carrier: change(made.carrier) })), base);
  assert.notEqual(requestDigestOf(snapshotRequestCore({ argv: [...argv, "-q"], workspace: cwd, carrier: made.carrier })), base);
  assert.notEqual(requestDigestOf(snapshotRequestCore({ argv, workspace: `${cwd}/x`, carrier: made.carrier })), base);
  assert.notEqual(requestDigestOf(snapshotRequestCore({ argv, workspace: cwd, observedEnv: { RUSTFLAGS: "x" }, carrier: made.carrier })), base);
});

// ---- row 7.22: every returned-evidence field is required and matches the request; the signature covers the snapshot evidence ----
const JOB = "11111111-1111-4111-8111-111111111111";
// RA-03: the cause evidence a broker signs inside `freshness` (rustright.cause-evidence.v1). `causeOf` builds one in its production form (the keys of cargo_evidence.rs
// `Classification::compact`), by default the whole, explained evidence of a census that rebuilt nothing.
const causeOf = (over = {}) => ({
  schema: CAUSE_EVIDENCE_SCHEMA, state: "complete", verdict: "explained", basis: "reasons", unavailable: null, gaps: [], historyMigrationNeeded: false,
  counts: { rows: 0, rowsDropped: 0, units: 0, unexplained: 0, uncovered: 0 }, unexplainedUnits: [], unexplainedUnitsTruncated: 0, uncoveredUnits: [], uncoveredUnitsTruncated: 0, ...over,
});
/** A census whose log was never read (a quiet, caller-logged or artifact-running root): no reasons, the typed reason why. */
const unreadCause = (reason, over = {}) => causeOf({ state: "unavailable", basis: "none", verdict: "unknown", unavailable: reason, gaps: ["log-not-read"], ...over });
/** Marks a freshness field to delete (a `null` is a value: `unexplainedCount` is null when the verdict is unknown). */
const DROP = Symbol("drop");
const freshnessWith = (f, fields) => { for (const [name, value] of Object.entries(fields)) { if (value === DROP) delete f[name]; else f[name] = structuredClone(value); } };
/**
 * `freshness` is a map of fields (`DROP` deletes one) applied to BOTH copies of the receipt's freshness (the signed stage evidence and the receipt's own, so parity holds) after the default cause evidence is added to
 * each that carries none: the fixture broker's freshness predates the schema, and a gate refuses cause evidence that is absent, so every case here states its own.
 */
function receiptCase(t, { tamper = "none", inputs = ["gen/in.txt"], carrierInputs = inputs ?? [], over = {}, mutate = null, argv = ["test", "-p", "a", "--locked"], freshness = null } = {}) {
  const fx = dataHost(t, { inputs });
  const made = writeSnapshotRequest({ controlRoot: fx.controlRoot, commit: fx.sb.commit, snapshot: fx.sb.snapshot, declaredInputs: carrierInputs, handle: fx.handle, primaryCwd: fx.checkout, argv, nonce: "a".repeat(64) });
  const edit = (f) => {
    if (f === null || typeof f !== "object" || Array.isArray(f)) return;
    if (f.causeEvidence === undefined) { f.causeEvidence = causeOf(); f.unexplainedCount = 0; f.causeCounts = {}; }
    if (freshness) freshnessWith(f, freshness);
  };
  const receipt = signedReceipt(fx, { id: JOB, carrier: made.carrier, argv, workspace: fx.checkout, tamper, mutate: (p) => { edit(p.stageEvidence?.freshness); mutate?.(p); } });
  edit(receipt.freshness);
  const check = (extra = {}) => verifySnapshotReceipt(receipt, { pins: fx.pins, jobId: JOB, carrier: made.carrier, argv, workspace: fx.checkout, checkout: fx.checkout, outputRoots: [fx.wsRoot], platform: process.platform, ...over, ...extra });
  return { fx, made, receipt, check, argv };
}

test("data: [7.22] a fully formed receipt verifies, with and without declared inputs", (t) => {
  for (const inputs of [["gen/in.txt"], null]) {
    const c = receiptCase(t, { inputs });
    const { evidence, payload } = c.check();
    assert.equal(evidence.outputDir, c.fx.outputDir);
    assert.equal(payload.result.status, "SUCCEEDED");
    assert.deepEqual(evidence.declaredInputDigests.map((d) => d.path), inputs ?? []);
  }
});

test("data: a broker verdict after a clean run (FAILED, exit 0) is refused and the message carries the receipt's unsigned error for diagnosis", (t) => {
  const c = receiptCase(t, { tamper: "failed" });
  c.receipt.error = "RR_UNEXPLAINED_COLD: 1 rebuilt unit(s) have no explained cause";
  assert.throws(() => c.check(), (e) => e.code === "result" && /signed FAILED \(exit \d+; the receipt's unsigned error: RR_UNEXPLAINED_COLD/.test(e.message) && /not SUCCEEDED with exit 0/.test(e.message));
});

test("data: [7.22] each refusal: provenance, signature, pins, request, job, nonce, signed outcome and snapshot evidence", (t) => {
  const cases = [
    // [tamper, error code, message pattern]
    ["noProvenance", "legacy", /no provenance envelope/], ["editedAfterSigning", "signature", /does not verify/], ["signedByOtherKey", "signature", /does not verify/],
    ["otherHost", "host", /another host/], ["otherBrokerDigest", "runtime", /another broker runtime/], ["otherJob", "job", /another job/], ["otherNonce", "nonce", /nonce/],
    ["requestOtherArgv", "request", /request digest/],
    ["failed", "result", /signed FAILED/], ["noResult", "result", /no result/],
    ["noSnapshotEvidence", "evidence", /no snapshotEvidence/], ["receiptCopyDiffers", "evidence", /outputDir differs/],
    ["treeAtExit", "evidence", /throughout/], ["treeAtStart", "evidence", /throughout/], ["otherHandle", "evidence", /handle is not this gate's/], ["otherCommit", "evidence", /signed commit is not/],
    ["otherSnapshotPath", "evidence", /signed snapshot/], ["otherRequestId", "evidence", /request id/],
    ["noBrokerVersion", "evidence", /brokerVersion differs/], ["noExecutor", "evidence", /executor differs/],
  ];
  for (const [tamper, code, why] of cases) {
    const c = receiptCase(t, { tamper });
    assert.throws(() => c.check(), (e) => e.code === code && why.test(e.message), `${tamper}: expected ${code} ${why}`);
  }
});

test("data: [7.22] declared inputs: the signed digests must be the gate's own digest of the snapshot's declared inputs, from the manifest it carried", (t) => {
  for (const tamper of ["digestWrong", "digestsDropped"]) {
    const c = receiptCase(t, { tamper });
    assert.throws(() => c.check(), (e) => e.code === "evidence" && /declaredInputDigests are not the gate's own digest/.test(e.message), tamper);
  }
  // A declared input changed in the snapshot after the broker digested it: the gate's own digest no longer matches.
  const moved = receiptCase(t);
  fs.writeFileSync(path.join(moved.fx.snapshot, "gen", "in.txt"), "changed after the build\n");
  assert.throws(() => moved.check(), /declaredInputDigests are not the gate's own digest/);
  // The manifest in the snapshot lists inputs the request did not carry.
  const other = receiptCase(t, { carrierInputs: [] });
  assert.throws(() => other.check(), /declared inputs are not the ones this request carried/);
  // The checkout's own manifest is never read: a different one there changes nothing.
  const c = receiptCase(t);
  fs.mkdirSync(path.join(c.fx.checkout, ".rightkit"), { recursive: true });
  fs.writeFileSync(path.join(c.fx.checkout, ".rightkit", "compile-inputs.json"), JSON.stringify({ inputs: ["Cargo.toml"] }));
  assert.equal(c.check().evidence.declaredInputDigests.length, 1);
});

test("data: declared inputs that are build outputs: a non-compiling request records a missing input as {path, absent:true}; a compile request refuses it", (t) => {
  const NONCOMPILE = ["metadata", "--no-deps", "--locked"];
  const ABSENT = [{ path: "gen/missing", absent: true }];
  // Non-compile, input absent in the snapshot and signed absent: accepted, in the exact wire form (no sha256 key).
  const ok = receiptCase(t, { inputs: ["gen/missing"], argv: NONCOMPILE });
  const got = ok.check().evidence.declaredInputDigests;
  assert.deepEqual(got, ABSENT);
  assert.equal(Object.hasOwn(got[0], "sha256"), false);
  // A mix: the present input keeps its digest, the missing one is absent.
  const mixed = receiptCase(t, { inputs: ["gen/in.txt", "gen/missing"], argv: NONCOMPILE });
  const mix = mixed.check().evidence.declaredInputDigests;
  assert.equal(mix[0].sha256.length, 64); assert.deepEqual(mix[1], ABSENT[0]);
  // Compile: the broker refuses a missing declared input, so a compile receipt that records one is refused, and the gate never digests it as absent.
  const compile = receiptCase(t, { inputs: ["gen/in.txt"], argv: ["test", "-p", "a", "--locked"] });
  fs.chmodSync(path.join(compile.fx.snapshot, "gen"), 0o755); fs.rmSync(path.join(compile.fx.snapshot, "gen", "in.txt"));
  assert.throws(() => compile.check(), (e) => e.code === "evidence" && /cannot digest the snapshot's declared inputs \(.*ENOENT/.test(e.message));
  const forged = receiptCase(t, { inputs: ["gen/in.txt"], argv: ["test", "-p", "a", "--locked"], tamper: "absentForged" });
  assert.throws(() => forged.check(), (e) => e.code === "evidence" && /absent declared input on a compile request/.test(e.message));
  // Disagreement, receipt absent but the gate sees the input present.
  const present = receiptCase(t, { inputs: ["gen/missing"], argv: NONCOMPILE });
  fs.mkdirSync(path.join(present.fx.snapshot, "gen"), { recursive: true });
  fs.writeFileSync(path.join(present.fx.snapshot, "gen", "missing"), "built later\n");
  assert.throws(() => present.check(), (e) => e.code === "evidence" && /not the gate's own digest/.test(e.message));
  // Disagreement, receipt present but the gate sees the input absent.
  const gone = receiptCase(t, { inputs: ["gen/in.txt"], argv: NONCOMPILE });
  fs.chmodSync(path.join(gone.fx.snapshot, "gen"), 0o755); fs.rmSync(path.join(gone.fx.snapshot, "gen", "in.txt"));
  assert.throws(() => gone.check(), (e) => e.code === "evidence" && /not the gate's own digest/.test(e.message));
  // The digest function: absent only when asked; a link is never absent.
  const snap = ok.fx.snapshot;
  assert.deepEqual(declaredInputDigestsOf(snap, ["gen/missing", "nope/deeper"], { allowAbsent: true }), [{ path: "gen/missing", absent: true }, { path: "nope/deeper", absent: true }]);
  assert.throws(() => declaredInputDigestsOf(snap, ["gen/missing"]), /ENOENT/);
  assert.throws(() => declaredInputDigestsOf(snap, ["gen/missing"], { allowAbsent: false }), /ENOENT/);
});

test("data: [7.22] output directory: absolute, outside the snapshot and the checkout, and a managed generation target", (t) => {
  for (const [tamper, why] of [["outputInsideSnapshot", /inside the snapshot/], ["outputInsideCheckout", /inside the checkout/], ["outputOutsideWs", /not a managed generation target/], ["outputNotTarget", /not a managed generation target/], ["outputRelative", /no absolute output directory/]]) {
    const c = receiptCase(t, { tamper });
    assert.throws(() => c.check(), (e) => e.code === "evidence" && why.test(e.message), tamper);
  }
  const c = receiptCase(t);
  // The gate must know where outputs may go: without a checkout and a managed root nothing can be judged, so it refuses.
  assert.throws(() => c.check({ outputRoots: [] }), /no checkout or managed output root/);
  assert.throws(() => c.check({ checkout: undefined }), /no checkout or managed output root/);
  assert.throws(() => c.check({ outputRoots: [path.join(c.fx.root, "another-ws")] }), /not a managed generation target/);
  const wsRoot = c.fx.wsRoot;
  for (const [out, ok] of [[`${wsRoot}/0123456789ab/g/002/target`, true], [`${wsRoot}/0123456789ab/g/1000/target`, true], [`${wsRoot}/0123456789abcdef/g/002/target`, false], [`${wsRoot}/0123456789a/g/002/target`, false], [`${wsRoot}/0123456789AB/g/002/target`, false], [`${wsRoot}/0123456789ab/g/2/target`, false], [`${wsRoot}/zzzzzzzzzzzz/g/002/target`, false],
    [`${wsRoot}/0123456789ab/g/002/target/debug`, false], [`${wsRoot}/0123456789ab/002/target`, false], [`${wsRoot}/../outside/0123456789ab/g/002/target`, false], [`${wsRoot}`, false]]) {
    assert.equal(isGenerationTarget(out, [wsRoot], process.platform), ok, out);
  }
  // The leaf is per generation (rustright-state lib.rs:52-62, :422-461): `target` or `t`, on either platform; any other leaf is refused.
  const winWs = "D:\\.rightkit-managed\\rightkit-build-control\\ws"; const winGen = `${winWs}\\0123456789ab\\g\\002`;
  for (const leaf of ["t", "target"]) {
    assert.equal(isGenerationTarget(`${winGen}\\${leaf}`, [winWs], "win32"), true, `win32 ${leaf}`);
    assert.equal(isGenerationTarget(`${wsRoot}/0123456789ab/g/002/${leaf}`, [wsRoot], "darwin"), true, `darwin ${leaf}`);
    assert.equal(isGenerationTarget(`${wsRoot}/0123456789ab/g/002/${leaf}`, [wsRoot], "linux"), true, `linux ${leaf}`);
  }
  for (const leaf of ["T", "tt", "debug", "targets", "release", ""]) {
    if (leaf !== "T") assert.equal(isGenerationTarget(`${winGen}\\${leaf}`, [winWs], "win32"), false, `win32 ${leaf}`); // Windows paths are case-insensitive
    assert.equal(isGenerationTarget(`${wsRoot}/0123456789ab/g/002/${leaf}`, [wsRoot], "linux"), false, `linux ${leaf}`);
  }
  assert.equal(isGenerationTarget("d:\\.RIGHTKIT-managed\\rightkit-build-control\\ws\\0123456789ab\\g\\002\\t", [winWs], "win32"), true, "Windows paths compare case-insensitively");
  assert.equal(isGenerationTarget("E:\\elsewhere\\0123456789ab\\g\\002\\t", [winWs], "win32"), false, "outside the managed root");
  assert.equal(isGenerationTarget(`${wsRoot}/../outside/0123456789ab/g/002/t`, [wsRoot], "linux"), false, "outside the managed root");
  assert.deepEqual([...GENERATION_LEAVES], ["target", "t"]);
});

test("data: [7.22] a nested compile's directory is inside a generation target with either leaf on either platform, and nowhere else", () => {
  const winWs = "D:\\ws"; const gen = `${winWs}\\0123456789ab\\g\\001`;
  for (const leaf of ["t", "target"]) {
    assert.equal(isInsideGenerationTarget(`${gen}\\${leaf}`, [winWs], "win32"), true, leaf);
    assert.equal(isInsideGenerationTarget(`${gen}\\${leaf}\\debug\\x`, [winWs], "win32"), true, leaf);
    assert.equal(isInsideGenerationTarget(`/w/ws/0123456789ab/g/001/${leaf}/debug`, ["/w/ws"], "linux"), true, leaf);
    assert.equal(isInsideGenerationTarget(`/w/ws/0123456789ab/g/001/${leaf}`, ["/w/ws"], "darwin"), true, leaf);
  }
  assert.equal(isInsideGenerationTarget(`${gen}\\debug\\x`, [winWs], "win32"), false);
  assert.equal(isInsideGenerationTarget("/w/ws/0123456789ab/g/001/other/x", ["/w/ws"], "linux"), false);
  assert.equal(isInsideGenerationTarget("/elsewhere/0123456789ab/g/001/t/x", ["/w/ws"], "linux"), false);
});

test("data: [7.22] lease handles: each carrier names its own `<ownership>#<n>`; the bare handle, another ownership's and a malformed one are refused; receipts bind the exact sub-handle", (t) => {
  const a = nextCallHandle("own-1"); const b = nextCallHandle("own-1");
  assert.notEqual(a, b); assert.equal(isCallHandleOf(a, "own-1"), true); assert.equal(ownershipOfHandle(a), "own-1");
  for (const bad of ["own-1", "own-1#", "own-10#1", "own-1#a b", "own-1##1", undefined, 7]) assert.equal(isCallHandleOf(bad, "own-1"), false, String(bad));
  assert.equal(isCallHandleOf("own-1#1", "own-1"), true);
  assert.throws(() => request(t, { callHandle: "own-1" }), /not a sub-handle/);
  assert.throws(() => request(t, { callHandle: "other#1" }), /not a sub-handle/);
  assert.equal(request(t, { callHandle: "own-1#fixed-1" }).made.carrier.ownership.handle, "own-1#fixed-1");
  assert.notEqual(request(t).made.carrier.ownership.handle, request(t).made.carrier.ownership.handle, "each call gets a fresh one");
  // The receipt must sign exactly the sub-handle the gate sent: another sub-handle of the same ownership, or the bare handle, fails.
  assert.doesNotThrow(() => receiptCase(t).check());
  for (const tamper of ["otherSubHandle", "bareHandle", "otherHandle"]) assert.throws(() => receiptCase(t, { tamper }).check(), (e) => e.code === "evidence" && /handle is not this gate's/.test(e.message), tamper);
});

test("data: [7.22] stage evidence and the unsigned fields: freshness, attribution and toolchain are signed and equal; timingsMs, exit, diagnosticsPath, executor are present with their types, and the retired verdict and refreshed are refused", (t) => {
  for (const [tamper, why] of [["noStageEvidence", /no stageEvidence/], ["noFreshness", /freshness is missing/], ["freshnessReceiptOnly", /freshness is missing/], ["freshnessSignedOnly", /freshness is missing/], ["freshnessDiffers", /freshness differs from the signed stage evidence/]]) {
    const c = receiptCase(t, { tamper });
    assert.throws(() => c.check(), (e) => e.code === "evidence" && why.test(e.message), tamper);
  }
  for (const [tamper, field] of [["noTimings", "timingsMs"], ["noExit", "exit"], ["noDiagnostics", "diagnosticsPath"]]) {
    const c = receiptCase(t, { tamper });
    assert.throws(() => c.check(), (e) => e.code === "evidence" && new RegExp(`receipt ${field} is missing or has the wrong type`).test(e.message), tamper);
  }
  // Wrong types are as bad as missing ones.
  for (const [field, value] of [["timingsMs", [1]], ["exit", "0"], ["signal", {}], ["diagnosticsPath", ""]]) {
    const c = receiptCase(t); c.receipt[field] = value;
    assert.throws(() => c.check(), new RegExp(`receipt ${field} is missing or has the wrong type`), `${field}=${JSON.stringify(value)}`);
  }
  for (const [tamper, field] of [["retiredVerdict", "verdict"], ["retiredRefreshed", "refreshed"]]) {
    assert.throws(() => receiptCase(t, { tamper }).check(), (e) => e.code === "evidence" && new RegExp(`receipt carries the retired field ${field}`).test(e.message), tamper);
  }
});

// ---- F7 re-decision #4: attribution and toolchain only for compile-class commands; the rest for every carried command ----
const NON_COMPILE = [["metadata", "--format-version", "1", "--offline"], ["fetch", "--locked"], ["fmt", "--check"], ["tree", "--locked"], ["--frozen", "pkgid"], ["version"]];
const COMPILE = [["check", "--locked"], ["clippy", "--", "-D", "warnings"], ["build"], ["doc", "--no-deps"], ["rustc", "--", "-Zx"], ["test", "-p", "a"], ["+stable", "--locked", "build"], ["--color", "always", "test"], ["b"], ["t"]];

test("data: [F7 #4] a non-compile command (metadata, fetch --locked, fmt --check, tree --locked) verifies without attribution and toolchain, with or without them present", (t) => {
  for (const argv of NON_COMPILE) {
    for (const tamper of ["freshnessOnly", "none"]) {
      const c = receiptCase(t, { argv, tamper });
      assert.equal(c.check().payload.result.status, "SUCCEEDED", `${argv.join(" ")} / ${tamper}`);
    }
  }
});

test("data: [F7 #4] a compile-class command (check, clippy, build, doc, rustc, test, and their aliases) fails without attribution or without toolchain", (t) => {
  for (const argv of COMPILE) {
    assert.equal(receiptCase(t, { argv }).check().payload.result.status, "SUCCEEDED", `${argv.join(" ")} verifies with both`);
    for (const [tamper, name] of [["noAttribution", "attribution"], ["noToolchain", "toolchain"], ["freshnessOnly", "attribution"]]) {
      const c = receiptCase(t, { argv, tamper });
      assert.throws(() => c.check(), (e) => e.code === "evidence" && new RegExp(`${name} is missing from the signed stage evidence or the receipt`).test(e.message), `${argv.join(" ")} / ${tamper}`);
    }
  }
});

test("data: [#1311] a read-only carried command's receipt passes with freshness (or the whole census) absent on both sides", (t) => {
  for (const argv of NON_COMPILE) {
    for (const tamper of ["noFreshness", "freshnessOnly", "none"]) {
      assert.equal(receiptCase(t, { argv, tamper }).check().payload.result.status, "SUCCEEDED", `${argv.join(" ")} / ${tamper}`);
    }
    // The whole census absent on both sides: a read-only metadata job has none.
    assert.equal(receiptCase(t, { argv, tamper: "noStageCensus" }).check().payload.result.status, "SUCCEEDED", `${argv.join(" ")} / no census`);
  }
});

test("data: [#1311] a compile-class receipt without a census fails; freshness on one side only fails (either side); a tampered freshness fails; the parity check is unchanged for read-only commands", (t) => {
  for (const argv of COMPILE) {
    assert.throws(() => receiptCase(t, { argv, tamper: "noStageCensus" }).check(), (e) => e.code === "evidence" && /is missing from the signed stage evidence or the receipt/.test(e.message), `${argv.join(" ")} / no census`);
    assert.throws(() => receiptCase(t, { argv, tamper: "noFreshness" }).check(), (e) => e.code === "evidence" && /freshness is missing from the signed stage evidence or the receipt/.test(e.message), `${argv.join(" ")} / no freshness on either side`);
    for (const tamper of ["freshnessReceiptOnly", "freshnessSignedOnly"]) {
      assert.throws(() => receiptCase(t, { argv, tamper }).check(), (e) => e.code === "evidence" && /freshness is missing/.test(e.message), `${argv.join(" ")} / ${tamper}`);
    }
  }
  for (const argv of NON_COMPILE) {
    // Present on the receipt side only (the signed stage evidence has none).
    assert.throws(() => receiptCase(t, { argv, tamper: "freshnessSignedOnly" }).check(), (e) => e.code === "evidence" && /receipt freshness differs from the signed stage evidence/.test(e.message), `${argv.join(" ")} / receipt side only`);
    // Present on the signed side only (the receipt dropped it).
    assert.throws(() => receiptCase(t, { argv, tamper: "freshnessReceiptOnly" }).check(), (e) => e.code === "evidence" && /receipt freshness differs from the signed stage evidence/.test(e.message), `${argv.join(" ")} / signed side only`);
    assert.throws(() => receiptCase(t, { argv, tamper: "freshnessDiffers" }).check(), (e) => e.code === "evidence" && /receipt freshness differs from the signed stage evidence/.test(e.message), `${argv.join(" ")} / tampered`);
    for (const tamper of ["attributionReceiptOnly", "attributionSignedOnly"]) {
      assert.throws(() => receiptCase(t, { argv, tamper }).check(), (e) => e.code === "evidence" && /attribution differs from the signed stage evidence/.test(e.message), `${argv.join(" ")} / ${tamper}`);
    }
    for (const [tamper, field] of [["noExit", "exit"], ["noTimings", "timingsMs"], ["noDiagnostics", "diagnosticsPath"]]) {
      assert.throws(() => receiptCase(t, { argv, tamper: tamper }).check(), (e) => e.code === "evidence" && new RegExp(`receipt ${field} is missing or has the wrong type`).test(e.message), `${argv.join(" ")} / ${tamper}`);
    }
    for (const [tamper, field] of [["retiredVerdict", "verdict"], ["retiredRefreshed", "refreshed"]]) {
      assert.throws(() => receiptCase(t, { argv, tamper }).check(), (e) => e.code === "evidence" && new RegExp(`receipt carries the retired field ${field}`).test(e.message), `${argv.join(" ")} / ${tamper}`);
    }
  }
});

test("data: [#1311] a nested adapter job's compile children are still validated when the job signs no freshness, attribution or toolchain", (t) => {
  const JOBID = JOB; const argv = ["exec", "tauri", "build"];
  const child = (fx, over = {}) => ({ requestId: "22222222-2222-4222-8222-222222222222", parentRequestId: JOBID, shim: "cargo", argv: ["build", "--locked"], cwd: path.join(fx.snapshot, "native"), exitCode: 0, status: "SUCCEEDED", outputDir: path.join(fx.outputDir, "nested", "native"), ...over });
  const run = (tamper, over = {}) => {
    const fx = dataHost(t, { inputs: ["gen/in.txt"] });
    const made = writeSnapshotRequest({ controlRoot: fx.controlRoot, commit: fx.sb.commit, snapshot: fx.sb.snapshot, declaredInputs: ["gen/in.txt"], handle: fx.handle, primaryCwd: fx.checkout, argv, nonce: "a".repeat(64) });
    const receipt = signedReceipt(fx, { id: JOBID, carrier: made.carrier, argv, workspace: fx.checkout, tamper, adapter: "rightkit-pnpm", children: [child(fx, over)] });
    return () => verifySnapshotReceipt(receipt, { pins: fx.pins, jobId: JOBID, carrier: made.carrier, argv, workspace: fx.checkout, checkout: fx.checkout, outputRoots: [fx.wsRoot], platform: process.platform, adapter: "rightkit-pnpm" });
  };
  const ok = run("freshnessOnly")();
  assert.equal(ok.nestedChildren.length, 1, "the adapter job verifies and its compile child is carried");
  assert.equal(run("noFreshness")().nestedChildren.length, 1, "no freshness on either side is fine for an adapter job");
  assert.throws(run("childFailed"), (e) => e.code === "nested" && /exited 1, not 0/.test(e.message));
  assert.throws(run("none", { cwd: "/elsewhere" }), (e) => e.code === "nested" && /outside the snapshot/.test(e.message));
  assert.throws(run("nestedCopyDiffers"), (e) => e.code === "nested" && /differs from the signed list/.test(e.message));
});

// ---- RR's receipt shapes (item 4 row 4.18, answers at rustright HEAD f4cf1170) ----
test("data: [RR 4.18] attribution: `{available:false, reason}` is accepted and recorded when the reason is a non-empty string; no reason, or not an object, is refused", (t) => {
  const ok = receiptCase(t, { tamper: "attributionUnavailable" }).check();
  assert.deepEqual(ok.attribution, { available: false, reason: "cargo-json-message-format-not-requested" }, "recorded for the consumer, which must treat available:false as FAILED where per-package attribution is needed");
  assert.equal(receiptCase(t).check().attribution.available, true);
  assert.throws(() => receiptCase(t, { tamper: "attributionNoReason" }).check(), (e) => e.code === "evidence" && /unavailable and names no reason/.test(e.message));
  assert.throws(() => receiptCase(t, { tamper: "attributionNotObject" }).check(), (e) => e.code === "evidence" && /not \{available/.test(e.message));
});

test("data: [RR 4.18] toolchain is `<toolchain>|cargo:<sha256>|rustc:<sha256>`, and is returned for the record; another shape is refused", (t) => {
  assert.match(receiptCase(t).check().toolchain, /^stable\|cargo:1{64}\|rustc:2{64}$/);
  for (const tamper of ["toolchainObject", "toolchainNoHashes"]) assert.throws(() => receiptCase(t, { tamper }).check(), (e) => e.code === "evidence" && /toolchain is not <toolchain>\|cargo:<sha256>\|rustc:<sha256>/.test(e.message), tamper);
});

test("data: [RR 4.18] top-level fields: diagnosticsPath may be null, timingsMs needs all six keys (each a number or null), exit may be null only with a signal and a null exit without one is not a success, and a receipt copy that differs from the signed one is refused", (t) => {
  assert.equal(receiptCase(t, { tamper: "diagnosticsNull" }).check().payload.result.status, "SUCCEEDED");
  assert.equal(receiptCase(t, { tamper: "timingsNulls" }).check().payload.result.status, "SUCCEEDED");
  assert.throws(() => receiptCase(t, { tamper: "timingsMissingKey" }).check(), /receipt timingsMs is missing or has the wrong type/);
  assert.throws(() => receiptCase(t, { tamper: "exitNullNoSignal" }).check(), /exit null and signal null are not a success/);
  const bad = receiptCase(t); bad.receipt.exit = 1;
  assert.throws(() => bad.check(), /not a success/);
  const signalled = receiptCase(t); signalled.receipt.exit = null; signalled.receipt.signal = "SIGKILL"; delete signalled.receipt.provenance.payload.resultEvidence.exit; delete signalled.receipt.provenance.payload.resultEvidence.signal;
  assert.throws(() => signalled.check(), (e) => e.code === "signature" || /not a success/.test(e.message), "a signalled run is never a success (editing the signed payload breaks the signature)");
});

test("data: [L1] the signed result fields are compared where the broker signs them (payload.resultEvidence): a missing, partial or disagreeing resultEvidence fails, a matching one passes", (t) => {
  assert.equal(receiptCase(t).check().payload.result.status, "SUCCEEDED");
  assert.deepEqual(Object.keys(receiptCase(t).check().payload.resultEvidence).sort(), [...RESULT_EVIDENCE_FIELDS].sort());
  assert.equal(Object.hasOwn(receiptCase(t).check().payload, "exit"), false, "the fake signs them as the real broker does: not at the top level");
  for (const tamper of ["noResultEvidence", "resultEvidenceTopLevelOnly", "resultEvidenceEmpty"]) assert.throws(() => receiptCase(t, { tamper }).check(), (e) => e.code === "evidence" && /no resultEvidence|differs from the signed result evidence/.test(e.message), tamper);
  for (const [tamper, field] of [["resultEvidenceNoExit", "exit"], ["resultEvidenceNoSignal", "signal"], ["resultEvidenceNoTimings", "timingsMs"], ["resultEvidenceNoDiagnostics", "diagnosticsPath"],
    ["resultEvidenceExitDiffers", "exit"], ["resultEvidenceSignalDiffers", "signal"], ["resultEvidenceTimingsDiffer", "timingsMs"], ["resultEvidenceDiagnosticsDiffers", "diagnosticsPath"]]) {
    assert.throws(() => receiptCase(t, { tamper }).check(), (e) => e.code === "evidence" && new RegExp(`receipt ${field} differs from the signed result evidence`).test(e.message), tamper);
  }
  assert.throws(() => receiptCase(t, { tamper: "resultEvidenceRetiredVerdict" }).check(), (e) => e.code === "evidence" && /retired field verdict/.test(e.message));
  // Added field: signed but unnamed by the protocol (in resultEvidence, or as a timingsMs key both sides carry).
  assert.throws(() => receiptCase(t, { tamper: "resultEvidenceAddedField" }).check(), (e) => e.code === "evidence" && /unknown field extra/.test(e.message), "added signed field");
  assert.throws(() => receiptCase(t, { tamper: "resultEvidenceTimingsAddedKey" }).check(), (e) => e.code === "evidence" && /unknown key bogus/.test(e.message), "added timings key");
  // Dropped on the receipt side while still signed, and present on the receipt while unsigned (one-sided either way), and dropped on both sides.
  for (const field of RESULT_EVIDENCE_FIELDS) {
    const dropReceipt = receiptCase(t); delete dropReceipt.receipt[field];
    assert.throws(() => dropReceipt.check(), (e) => e.code === "evidence", `receipt drops ${field}, the signature still carries it`);
    const dropBoth = receiptCase(t); delete dropBoth.receipt[field]; delete dropBoth.receipt.provenance.payload.resultEvidence[field];
    assert.throws(() => dropBoth.check(), (e) => e.code === "evidence" || e.code === "signature", `both drop ${field}`);
  }
  // The gate still reads the signed status and exit code: a signed FAILED never passes however the copies agree.
  assert.throws(() => receiptCase(t, { tamper: "failed" }).check(), (e) => e.code === "result");
  // A receipt copy edited after the fact (the signature still verifies the payload) disagrees with resultEvidence.
  for (const [field, value] of [["timingsMs", { admission: 1, queue: 0, bind: 1, cargo: 3, afterExitBind: 1, diagnostics: 1 }], ["diagnosticsPath", "/diag/elsewhere.json"]]) {
    const c = receiptCase(t); c.receipt[field] = value;
    assert.throws(() => c.check(), (e) => e.code === "evidence" && new RegExp(`receipt ${field} differs from the signed result evidence`).test(e.message), field);
  }
});

test("data: [F7 #4] the subcommand is read from the argv the way cargo reads it: after +toolchain and global options, before --", () => {
  for (const [argv, sub] of [[["build"], "build"], [["+nightly", "test"], "test"], [["--color", "always", "check"], "check"], [["--locked", "-q", "metadata"], "metadata"], [["--", "build"], null], [[], null], [["--explain", "E0001", "doc"], "doc"]]) {
    assert.equal(cargoSubcommandOf(argv), sub, argv.join(" "));
  }
  assert.equal(isCompileClass(["clippy"]), true); assert.equal(isCompileClass(["run"]), true); assert.equal(isCompileClass(["metadata"]), false); assert.equal(isCompileClass([]), false);
});

test("data: [GB-02] the compile table is the broker's: pinned to rustright b84ae605 command_policy.rs cargo_capability (:581 fix, :597 run, :611 package, :615 publish, :619 the BUILD list)", () => {
  // The broker's own spellings, copied from the cited lines at b84ae605 (git show b84ae605:crates/rustright-policy/src/command_policy.rs): a change on either side shows here.
  const brokerBuildList = ["build", "check", "test", "bench", "clippy", "doc", "rustc", "rustdoc"]; // :619
  const brokerOthers = ["fix", "run", "package", "publish"]; // :581, :597, :611, :615
  assert.deepEqual([...BROKER_COMPILE_COMMANDS].sort(), [...brokerBuildList, ...brokerOthers].sort());
  for (const command of [...brokerBuildList, "fix", "run", "publish"]) assert.equal(isCompileClass([command]), true, command);
  assert.equal(isCompileClass(["+nightly", command0()]), true);
  // package compiles unless it is exactly the observation listing the broker admits (build_rules.rs observation_only_cargo).
  assert.equal(isCompileClass(["package", "-p", "a"]), true);
  assert.equal(isCompileClass(["package", "-p", "a", "--list"]), true, "an unlocked listing is not the observation shape");
  assert.equal(isCompileClass(["package", "-p", "a", "--list", "--locked"]), false);
  assert.equal(isCompileClass(["package", "--package=a", "--list", "--locked", "--allow-dirty"]), false);
  for (const argv of [["package", "-p=a", "--list", "--locked"], ["package", "-p", "a", "-p", "b", "--list", "--locked"], ["package", "--list", "--locked"], ["package", "-p", "a", "--list", "--locked", "--no-verify"], ["+nightly", "package", "-p", "a", "--list", "--locked"], ["package", "-p", "a", "--list", "--locked", "--", "x"], ["package", "-p", "-q", "--list", "--locked"]]) {
    assert.equal(isPackageObservation(argv), false, argv.join(" "));
    assert.equal(isCompileClass(argv), true, argv.join(" "));
  }
  // Not compiles: the read-only and write classes.
  for (const argv of [["metadata"], ["metadata", "--no-deps"], ["fetch", "--locked"], ["fmt", "--check"], ["tree", "--locked"], ["--version"], ["deny", "check"], ["audit"], ["about", "generate"], []]) assert.equal(isCompileClass(argv), false, argv.join(" "));
  // The one-letter aliases are the gate's stricter reading (the broker lists none): each reads as its command.
  for (const [alias, command] of Object.entries(CARGO_COMMAND_ALIASES)) assert.equal(isCompileClass([alias]), isCompileClass([command]), alias);
  // lockedArgv derives from the same table: every compile command is locked, nothing else is.
  for (const command of [...brokerBuildList, "fix", "run", "publish"]) assert.deepEqual(lockedArgv([command, "-x"]), [command, "--locked", "-x"], command);
  assert.deepEqual(lockedArgv(["package", "-p", "a"]), ["package", "--locked", "-p", "a"]);
  assert.deepEqual(lockedArgv(["package", "-p", "a", "--list", "--locked"]), ["package", "-p", "a", "--list", "--locked"]);
  for (const argv of [["metadata"], ["fmt", "--check"], ["fetch"]]) assert.deepEqual(lockedArgv(argv), argv);
  for (const argv of [["build"], ["rustdoc"], ["package", "-p", "a"], ["bench"]]) assert.equal(isLockBound(argv), isCompileClass(argv));
});
const command0 = () => "build";

test("data: [GB-02] every compile command's receipt must carry freshness, attribution and toolchain: dropping a field from BOTH the signed evidence and the receipt fails for each; a read-only command passes without them", (t) => {
  for (const argv of [["bench"], ["run"], ["rustdoc"], ["package", "-p", "a"], ["fix"], ["publish"], ["clippy"], ["build"]]) {
    for (const field of ["freshness", "attribution", "toolchain"]) {
      const c = receiptCase(t, { argv });
      delete c.receipt[field]; delete c.receipt.provenance.payload.stageEvidence[field];
      assert.throws(() => c.check(), (e) => e.code === "evidence" || e.code === "signature", `${argv.join(" ")} without ${field}`);
    }
  }
});

test("data: [7.22] declared inputs are read from the snapshot and digested as the broker digests them (check_admission.rs snapshot_inputs)", (t) => {
  const root = tmp(t);
  const snap = path.join(root, "snap"); fs.mkdirSync(path.join(snap, ".rightkit"), { recursive: true });
  assert.deepEqual(readDeclaredInputs(snap), [], "no manifest: an empty declared set");
  const manifest = (v) => fs.writeFileSync(path.join(snap, ".rightkit", "compile-inputs.json"), typeof v === "string" ? v : JSON.stringify(v));
  manifest({ inputs: ["a.txt", "dir"] });
  fs.writeFileSync(path.join(snap, "a.txt"), "hello"); fs.mkdirSync(path.join(snap, "dir", "sub"), { recursive: true });
  fs.writeFileSync(path.join(snap, "dir", "b"), "B"); fs.writeFileSync(path.join(snap, "dir", "a"), "A"); fs.writeFileSync(path.join(snap, "dir", "sub", "c"), "C");
  const inputs = readDeclaredInputs(snap);
  assert.deepEqual(inputs, ["a.txt", "dir"]);
  const h = (...parts) => crypto.createHash("sha256").update(Buffer.concat(parts.map((p) => (typeof p === "string" ? Buffer.from(p) : p)))).digest("hex");
  // Per file, in sorted directory order: relative name, NUL, bytes, NUL (names joined with `/` on every platform, GB-49).
  assert.deepEqual(declaredInputDigestsOf(snap, inputs, { platform: "darwin" }), [
    { path: "a.txt", sha256: h("a.txt", "\0", "hello", "\0") },
    { path: "dir", sha256: h("dir/a", "\0", "A", "\0", "dir/b", "\0", "B", "\0", "dir/sub/c", "\0", "C", "\0") },
  ]);
  assert.equal(declaredInputDigestsOf(snap, ["dir"], { platform: "win32" })[0].sha256, h("dir/a", "\0", "A", "\0", "dir/b", "\0", "B", "\0", "dir/sub/c", "\0", "C", "\0"), "GB-49: RustRight now hashes names with `/` on Windows too (input_name), so the gate no longer joins with a backslash there");
  // A malformed or unsafe manifest is refused before any request.
  for (const [body, why] of [["not json", /not JSON/], [{ inputs: "x" }, /list of paths/], [{ inputs: [1] }, /non-empty relative path/], [{ inputs: ["/abs"] }, /plain relative path/], [{ inputs: ["../x"] }, /plain relative path/],
    [{ inputs: ["a/./b"] }, /plain relative path/], [{ inputs: ["a//b"] }, /plain relative path/], [{ inputs: [".git/config"] }, /inside \.git/], [{ inputs: [".rightkit/compile-inputs.json"] }, /declaration file itself/], [{ inputs: [".rightkit"] }, /declaration file itself/]]) {
    manifest(body);
    assert.throws(() => readDeclaredInputs(snap), (e) => e instanceof SnapshotRequestError && e.code === "declared-inputs" && why.test(e.message), JSON.stringify(body));
  }
  // A link anywhere is refused, a missing input too (creating a link needs a privilege on Windows: those cases run where it is held).
  const link = (target, at) => { try { fs.symlinkSync(target, at); return true; } catch { return false; } };
  if (link(path.join(snap, "a.txt"), path.join(snap, "dir", "link"))) {
    assert.throws(() => declaredInputDigestsOf(snap, ["dir"]), /holds a link/);
    fs.rmSync(path.join(snap, "dir", "link"));
  }
  assert.throws(() => declaredInputDigestsOf(snap, ["missing"]), /ENOENT/);
  if (link(path.join(snap, "dir"), path.join(snap, "viaLink"))) assert.throws(() => declaredInputDigestsOf(snap, ["viaLink"]), /runs through a link/);
  fs.mkdirSync(path.join(root, "real")); fs.rmSync(path.join(snap, ".rightkit", "compile-inputs.json"));
  fs.writeFileSync(path.join(root, "real", "m.json"), JSON.stringify({ inputs: [] }));
  if (link(path.join(root, "real", "m.json"), path.join(snap, ".rightkit", "compile-inputs.json"))) assert.throws(() => readDeclaredInputs(snap), /runs through a link/, "a manifest that is a link is refused");
});

test("data: request reference: the client's rustright.request-ref.v1, or a refusal", (t) => {
  const root = tmp(t); const f = path.join(root, "ref.json");
  fs.writeFileSync(f, JSON.stringify({ schema: "rustright.request-ref.v1", requestId: "r-1", submittedAt: "2026-10-29T00:00:00Z", workspace: root }));
  assert.equal(readRequestRef(f).requestId, "r-1");
  fs.writeFileSync(f, JSON.stringify({ schema: "other", requestId: "r-1", workspace: root }));
  assert.throws(() => readRequestRef(f), /not a rustright.request-ref.v1/);
  assert.throws(() => readRequestRef(path.join(root, "missing.json")), /no request reference/);
});

/**
 * The pins a gate enrolls for one isolated instance: the PUBLIC half of its own attestation key (`<control root>/attestation/key.json`, never its secret) and its runtime's broker digest, read from
 * the real client's own `doctor --json`. The gate never reads a key from a receipt; this is the enrollment a host operator performs for the instance a test started.
 */
function enrolledPinsOf(broker) {
  const key = JSON.parse(fs.readFileSync(path.join(broker.controlRoot, "attestation", "key.json"), "utf8"));
  const doctor = JSON.parse(broker.run(["doctor", "--json", "--no-scan"]).stdout);
  const find = (v, name) => { if (v && typeof v === "object") { if (typeof v[name] === "string") return v[name]; for (const x of Object.values(v)) { const r = find(x, name); if (r) return r; } } return null; };
  const brokerDigest = find(doctor, "brokerDigest"); const host = find(doctor, "host") ?? os.hostname();
  assert.match(String(brokerDigest), /^[0-9a-f]{64}$/, "the real client reports the installed broker's digest");
  return { host, keyId: key.keyId, publicKey: key.publicKey, brokerDigest };
}

test("runScans (real broker): a broker cargo request builds a real snapshot under its own carrier and passes only on the real broker's verified evidence; the mark is recorded before launch or nothing launches; every exit removes the carrier", async (t) => {
  const { runScans } = await import("./deterministic-scans.mjs");
  const { createSnapshotBroker } = await import("./snapshot-broker.mjs");
  const { syncSnapshot } = await import("./snapshot.mjs");
  const { spawnSync: realSpawn } = await import("./background-process.mjs");
  const broker = await startRealBroker({ name: "runscans" });
  t.after(() => broker.stop());
  const repo = tmp(t);
  const git = (...a) => { const r = realSpawn("git", a, { cwd: repo, encoding: "utf8" }); assert.equal(r.status, 0, r.stderr); return r.stdout.trim(); };
  git("init", "-q", "-b", "main"); git("config", "user.email", "t@example.test"); git("config", "user.name", "t");
  write(repo, { "Cargo.toml": '[workspace]\nmembers = ["crates/core"]\nresolver = "2"\n', "Cargo.lock": "version = 4\n", "crates/core/Cargo.toml": '[package]\nname = "core"\nversion = "0.1.0"\nedition = "2021"\n', "crates/core/src/lib.rs": "\n" });
  git("add", "-A"); git("commit", "-q", "-m", "A"); const sha1 = git("rev-parse", "HEAD");
  const snap = syncSnapshot({ checkout: repo, rev: sha1, root: path.join(broker.dataRoot, "snapshots") });
  const checkout = fs.realpathSync.native(repo);
  const handle = "own-9";
  const baseBroker = (mark) => createSnapshotBroker({ env: broker.env, platform: process.platform, checkout, commit: { sha: snap.commit, tree: snap.tree }, snapshot: { path: snap.path, expectedTree: snap.tree }, ownership: { handle, markBrokerSubmit: mark }, client: broker.clientPath, dataRoot: broker.dataRoot, loadPins: () => enrolledPinsOf(broker) });
  const scan = { id: "check:core", class: "B", tool: "rustright", args: ["cargo", "check", "-p", "core", "--locked"], root: "cargoLive", env: {}, command: "rustright cargo check -p core --locked" };
  const classA = { id: "a:noop", class: "A", tool: "node", args: ["-e", "0"], root: "live", env: {}, command: "node -e 0" };
  const plan = { roots: { contentRoot: snap.path, liveRoot: snap.path, cargoLiveRoot: checkout }, scans: [classA, scan], notes: [], deferred: [] };
  let launchSignal;
  // `inject` fails the launch at the spawn seam (an exception, an abort, a timeout); every other spawn, the carried request included, is the real client against the real broker.
  const run = async ({ mark, inject = null } = {}) => {
    const seen = { launched: false, submitsBefore: 0, file: null, refDir: null }; let submits = 0;
    const defaultMark = (h) => { assert.ok(isCallHandleOf(h, handle), `a broker submit is recorded for a sub-handle <handle>#<id>, not ${JSON.stringify(h ?? null)}`); submits += 1; return { handle, broker: { submitted: true, handles: [h] } }; };
    const sb = baseBroker(mark === undefined ? defaultMark : mark ?? (() => null));
    if (mark === null) delete sb.markBrokerSubmit;
    const spawn = (command, args, opts = {}) => {
      const env = { ...(opts.env ?? {}), ...broker.env };
      if (!env.RUSTRIGHT_SNAPSHOT_REQUEST) { if (args.includes("check")) seen.launched = true; return realSpawn(command, args, { ...opts, env, encoding: "utf8" }); }
      seen.launched = true; seen.submitsBefore = submits;
      const d = JSON.parse(env.RUSTRIGHT_SNAPSHOT_REQUEST); seen.file = d.path; seen.refDir = path.dirname(env.RUSTRIGHT_REQUEST_ID_FILE);
      if (inject) return inject(opts);
      return realSpawn(command, args, { ...opts, env, encoding: "utf8" });
    };
    const result = await runScans(plan, { spawn, lookupSpawn: spawn, env: { PATH: process.env.PATH, ...broker.env }, classes: ["A", "B"], signal: launchSignal, snapshotBroker: sb }).catch((e) => ({ thrown: e, scans: [] }));
    return { row: result.scans.find((r) => r.id === "check:core"), result, seen, submits, sb };
  };
  const good = await run();
  assert.equal(good.row.status, "pass", JSON.stringify(good.row));
  assert.equal(good.submits, 1); assert.equal(good.seen.submitsBefore, 1, "the ownership recorded the broker submit before the request ran");
  assert.equal(typeof good.row.snapshotEvidence.requestId, "string");
  assert.ok(good.row.snapshotEvidence.outputDir.startsWith(path.join(broker.dataRoot, "rightkit-build-control", "ws")), "the broker's own managed output directory");
  assert.equal(good.row.snapshotEvidence.treeAtStart, snap.tree); assert.equal(good.row.snapshotEvidence.treeAtExit, snap.tree);
  assert.equal(fs.existsSync(good.seen.file), false, "the carrier file is removed after the request");
  const carriers = () => fs.readdirSync(path.join(good.sb.controlRoot, "snapshot-requests"));
  // The submit is recorded before launch, or nothing launches: no hook, a hook that throws, a mark that did not land.
  for (const [mark, why] of [
    [null, /no build-ownership submit hook/],
    [() => { throw new Error("build ownership is no longer held"); }, /no longer held/],
    [() => ({ handle, broker: {} }), /did not record the broker submit/],
    [() => ({ handle: "someone-else", broker: { submitted: true } }), /did not record the broker submit/],
  ]) {
    const refused = await run({ mark });
    assert.equal(refused.row.status, "fail"); assert.equal(refused.row.code, "SNAPSHOT_REQUEST"); assert.match(refused.row.detail, why);
    assert.equal(refused.seen.launched, false, "the cargo request was never launched");
    assert.deepEqual(carriers(), [], "no carrier is left behind");
  }
  // Every exit after the carrier exists removes it and the request reference: a thrown spawn, a cancel, a timeout.
  const gone = (seen) => { assert.ok(seen.file && seen.refDir); assert.equal(fs.existsSync(seen.file), false, seen.file); assert.equal(fs.existsSync(seen.refDir), false, seen.refDir); };
  const thrown = await run({ inject: () => { throw new Error("spawn exploded"); } });
  assert.equal(thrown.row.status, "fail"); assert.equal(thrown.row.code, "SCAN_ERROR"); assert.match(thrown.row.detail, /spawn exploded/); gone(thrown.seen);
  const ac = new AbortController(); launchSignal = ac.signal;
  const cancelled = await run({ inject: () => { ac.abort(); return { status: null, error: Object.assign(new Error("aborted"), { code: "ABORT_ERR" }) }; } });
  assert.equal(cancelled.row.code, "CANCELLED"); gone(cancelled.seen);
  launchSignal = undefined;
  const late = await run({ inject: () => ({ status: null, error: Object.assign(new Error("timed out"), { code: "ETIMEDOUT" }) }) });
  assert.equal(late.row.code, "TIMEOUT"); gone(late.seen);
  assert.deepEqual(carriers(), []);
});

test("data: [7.22] a sibling directory whose name merely begins with two dots is not inside the snapshot or the checkout", (t) => {
  const c = receiptCase(t);
  // `<checkout>/..x/target` is inside the checkout; `<root>/..snap-sibling` is not inside the snapshot (a path-prefix check would say both).
  const out = (dir) => signedReceiptWithOutput(c, dir);
  assert.throws(() => out(path.join(c.fx.checkout, "..x", "target")).check(), /inside the checkout/);
  assert.throws(() => out(path.join(c.fx.snapshot, "..x", "target")).check(), /inside the snapshot/);
  assert.equal(isGenerationTarget(path.join(c.fx.wsRoot, "..0123456789a", "g", "002", "target"), [c.fx.wsRoot], process.platform), false);
});
function signedReceiptWithOutput(c, outputDir) {
  const receipt = signedReceipt(c.fx, { id: JOB, carrier: c.made.carrier, argv: c.argv, workspace: c.fx.checkout, tamper: "none", mutate: null });
  // Re-sign with the chosen output directory (signed and copied consistently).
  const evidence = { ...receipt.provenance.payload.snapshotEvidence, outputDir };
  const rebuilt = signedReceipt({ ...c.fx, outputDir }, { id: JOB, carrier: c.made.carrier, argv: c.argv, workspace: c.fx.checkout });
  void evidence;
  return { check: () => verifySnapshotReceipt(rebuilt, { pins: c.fx.pins, jobId: JOB, carrier: c.made.carrier, argv: c.argv, workspace: c.fx.checkout, checkout: c.fx.checkout, outputRoots: [c.fx.wsRoot], platform: process.platform }) };
}

test("data: [B9] the filesystem-free evidence identity: commit, handle, request id, snapshot path and the tree at start and exit, in the gate's order", () => {
  const commit = { sha: "a".repeat(40), tree: "b".repeat(40) };
  const evidence = { requestId: "job-1", commit, snapshot: { path: "/s/x", treeAtStart: commit.tree, treeAtExit: commit.tree }, ownership: { handle: "h" } };
  const ok = (over = {}, expect = {}) => verifySnapshotEvidenceIdentity({ ...evidence, ...over }, { commit, jobId: "job-1", ...expect });
  assert.deepEqual(ok(), evidence.snapshot, "handle and snapshot path are checked only when the caller knows them");
  assert.deepEqual(ok({}, { handle: "h", snapshotPath: "/s/x" }), evidence.snapshot);
  const refuses = (over, expect, pattern) => assert.throws(() => ok(over, expect), (e) => e instanceof SnapshotRequestError && e.code === "evidence" && pattern.test(e.message));
  refuses({ commit: { ...commit, sha: "c".repeat(40) } }, {}, /commit is not the one/);
  refuses({ commit: { ...commit, tree: "c".repeat(40) } }, {}, /commit is not the one/);
  refuses({}, { handle: "other" }, /ownership handle/);
  refuses({ requestId: "job-2" }, {}, /request id is not this job/);
  refuses({ requestId: undefined }, {}, /request id is not this job/);
  refuses({}, { snapshotPath: "/s/y" }, /signed snapshot \/s\/x is not \/s\/y/);
  refuses({ snapshot: { ...evidence.snapshot, treeAtStart: "c".repeat(40) } }, {}, /throughout/);
  refuses({ snapshot: { ...evidence.snapshot, treeAtExit: "c".repeat(40) } }, {}, /throughout/);
  refuses({ snapshot: undefined }, {}, /throughout/);
  assert.throws(() => verifySnapshotEvidenceIdentity(undefined, { commit, jobId: "job-1" }), /no snapshotEvidence/);
  assert.equal(ok({}, { snapshotPath: "/s/X", samePath: (a, b) => a.toLowerCase() === b.toLowerCase() }) === evidence.snapshot, true, "the gate compares real paths through its own comparator");
});

// ---- GB-02: the evidence a receipt owes is the broker's own compile classification (RustRight's cargo-shapes vector, `compile` column) ----
test("data: [GB-02] for every compile form of the vector, removing both copies of freshness, attribution or toolchain is refused; a read-only form legitimately carries none", (t) => {
  const rows = loadVector("cargo-shapes.fixtures.json").rows.filter((r) => !r.refused);
  assert.ok(rows.some((r) => r.compile) && rows.some((r) => !r.compile));
  for (const row of rows) {
    const argv = row.normalizedArgv;
    if (row.compile) {
      for (const tamper of ["noFreshness", "noAttribution", "noToolchain", "noStageCensus"]) {
        assert.throws(() => receiptCase(t, { argv, tamper }).check(), (e) => e.code === "evidence" && /is missing from the signed stage evidence or the receipt/.test(e.message), `${row.name} / ${tamper}`);
      }
      assert.equal(receiptCase(t, { argv }).check().payload.result.status, "SUCCEEDED", `${row.name}: the full evidence verifies`);
    } else {
      assert.equal(receiptCase(t, { argv, tamper: "noStageCensus" }).check().payload.result.status, "SUCCEEDED", `${row.name}: absence is valid for a form that compiles nothing`);
      assert.throws(() => receiptCase(t, { argv, tamper: "attributionReceiptOnly" }).check(), (e) => e.code === "evidence", `${row.name}: one-sided evidence still fails`);
    }
  }
});

// ---- GB-10 / GB-11: the gate verifies a compile receipt with the strict verifier ----
test("data: [GB-10] a receipt the strict verifier refuses never verifies: no or one-sided or malformed BR19 evidence, signed-only and receipt-only result evidence", (t) => {
  assert.equal(receiptCase(t).check().payload.result.status, "SUCCEEDED");
  for (const tamper of ["noBr19Evidence", "br19ReceiptOnly", "br19SignedOnly", "br19ProfileDiffers", "br19ResourcesNoPriority", "br19InventedZero", "br19DigestNotHex"]) {
    assert.throws(() => receiptCase(t, { tamper }).check(), (e) => e.code === "evidence", tamper);
  }
  // A timing phase missing on BOTH sides: equal, but not the contract's six phases.
  assert.throws(() => receiptCase(t, { mutate: (p) => { delete p.resultEvidence.timingsMs.queue; } }).check(), (e) => e.code === "evidence");
});

// ---- GB-66: the toolchain identity the broker could not build is a typed unavailable state ----
test("data: [GB-66] an UNKNOWN toolchain is a typed unavailable identity, never evidence; an empty component or a malformed UNKNOWN is still refused; a compile refused for it names the broker's code", (t) => {
  assert.equal(receiptCase(t).check().toolchain.startsWith("stable|cargo:"), true, "a full identity verifies");
  assert.throws(() => receiptCase(t, { tamper: "toolchainUnknown" }).check(), (e) => e.code === "toolchain-unavailable" && /RK_TOOLCHAIN_IDENTITY_UNAVAILABLE: UNKNOWN, cargo is not registered/.test(e.message));
  assert.throws(() => receiptCase(t, { tamper: "toolchainUnavailableFailed" }).check(), (e) => e.code === "toolchain-unavailable" && /did not run this compile.*rustc changed since registration/.test(e.message));
  for (const tamper of ["toolchainUnknownMalformed", "toolchainEmptyName", "toolchainNoHashes", "toolchainObject"]) assert.throws(() => receiptCase(t, { tamper }).check(), (e) => e.code === "evidence", tamper);
  // A read-only form carries no toolchain at all and is not made to.
  assert.equal(receiptCase(t, { argv: ["metadata", "--no-deps"], tamper: "noStageCensus" }).check().toolchain, null);
});

// ---- RA-03: a census that counts rebuilt units proves nothing about why; a compile is accepted only on cause evidence that is whole and explained ----
// Every case runs the real verifier over a signed receipt (both copies equal, the signature over the signed one). The cause evidence has the keys cargo_evidence.rs
// `Classification::compact` signs; its forms are the ones freshness_vectors.rs derives from real Cargo log text for each command form.
const KEPT = "app v0.1.0 (lib:app) [build]";
const oneRow = (over = {}) => ({ rows: 1, rowsDropped: 0, units: 1, unexplained: 0, uncovered: 0, ...over });
/** A compile's freshness with these cause fields (the census fields keep the fixture's, unless given). */
const withCause = (cause, { count = 0, causeCounts = {}, ...census } = {}) => ({ causeEvidence: cause, unexplainedCount: count, causeCounts, ...census });
const quietRebuilt = (reason) => withCause(unreadCause(reason), { count: null, causeCounts: DROP, invalidationReasons: DROP, notFresh: 1, invalidationReasonsUnavailable: reason });
const removedFingerprint = withCause(causeOf({ verdict: "unexplained", counts: oneRow({ unexplained: 1 }), unexplainedUnits: [KEPT] }), { count: 1, causeCounts: { "fingerprint-missing": 1 }, notFresh: 1 });

test("data: [RA-03] the typed codes of a verdict that is not explained are named", () => {
  assert.deepEqual([CAUSE_UNKNOWN, CAUSE_UNEXPLAINED], ["cause-unknown", "cause-unexplained"]);
  assert.equal(CAUSE_EVIDENCE_SCHEMA, "rustright.cause-evidence.v1");
});

test("data: [RA-03] a compile whose cause evidence is whole and explained verifies in every compile form, and the verified evidence is returned for the record", (t) => {
  const forms = {
    "a source edit: reasons read, every cause explained": withCause(causeOf({ counts: oneRow() }), { causeCounts: { "source-or-dependency-changed": 1 }, notFresh: 1 }),
    "a first build": withCause(causeOf({ counts: oneRow() }), { causeCounts: { "first-build": 1 }, notFresh: 1 }),
    "genuinely fresh quiet work: no log read, the census proves nothing was rebuilt": withCause(causeOf({ state: "unavailable", basis: "census-nothing-rebuilt", unavailable: "quiet-root" }), { causeCounts: DROP, invalidationReasons: DROP, notFresh: 0, invalidationReasonsUnavailable: "quiet-root" }),
    "a test run that executes artifacts but rebuilt nothing": withCause(causeOf({ state: "unavailable", basis: "census-nothing-rebuilt", unavailable: "root-executes-artifacts" }), { causeCounts: DROP, invalidationReasons: DROP, notFresh: 0, invalidationReasonsUnavailable: "root-executes-artifacts" }),
    "nothing compiled": withCause(causeOf({ state: "unavailable", basis: "nothing-compiled", unavailable: "not-read" }), { causeCounts: DROP, invalidationReasons: DROP, available: false, reason: "nothing-compiled" }),
  };
  for (const argv of COMPILE) {
    for (const [name, freshness] of Object.entries(forms)) {
      const verified = receiptCase(t, { argv, freshness }).check();
      assert.equal(verified.payload.result.status, "SUCCEEDED", `${argv.join(" ")} / ${name}`);
      assert.equal(verified.causeEvidence.verdict, "explained", `${argv.join(" ")} / ${name}`);
      assert.equal(verified.causeEvidence.unexplainedCount, 0);
    }
  }
  assert.equal(receiptCase(t).check().causeEvidence.basis, "reasons");
});

test("data: [RA-03] no compile succeeds without a valid cause: a removed fingerprint is unexplained, and quiet, caller-logged and artifact-running roots whose log was not read are unknown, in every compile form", (t) => {
  for (const argv of COMPILE) {
    assert.throws(() => receiptCase(t, { argv, freshness: removedFingerprint }).check(), (e) => e.code === CAUSE_UNEXPLAINED && e.message.includes(KEPT) && /1 rebuilt unit/.test(e.message), `${argv.join(" ")} / removed fingerprint`);
    for (const reason of ["quiet-root", "caller-cargo-log", "root-executes-artifacts"]) {
      assert.throws(() => receiptCase(t, { argv, freshness: quietRebuilt(reason) }).check(), (e) => e.code === CAUSE_UNKNOWN && e.message.includes(reason) && /never read as no unexplained unit/.test(e.message), `${argv.join(" ")} / ${reason}`);
    }
  }
  // Incomplete evidence is unknown, never a pass: dropped output, an unattributable row, a rebuilt unit no reason covers, a census that is itself unavailable.
  const incomplete = {
    "output dropped": withCause(causeOf({ state: "incomplete", verdict: "unknown", gaps: ["output-dropped"] }), { count: null }),
    "a rebuilt unit no row covers": withCause(causeOf({ state: "incomplete", verdict: "unknown", gaps: ["rebuilt-unit-without-reason"], counts: { rows: 0, rowsDropped: 0, units: 0, unexplained: 0, uncovered: 1 }, uncoveredUnits: ["b v0.1.0"] }), { count: null }),
    "reasons truncated": withCause(causeOf({ state: "incomplete", verdict: "unknown", gaps: ["reasons-truncated"], counts: { rows: 0, rowsDropped: 3, units: 0, unexplained: 0, uncovered: 0 } }), { count: null }),
    "census unavailable": withCause(unreadCause("not-read", { gaps: ["log-not-read", "census-unavailable"] }), { count: null, causeCounts: DROP, available: false, reason: "timeout" }),
  };
  for (const [name, freshness] of Object.entries(incomplete)) {
    assert.throws(() => receiptCase(t, { freshness }).check(), (e) => e.code === CAUSE_UNKNOWN, name);
  }
  // The unexplained units of a verdict are named even when the receipt kept only some of them.
  const many = withCause(causeOf({ verdict: "unexplained", counts: { rows: 40, rowsDropped: 0, units: 40, unexplained: 40, uncovered: 0 }, unexplainedUnits: Array.from({ length: 16 }, (_, i) => `u${i} v0.1.0 (lib:u${i}) [build]`), unexplainedUnitsTruncated: 24 }), { count: 40, causeCounts: { unexplained: 40 } });
  assert.throws(() => receiptCase(t, { freshness: many }).check(), (e) => e.code === CAUSE_UNEXPLAINED && /40 rebuilt unit/.test(e.message) && /u0 v0\.1\.0/.test(e.message) && /more not listed/.test(e.message));
});

test("data: [RA-03] missing cause evidence, an unrecognized schema and every incoherent field are refused as evidence: an older broker is never trusted as if it had signed the schema", (t) => {
  const noSchema = causeOf(); delete noSchema.schema;
  const noMigration = causeOf(); delete noMigration.historyMigrationNeeded;
  const absent = (cause, why) => assert.throws(() => receiptCase(t, { argv: ["build"], freshness: { causeEvidence: cause } }).check(), (e) => e.code === "evidence" && why.test(e.message), why.source);
  absent(DROP, /carries no causeEvidence/); absent(null, /carries no causeEvidence/);
  absent("explained", /is not an object/); absent([], /is not an object/);
  absent(noSchema, /does not read/); absent(causeOf({ schema: "rustright.cause-evidence.v2" }), /rustright\.cause-evidence\.v2.*does not read/); absent(causeOf({ schema: "rustright.cause-evidence.v0" }), /does not read/); absent(causeOf({ schema: "" }), /does not read/);
  absent(causeOf({ state: "done" }), /unknown state/); absent(causeOf({ verdict: "maybe" }), /unknown verdict/); absent(causeOf({ basis: "magic" }), /unknown basis/);
  absent(noMigration, /migration/); absent(causeOf({ unavailable: "" }), /unavailable is neither null nor a reason/); absent(causeOf({ gaps: "none" }), /gaps is not a list/); absent(causeOf({ gaps: [""] }), /gaps is not a list/);
  absent(causeOf({ counts: { rows: "1", rowsDropped: 0, units: 0, unexplained: 0, uncovered: 0 } }), /counts needs/); absent(causeOf({ counts: { rows: 0, rowsDropped: 0, units: 0, unexplained: 0 } }), /counts needs/); absent(causeOf({ counts: { rows: -1, rowsDropped: 0, units: 0, unexplained: 0, uncovered: 0 } }), /counts needs/);
  absent(causeOf({ unexplainedUnits: "x" }), /not a list of labels/); absent(causeOf({ uncoveredUnitsTruncated: "0" }), /not a list of labels/);
  absent(causeOf({ unexplainedUnits: ["x"] }), /do not add up/); absent(causeOf({ counts: oneRow({ unexplained: 2 }), unexplainedUnits: ["x"], verdict: "unexplained" }), /do not add up/);
  const incoherent = [
    ["an unexplained unit under an explained verdict", withCause(causeOf({ counts: oneRow({ unexplained: 1 }), unexplainedUnits: ["x"] }), { count: 1, causeCounts: { unexplained: 1 } }), /verdict explained disagrees/],
    ["an explained verdict with a gap", withCause(causeOf({ state: "incomplete", gaps: ["output-dropped"] }), { count: 0 }), /verdict explained disagrees/],
    ["a complete state with a gap", withCause(causeOf({ verdict: "unknown", gaps: ["output-dropped"] }), { count: null }), /state complete disagrees/],
    ["an incomplete state with no gap", withCause(causeOf({ state: "incomplete" })), /state incomplete disagrees/],
    ["an unexplained verdict with no unexplained unit", withCause(causeOf({ verdict: "unexplained" })), /verdict unexplained disagrees/],
    ["counts that are not nested", withCause(causeOf({ counts: oneRow({ units: 2 }) }), { causeCounts: { "first-build": 1 } }), /not nested/],
    ["rows dropped with no truncation gap", withCause(causeOf({ counts: { rows: 0, rowsDropped: 3, units: 0, unexplained: 0, uncovered: 0 } })), /reasons-truncated gap disagree/],
    ["an uncovered unit with no gap", withCause(causeOf({ counts: { rows: 0, rowsDropped: 0, units: 0, unexplained: 0, uncovered: 1 }, uncoveredUnits: ["b v0.1.0"] })), /rebuilt-unit-without-reason gap disagree/],
    ["reasons read but a reason they were not", withCause(causeOf({ unavailable: "quiet-root" })), /names an unavailability reason/],
    ["unread reasons with a reason-bearing state", withCause(causeOf({ basis: "none", verdict: "unknown", unavailable: "quiet-root", gaps: ["log-not-read"] }), { count: null }), /state complete is not unavailable/],
    ["unread reasons that state no reason", withCause(unreadCause("quiet-root", { unavailable: null }), { count: null }), /states no reason why/],
    ["unread reasons that carry counts", withCause(causeOf({ state: "unavailable", basis: "census-nothing-rebuilt", unavailable: "quiet-root", counts: oneRow() }), { causeCounts: DROP }), /counts reasons that were never read/],
    ["basis none that is not the typed unknown", withCause(causeOf({ state: "unavailable", basis: "none", unavailable: "quiet-root" }), { causeCounts: DROP }), /basis none is the typed unknown/],
    ["basis none without the log-not-read gap", withCause(unreadCause("quiet-root", { gaps: ["census-unavailable"] }), { count: null }), /basis none is the typed unknown/],
    ["a census claim that does not hold: rebuilt units under census-nothing-rebuilt", withCause(causeOf({ state: "unavailable", basis: "census-nothing-rebuilt", unavailable: "quiet-root" }), { causeCounts: DROP, notFresh: 2 }), /does not prove it/],
    ["a census claim with parse errors", withCause(causeOf({ state: "unavailable", basis: "census-nothing-rebuilt", unavailable: "quiet-root" }), { causeCounts: DROP, parseErrors: 1 }), /does not prove it/],
    ["a census claim with a truncated list", withCause(causeOf({ state: "unavailable", basis: "census-nothing-rebuilt", unavailable: "quiet-root" }), { causeCounts: DROP, notFreshPackagesTruncated: 1 }), /does not prove it/],
    ["a census claim without a rebuilt count", withCause(causeOf({ state: "unavailable", basis: "census-nothing-rebuilt", unavailable: "quiet-root" }), { causeCounts: DROP, notFresh: DROP }), /does not prove it/],
    ["nothing compiled while the census compiled", withCause(causeOf({ state: "unavailable", basis: "nothing-compiled", unavailable: "not-read" }), { causeCounts: DROP }), /nothing was compiled/],
    ["an unknown count read as zero", withCause(unreadCause("quiet-root"), { count: 0, causeCounts: DROP }), /unexplainedCount \(0\) is not the projection/],
    ["an unexplained count that is missing", withCause(causeOf(), { count: DROP }), /unexplainedCount \(absent\)/],
    ["an unexplained count that is not the projection", withCause(causeOf(), { count: 3 }), /unexplainedCount \(3\)/],
    ["a cause count that is missing for read reasons", withCause(causeOf(), { causeCounts: DROP }), /causeCounts is missing/],
    ["a cause count that is mistyped", withCause(causeOf(), { causeCounts: { "first-build": "1" } }), /causeCounts is missing/],
    ["cause counts that do not add up to the rows", withCause(causeOf(), { causeCounts: { "first-build": 2 } }), /add up to 2/],
    ["an unexplained cause behind an explained verdict", withCause(causeOf({ counts: oneRow() }), { causeCounts: { unexplained: 1 } }), /cannot give 0 unexplained/],
    ["a verdict of unexplained units with explained causes only", withCause(causeOf({ verdict: "unexplained", counts: oneRow({ unexplained: 1 }), unexplainedUnits: ["x"] }), { count: 1, causeCounts: { "first-build": 1 } }), /cannot give 1 unexplained/],
  ];
  for (const [name, freshness, why] of incoherent) {
    assert.throws(() => receiptCase(t, { argv: ["build"], freshness }).check(), (e) => e.code === "evidence" && why.test(e.message), name);
  }
});

test("data: [RA-03] the cause evidence is signed and equal on both sides: a receipt-only or signed-only change, or a flipped verdict, never verifies", (t) => {
  const flipped = receiptCase(t, { argv: ["build"], freshness: removedFingerprint });
  flipped.receipt.freshness.causeEvidence.verdict = "explained";
  assert.throws(() => flipped.check(), (e) => e.code === "evidence" && /receipt freshness differs from the signed stage evidence/.test(e.message), "the receipt claims an explained verdict the broker did not sign");
  const loud = receiptCase(t, { argv: ["build"] });
  loud.receipt.provenance.payload.stageEvidence.freshness.causeEvidence.verdict = "unexplained";
  assert.throws(() => loud.check(), (e) => e.code === "signature", "the signed copy was altered after signing");
  for (const tamper of ["freshnessReceiptOnly", "freshnessSignedOnly", "freshnessDiffers"]) {
    assert.throws(() => receiptCase(t, { argv: ["build"], tamper }).check(), (e) => e.code === "evidence" && /freshness/.test(e.message), tamper);
  }
  // A broker that signs no cause evidence on either side (an older installed broker) is refused for every compile form, whatever else is right.
  for (const argv of COMPILE) assert.throws(() => receiptCase(t, { argv, freshness: { causeEvidence: DROP, unexplainedCount: DROP, causeCounts: DROP } }).check(), (e) => e.code === "evidence" && /carries no causeEvidence/.test(e.message), argv.join(" "));
});

test("data: [RA-03] a command that is not a compile carries no census and owes no cause: absent is legitimate, a stray cause is read but not judged, a foreign schema is still refused", (t) => {
  for (const argv of NON_COMPILE) {
    const absent = receiptCase(t, { argv, freshness: { causeEvidence: DROP, unexplainedCount: DROP, causeCounts: DROP } }).check();
    assert.equal(absent.causeEvidence, null, argv.join(" "));
    assert.equal(receiptCase(t, { argv, tamper: "noStageCensus" }).check().causeEvidence, null, `${argv.join(" ")} / no census`);
    assert.equal(receiptCase(t, { argv, freshness: quietRebuilt("quiet-root") }).check().causeEvidence.verdict, "unknown", `${argv.join(" ")}: read and recorded, not judged for a command that compiles nothing`);
    assert.throws(() => receiptCase(t, { argv, freshness: { causeEvidence: causeOf({ schema: "rustright.cause-evidence.v2" }) } }).check(), (e) => e.code === "evidence" && /does not read/.test(e.message), argv.join(" "));
  }
});

// ---- GB-82: the fixture broker's receipts are the production forms ----
test("data: [GB-82] the fixture signs compact attribution with a detail file the production verifier accepts, an unavailable census fails the compile, and a nonzero exit is a signed FAILED", (t) => {
  const c = receiptCase(t);
  const { attribution } = c.check();
  assert.equal(attribution.units, 1); assert.equal(Object.hasOwn(attribution, "rows"), false, "no manufactured full row set in the signed receipt");
  const bytes = fs.readFileSync(attribution.detail.path);
  assert.deepEqual(verifyAttributionDetail(c.receipt, bytes).failedUnits, 0);
  // Unavailable census: typed unavailable evidence that fails a compile; without a reason, or not an object, it is refused as evidence.
  assert.throws(() => receiptCase(t, { tamper: "censusUnavailable" }).check(), (e) => e.code === "result" && /the broker signed FAILED/.test(e.message));
  for (const tamper of ["censusUnavailableNoReason", "freshnessNotObject"]) assert.throws(() => receiptCase(t, { tamper }).check(), (e) => e.code === "evidence", tamper);
  // A read-only form that carries no census is unaffected.
  assert.equal(receiptCase(t, { argv: ["metadata", "--no-deps"], tamper: "noStageCensus" }).check().payload.result.status, "SUCCEEDED");
  // A depth-2 nested child keeps its lineage (its parent is the earlier child, not the root).
  const fx = dataHost(t);
  const made = writeSnapshotRequest({ controlRoot: fx.controlRoot, commit: fx.sb.commit, snapshot: fx.sb.snapshot, declaredInputs: [], handle: fx.handle, primaryCwd: fx.checkout, argv: ["run", "x"], nonce: "a".repeat(64) });
  const child = (requestId, parentRequestId) => ({ requestId, parentRequestId, shim: "cargo", argv: ["build"], cwd: fx.snapshot, exitCode: 0, status: "SUCCEEDED", outputDir: path.join(fx.outputDir, "nested", requestId) });
  const receipt = signedReceipt(fx, { id: JOB, carrier: made.carrier, argv: ["run", "x"], workspace: fx.checkout, adapter: "rightkit-pnpm", children: [child("c1", "root"), child("c2", "c1")] });
  assert.deepEqual(receipt.nestedChildren.map((n) => [n.requestId, n.parentRequestId]), [["c1", JOB], ["c2", "c1"]]);
  const verified = verifySnapshotReceipt(receipt, { pins: fx.pins, jobId: JOB, carrier: made.carrier, argv: ["run", "x"], workspace: fx.checkout, adapter: "rightkit-pnpm", checkout: fx.checkout, outputRoots: [fx.wsRoot], platform: process.platform });
  assert.equal(verified.nestedChildren.length, 2);
});
