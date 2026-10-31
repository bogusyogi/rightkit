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
import { OWNERSHIP_ENV, OWNERSHIP_FILE_ENV, brokerLeaseState, markCarriedSend } from "../build-ownership.mjs";
import { adapterEnvRefusal, isReservedAdapterEnvKey } from "../adapter-env.mjs";
import { isVersionProbe } from "../cargo-shapes.mjs";
import { NATIVE_ADAPTERS, nativeGrammar } from "../native-adapters.mjs";
import { adapterArgvRefusal } from "../pnpm-adapter-grammar.mjs";
import { verifyAttributionDetail } from "../provenance-verify.mjs";
import { ATTEST_STAGE_NONCE_ENV, canonicalJson, signedBytes } from "../provenance-verify.mjs";
import { SNAPSHOT_MUST_COMPILE, acceptedAfterFix } from "./cargo-classification.mjs";
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

/**
 * RA-03: the cause evidence a real broker signs inside a compile's compact freshness (rustright-broker cargo_evidence.rs `Classification::compact`, schema
 * rustright.cause-evidence.v1). The default is the whole, explained evidence of a census that rebuilt nothing; `over` replaces fields of the evidence.
 */
export const causeEvidence = (over = {}) => ({
  schema: "rustright.cause-evidence.v1", state: "complete", verdict: "explained", basis: "reasons", unavailable: null, gaps: [], historyMigrationNeeded: false,
  counts: { rows: 0, rowsDropped: 0, units: 0, unexplained: 0, uncovered: 0 }, unexplainedUnits: [], unexplainedUnitsTruncated: 0, uncoveredUnits: [], uncoveredUnitsTruncated: 0, ...over,
});
/** A compile's compact freshness as `compact_freshness` signs it: the census, the per-cause counts, the unexplained count (null when unknown) and the cause evidence. */
export const explainedFreshness = (over = {}) => ({
  available: true, unitsAtStart: 3, fresh: 3, notFresh: 0, invalidationCounts: {}, causeCounts: {}, rebuiltUnits: [], rebuiltUnitsTotal: 0, unexplainedCount: 0, causeEvidence: causeEvidence(), ...over,
});

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
export function signedReceipt(fx, { id, carrier, argv, workspace, observedEnv = {}, adapterEnv = {}, tamper = "none", mutate = null, adapter = null, children = null, exit = 0 }) {
  const t = TAMPERS[tamper] ?? fail(`unknown tamper ${tamper}`);
  const inputs = readDeclaredInputs(carrier.snapshot.path);
  const evidence = {
    requestId: id, brokerVersion: "rustright-test", executor: fx.pins.host, commit: carrier.commit,
    snapshot: { path: carrier.snapshot.path, treeAtStart: carrier.snapshot.expectedTree, treeAtExit: carrier.snapshot.expectedTree }, declaredInputDigests: declaredInputDigestsOf(carrier.snapshot.path, inputs),
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

/**
 * A fixture broker host under a fresh temp root (`readOnlySnapshots` defaults to true: the broker accepts the carried read-only shapes of RustRight's cargo-shapes vector; false models a broker that predates that and takes compiles only): a checkout and the gate's snapshot (two Cargo workspaces, the root and `native`), a
 * data root with the managed `ws` generation target, keys and pins, a control root, and a fake `rustright` client script whose
 * behaviour a test sets with `setBehavior({ tamper, exit, refuse })` (see client below). `sb` is the `deps.snapshotBroker` the gate builds.
 */
export function makeFakeBroker(t, { inputs = null, readOnlySnapshots = true } = {}) {
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
  if (inputs) { write(snapshot, { ".rightkit/compile-inputs.json": JSON.stringify({ inputs }), "gen/in.txt": "declared input\n" }); }
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
    // The broker's lease status for a sub-handle, through the fake client's `snapshot-lease status` (the gate hands createSnapshotBroker brokerLeaseState).
    leaseStatus: ({ handle: asked, waitMs = 0 }) => brokerLeaseState({ snapshot, handle: asked, env: {}, platform: process.platform, client, waitMs }),
  };
  // Models a broker host whose checkout and snapshot are a door fixture's (its own snapshots root holds the one snapshot): the client reads these at each call.
  const rebase = ({ checkout: checkoutRoot, snapshotsRoot }) => {
    const doc = JSON.parse(fs.readFileSync(path.join(state, "fixture.json"), "utf8"));
    fs.writeFileSync(path.join(state, "fixture.json"), JSON.stringify({ ...doc, ...(checkoutRoot ? { checkout: checkoutRoot } : {}), ...(snapshotsRoot ? { snapshotsRoot } : {}) }));
  };
  return { ...fx, client, state, setBehavior, rebase, realSpawn: (file, args, options) => spawnSync(file, args, options), calls, adapterCalls, rootRequests, sb, submits, ownership, ownershipEnv, owner };
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
    const envVar = (name) => env[`RUSTRIGHT_${name}`]; // GB-09: aliases off, as RUSTRIGHT_LEGACY_ALIASES=off: the gate must emit the current spelling
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

/** The carrier's VALUES, not only its key sets (rustright-cli shims.rs snapshot_carrier): full object ids, an absolute snapshot path and primaryCwd, the commit's tree as expectedTree, a sub-handle, a 64-hex argv digest, a 64..128-hex nonce, string inputs. */
function carrierValuesProblem(c) {
  const oid = (v) => typeof v === "string" && /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/.test(v);
  if (c.schema !== "rustright.snapshot-request/1") return `unknown carrier schema ${JSON.stringify(c.schema)} (the legacy rightkit.snapshot-request/1 is not read: aliases off)`;
  if (!oid(c.commit?.sha) || !oid(c.commit?.tree)) return "commit sha and tree must be full lowercase object ids";
  if (typeof c.snapshot?.path !== "string" || !path.isAbsolute(c.snapshot.path) || c.snapshot.expectedTree !== c.commit.tree) return "the snapshot needs an absolute path and the commit's tree as expectedTree";
  if (typeof c.ownership?.handle !== "string" || !/^[^#\s]+#[A-Za-z0-9._-]+$/.test(c.ownership.handle)) return "the ownership handle must be a sub-handle <handle>#<token>";
  if (typeof c.primaryCwd !== "string" || !path.isAbsolute(c.primaryCwd)) return "primaryCwd must be absolute";
  if (typeof c.argvSha256 !== "string" || !/^[0-9a-f]{64}$/.test(c.argvSha256)) return "argvSha256 must be 64 hex characters";
  if (typeof c.nonce !== "string" || !/^[0-9a-f]{64,128}$/.test(c.nonce)) return "the nonce must be 64 to 128 lowercase hex characters";
  if (!Array.isArray(c.declaredInputs?.inputs) || c.declaredInputs.inputs.some((i) => typeof i !== "string" || i === "")) return "declaredInputs.inputs must be a list of non-empty strings";
  return null;
}

/**
 * The broker's one-completion-per-(snapshot, lease handle) rule (rustright snapshot.rs:330-343): a second carried request under a handle the
 * snapshot already has a completion for is refused ("snapshot ownership handle has a prior or unreadable completion").
 */
/**
 * rustright-cli shims.rs (GB-16): the carrier is accepted when canonicalize(primaryCwd) is the client's working directory (as before), or when primaryCwd is gone and the nearest existing directory
 * above it is the working directory; the request's workspace is then primaryCwd itself (the attestation hashes that workspace), else the working directory. Anything else is `bad("primaryCwd")`.
 */
function acceptedWorkspace(primaryCwd, bad) {
  const cwd = fs.realpathSync(process.cwd());
  let canonical = null;
  try { canonical = fs.realpathSync(primaryCwd); } catch (e) { if (e.code !== "ENOENT" && e.code !== "ENOTDIR") bad("primaryCwd"); }
  if (canonical !== null) { if (canonical !== cwd) bad("primaryCwd"); return cwd; }
  let ancestor = path.dirname(primaryCwd);
  for (;;) {
    try { if (fs.statSync(ancestor).isDirectory()) break; } catch { /* keep walking up */ }
    const up = path.dirname(ancestor);
    if (up === ancestor) bad("primaryCwd");
    ancestor = up;
  }
  if (fs.realpathSync(ancestor) !== cwd) bad("primaryCwd");
  return primaryCwd;
}

/**
 * A typed refusal the broker answers before it spawns anything (`behavior.refuseWith`: the RESULT event, exit 125), and a connection the busy pipe never opened (`behavior.busy`: the client's
 * RK_BROKER_BUSY failure, no byte sent). `refuseTimes` / `busyTimes` limit either to the first N calls (a refusal that clears, as recovery and the storage pass do). A refusal is a durable,
 * settled submission as the real broker records it (`record_refused`): the handle reads REAPED, `spawned: false`, `refused: <code>` in SNAPSHOT_LEASE_STATUS.members.
 */
function takeCount(stateDir, name, limit) {
  const file = path.join(stateDir, name);
  let n = 0; try { n = Number(fs.readFileSync(file, "utf8")) || 0; } catch { n = 0; }
  if (typeof limit === "number" && n >= limit) return false;
  fs.writeFileSync(file, String(n + 1));
  return true;
}
function maybeBusy(stateDir, behavior) {
  if (!behavior.busy || !takeCount(stateDir, "busy.count", behavior.busyTimes)) return;
  process.stderr.write("rustright: RK_BROKER_BUSY: RustRight broker busy: every pipe instance of \\\\.\\pipe\\rustright-broker stayed busy for 120s: All pipe instances are busy. (os error 231)\n");
  process.exit(1);
}
function maybeRefuse(stateDir, behavior, descriptor) {
  if (!behavior.refuseWith || !takeCount(stateDir, "refused.count", behavior.refuseTimes)) return;
  try {
    const carrier = JSON.parse(fs.readFileSync(descriptor.path, "utf8"));
    fs.appendFileSync(path.join(stateDir, "refusals.jsonl"), `${JSON.stringify({ snapshot: carrier.snapshot?.path, handle: carrier.ownership?.handle, code: behavior.refuseWith.code ?? "REFUSED" })}\n`);
  } catch { /* the carrier is read only to record the refused submission */ }
  process.stdout.write(`${JSON.stringify(behavior.refuseWith)}\n`);
  process.exit(125);
}

function claimHandle(stateDir, carrier, bad) {
  const dir = path.join(stateDir, "handles"); fs.mkdirSync(dir, { recursive: true });
  const file = path.join(dir, sha(`${carrier.snapshot.path}\0${carrier.ownership.handle}`));
  try { fs.writeFileSync(file, "", { flag: "wx" }); } catch { bad("snapshot ownership handle has a prior or unreadable completion"); }
  // The fixture job finishes inside this call, so the claim is also its durable completion (it survives a restart: it is a file); `heldHandles`, `unknownHandles` and
  // `refusedHandles` of the behavior model a lease still held, a start that may have run with no completion, and a refusal that ran nothing.
  fs.appendFileSync(path.join(stateDir, "leases.jsonl"), `${JSON.stringify({ snapshot: carrier.snapshot.path, handle: carrier.ownership.handle })}\n`);
}

/**
 * `SNAPSHOT_LEASE_STATUS` as the production lease table answers it (snapshot.rs Leases::status; vector lease-status-vectors.v1.json): a handle covers its own lease and every
 * `<handle>#<suffix>`; a handle never held (and a prefix of one, and another suffix) is UNKNOWN, never REAPED; REAPED needs a durable completion; the answer carries
 * `members` {handle, state, spawned, refused?, job?} sorted by handle and its `state` is only the aggregate: ACTIVE when any member is held, REAPED when there is at
 * least one member and every member is REAPED, else UNKNOWN. `settledAfterMs` models the lease table moving on while a caller waits: a held or unknown member of the behavior's
 * `settleAfterMs` list of handles (or of every held handle when it is a number) completes (`REAPED`, `spawned: true`) once the call has waited that long.
 */
function leaseStatus(stateDir, behavior, snapshot, wanted, settledAfterMs = 0) {
  const covers = (h) => h === wanted || h.startsWith(`${wanted}#`);
  const settles = typeof behavior.settleAfterMs === "number" ? behavior.settleAfterMs : null;
  const settled = settles !== null && settledAfterMs >= settles;
  const members = new Map();
  let lines = []; try { lines = fs.readFileSync(path.join(stateDir, "leases.jsonl"), "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)); } catch { /* none yet */ }
  for (const { handle, snapshot: where } of lines) if (where === snapshot && covers(handle)) members.set(handle, { handle, state: "REAPED", spawned: true });
  for (const handle of behavior.completedHandles ?? []) if (covers(handle)) members.set(handle, { handle, state: "REAPED", spawned: true });
  for (const [handle, code] of Object.entries(behavior.refusedHandles ?? {})) if (covers(handle)) members.set(handle, { handle, state: "REAPED", spawned: false, refused: code });
  let refusals = []; try { refusals = fs.readFileSync(path.join(stateDir, "refusals.jsonl"), "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)); } catch { /* none yet */ }
  for (const { handle, snapshot: where, code } of refusals) if (where === snapshot && covers(handle)) members.set(handle, { handle, state: "REAPED", spawned: false, refused: code });
  for (const handle of behavior.unknownHandles ?? []) if (covers(handle)) members.set(handle, { handle, state: settled ? "REAPED" : "UNKNOWN", spawned: true });
  for (const handle of behavior.heldHandles ?? []) if (covers(handle)) members.set(handle, { handle, state: settled ? "REAPED" : "ACTIVE", spawned: true });
  const rows = [...members.values()].sort((a, b) => (a.handle < b.handle ? -1 : a.handle > b.handle ? 1 : 0));
  const state = rows.some((m) => m.state === "ACTIVE") ? "ACTIVE" : rows.length > 0 && rows.every((m) => m.state === "REAPED") ? "REAPED" : "UNKNOWN";
  return { state, rows };
}

/** The longest wait a status query is given (rustright_protocol MAX_LEASE_STATUS_WAIT_MS); a larger one is clamped. The broker re-reads its table every 25 ms (LEASE_STATUS_POLL). */
const MAX_LEASE_STATUS_WAIT_MS = 30_000;
const LEASE_STATUS_POLL_MS = 25;

/**
 * The whole answer, as service.rs writes it: the aggregate state, the member rows, the snapshot and ownership as received, `waitedMs` (the time spent waiting) and `waitTimedOut`
 * (a wait was asked and the state is not REAPED at its bound). With `waitMs` the call returns as soon as the aggregate is REAPED, or at the bound with the state it has.
 */
async function leaseStatusAnswer(stateDir, behavior, snapshot, wanted, waitMs) {
  const wait = Math.min(Math.max(waitMs ?? 0, 0), MAX_LEASE_STATUS_WAIT_MS);
  const since = Date.now();
  let answer;
  for (;;) {
    const waited = Date.now() - since;
    answer = leaseStatus(stateDir, behavior, snapshot, wanted, waited);
    if (answer.state === "REAPED" || waited >= wait) break;
    await new Promise((resolve) => setTimeout(resolve, Math.min(LEASE_STATUS_POLL_MS, Math.max(1, wait - (Date.now() - since)))));
  }
  return { type: "SNAPSHOT_LEASE_STATUS", schema: "rustright.snapshot-lease-status.v1", state: answer.state, snapshot, ownership: wanted, members: answer.rows, waitedMs: Date.now() - since, waitTimedOut: wait > 0 && answer.state !== "REAPED" };
}

// ---- the fake client: `node fake-broker.mjs --client <state dir> cargo <args>` | `status <id>` ----
async function client(stateDir, [command, ...args]) {
  const read = (f, d = null) => { try { return JSON.parse(fs.readFileSync(path.join(stateDir, f), "utf8")); } catch { return d; } };
  const fx = { ...read("fixture.json"), privateKey: crypto.createPrivateKey(fs.readFileSync(path.join(stateDir, "key.pem"))) };
  if (fx.snapshotsRoot) fx.snapshot = path.join(fx.snapshotsRoot, fs.readdirSync(fx.snapshotsRoot).find((n) => !n.includes(".") && fs.statSync(path.join(fx.snapshotsRoot, n)).isDirectory()));
  const behavior = read("behavior.json", {});
  if (fx.requestLog) logRequest(fx, command, args);
  if (command === "cargo" || command === "run") maybeBusy(stateDir, behavior);
  if (command === "run") return adapterRun(stateDir, fx, behavior, args);
  if (command === "nested-cargo") return nestedCargo(stateDir, fx, behavior, args);
  if (command === "cargo") {
    const env = process.env;
    // The reader's own rules (rustright-cli shims.rs snapshot_carrier and the attested-run check): a current RUSTRIGHT_* name wins over the
    // legacy RIGHTKIT_* spelling, the descriptor is at most 4096 bytes with exactly `path` and `sha256`, the request file is a regular
    // file (never a link) directly in <control root>/snapshot-requests of at most 4 MiB whose fields are exactly the carrier's, cargo
    // only, no parent request, no admission-check call, a calling gate, and an attested call never rewrites its invocation.
    const envVar = (name) => env[`RUSTRIGHT_${name}`]; // GB-09: aliases off, as RUSTRIGHT_LEGACY_ALIASES=off: the gate must emit the current spelling
    const bad = (why) => { process.stderr.write(`invalid snapshot request carrier (${why})\n`); process.exit(2); };
    const raw = envVar("SNAPSHOT_REQUEST");
    // The real broker refuses an uncarried ROOT request whose workspace is inside a managed snapshot (service.rs: "a root request cannot run
    // inside a managed snapshot"). Every uncarried request is logged, so a test can count them (`rootRequests()`).
    if (raw === undefined && !isVersionProbe(args)) {
      const here = fs.realpathSync(process.cwd());
      const roots = [fx.snapshotsRoot, path.join(fx.dataRoot, "snapshots"), path.dirname(fx.snapshot)].filter(Boolean).map((r) => { try { return fs.realpathSync(r); } catch { return r; } });
      const inside = roots.some((r) => here === r || here.startsWith(`${r}${path.sep}`));
      fs.appendFileSync(path.join(stateDir, "rootrequests.jsonl"), `${JSON.stringify({ argv: args, cwd: here, refused: inside })}\n`);
      if (inside) { process.stderr.write("rustright: a root request cannot run inside a managed snapshot\n"); process.exit(2); }
      // A root `metadata` in the primary checkout is answered with the checkout's own paths (what the gate compares the snapshot's members with).
      if (args.includes("metadata")) { process.stdout.write(synthMetadata(fx, here, false)); process.exit(0); }
    }
    // The client's own version probe is a root request in a workspace of the primary checkout (the gate's tool probe); it reads no file.
    if (raw === undefined && isVersionProbe(args)) { process.stdout.write("cargo 1.90.0 (fake)\n"); process.exit(0); }
    if (raw === undefined) bad("no descriptor");
    if (Buffer.byteLength(raw) > 4096) bad("descriptor exceeds 4096 bytes");
    let descriptor; try { descriptor = JSON.parse(raw); } catch { bad("descriptor is not JSON"); }
    if (!descriptor || typeof descriptor !== "object" || Object.keys(descriptor).sort().join() !== "path,sha256" || !/^[0-9a-f]{64}$/.test(descriptor.sha256) || !path.isAbsolute(descriptor.path)) bad("descriptor fields");
    maybeRefuse(stateDir, behavior, descriptor); // GB-54: a typed REJECTED result, nothing spawned
    if (behavior.refuse) bad("refused by the broker");
    if (envVar("PARENT_REQUEST_ID") !== undefined) bad("a parent request is set");
    if (String(envVar("CHECK_ADMISSION") ?? "").trim() === "1") bad("an admission check is not a snapshot compile");
    if (!String(envVar("CALLING_GATE") ?? "").trim()) bad("no calling gate");
    if (args.some((a) => a === "--manifest-path" || a.startsWith("--manifest-path="))) bad("attestation: the client would rewrite this invocation (--manifest-path); run from the manifest's directory without it");
    // The broker admits a snapshot request only when it compiles (job_service/service.rs: "a snapshot request must compile"); read-only
    // subcommands (metadata, locate-project, --version, ...) are refused today. `readOnlySnapshots` models the broker after RustRight's change (the lock-bound allowlist, acceptedAfterFix).
    // GB-82: admission is the production grammar (cargo-shapes.mjs, exercised against RustRight's cargo-shapes vector), not a grammar of the fake's own.
    if (!acceptedAfterFix(args, { checkout: fx.checkout, readOnly: fx.readOnlySnapshots !== false })) { process.stderr.write(`${SNAPSHOT_MUST_COMPILE}\n`); process.exit(2); }
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
  { const problem = carrierValuesProblem(carrier); if (problem) bad(problem); }
    exactly(carrier.commit, ["sha", "tree"], "commit"); exactly(carrier.snapshot, ["path", "expectedTree"], "snapshot"); exactly(carrier.declaredInputs, ["inputs"], "declaredInputs"); exactly(carrier.ownership, ["handle"], "ownership");
    if (carrier.nonce !== envVar("ATTEST_NONCE")) bad("nonce");
    if (carrier.argvSha256 !== argvSha256(args)) bad("argv");
    const named = acceptedWorkspace(carrier.primaryCwd, bad);
    // A used carrier is a replay: the broker accepts each nonce once.
    const used = path.join(stateDir, "nonces"); fs.mkdirSync(used, { recursive: true });
    if (fs.existsSync(path.join(used, carrier.nonce))) bad("nonce replayed");
    claimHandle(stateDir, carrier, bad);
    fs.writeFileSync(path.join(used, carrier.nonce), "");
    const id = crypto.randomUUID();
    const workspace = named;
    fs.writeFileSync(envVar("REQUEST_ID_FILE"), JSON.stringify({ schema: "rustright.request-ref.v1", requestId: id, submittedAt: new Date().toISOString(), workspace }));
    fs.writeFileSync(path.join(stateDir, "db", `${id}.json`), JSON.stringify({ id, carrier, argv: args, workspace, observedEnv: observedEnvOf(env, process.platform), tamper: behavior.tamper ?? "none", exit: behavior.exit ?? 0 }));
    fs.appendFileSync(path.join(stateDir, "calls.jsonl"), `${JSON.stringify({ id, signingEnvNames: Object.keys(env).filter((k) => /^(TAURI_SIGNING_|TAURI_PRIVATE_KEY|TAURI_KEY_PASSWORD)/i.test(k)).sort(), debugEnvSeen: env.TAURI_DEBUG !== undefined, carrierEnvNames: Object.keys(env).filter((k) => /^(RUSTRIGHT|RIGHTKIT)_(SNAPSHOT_REQUEST|ATTEST_NONCE|REQUEST_ID_FILE|CALLING_GATE|PARENT_REQUEST_ID)$/.test(k)).sort(), argv: args, cwd: workspace, stageNonceSeen: env.RIGHTKIT_ATTEST_STAGE_NONCE !== undefined, managedEnvNames: Object.keys(env).filter((k) => MANAGED_RUST_ENVIRONMENT_KEYS.includes(k.toUpperCase())).sort(), env: { CARGO_TARGET_DIR: env.CARGO_TARGET_DIR ?? null, CARGO_BUILD_TARGET_DIR: env.CARGO_BUILD_TARGET_DIR ?? null } })}\n`);
    if (behavior.stdout) process.stdout.write(behavior.stdout);
    else if (args.includes("metadata")) process.stdout.write(synthMetadata(fx, workspace));
    process.exit(behavior.exit ?? 0);
  }
  if (command === "snapshot-lease" && args[0] === "status") {
    // rustright snapshot.rs:469: the status of a (bare) ownership handle covers every held handle equal to it or starting with `<it>#`.
    const flag = (name) => args[args.indexOf(name) + 1];
    const snapshot = flag("--snapshot"); const wanted = flag("--ownership");
    // `--wait-ms N` (GB-85): the CLI refuses anything but a non-negative integer, as its argument parser does.
    let waitMs = 0;
    if (args.includes("--wait-ms")) { const raw = flag("--wait-ms"); if (!/^\d+$/.test(String(raw))) { process.stderr.write(`error: invalid value '${raw}' for '--wait-ms <N>': not a non-negative integer\n`); process.exit(2); } waitMs = Number(raw); }
    process.stdout.write(`${JSON.stringify(await leaseStatusAnswer(stateDir, behavior, snapshot, wanted, waitMs))}\n`);
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
    const receipt = signedReceipt(fx, { id: entry.id, carrier: entry.carrier, argv: entry.argv, workspace: entry.workspace, observedEnv: entry.observedEnv, adapterEnv: entry.adapterEnv ?? {}, exit: entry.exit ?? 0, tamper: read("behavior.json", {}).statusTamper ?? entry.tamper, adapter: entry.adapter ?? null, children });
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
const ADAPTERS = ["rightkit-pnpm", "rightkit-node", "rightkit-qa", "rightkit-swift", "rightkit-xcodegen", "rightkit-xcodebuild", "rightkit-gradle", "rightkit-cmake"];
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
  // GB-51: a native-tool adapter judges its argv by the reference registration (the production parser's port), snapshot-bound.
  const native = Object.entries(NATIVE_ADAPTERS).find(([, entry]) => entry.adapter === adapter);
  if (native) { const problem = adapterArgvRefusal(args, nativeGrammar(native[0]), { snapshot: true }); if (problem) return problem; }
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
  const envVar = (name) => env[`RUSTRIGHT_${name}`]; // GB-09: aliases off, as RUSTRIGHT_LEGACY_ALIASES=off: the gate must emit the current spelling
  const bad = (why) => { process.stderr.write(`invalid snapshot request carrier (${why})\n`); process.exit(2); };
  if (!ADAPTERS.includes(adapter)) bad(`${adapter} is not a registered snapshot adapter`);
  if (rest[0] !== "--") bad("the adapter's arguments follow `--`");
  const args = rest.slice(1);
  const raw = envVar("SNAPSHOT_REQUEST");
  if (raw === undefined) bad("no descriptor (an adapter job in a snapshot is refused without its carrier)");
  if (Buffer.byteLength(raw) > 4096) bad("descriptor exceeds 4096 bytes");
  let descriptor; try { descriptor = JSON.parse(raw); } catch { bad("descriptor is not JSON"); }
  if (!descriptor || typeof descriptor !== "object" || Object.keys(descriptor).sort().join() !== "path,sha256" || !/^[0-9a-f]{64}$/.test(descriptor.sha256) || !path.isAbsolute(descriptor.path)) bad("descriptor fields");
  maybeRefuse(stateDir, behavior, descriptor); // GB-54: a typed REJECTED result, nothing spawned
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
  { const problem = carrierValuesProblem(carrier); if (problem) bad(problem); }
  if (carrier.nonce !== envVar("ATTEST_NONCE")) bad("nonce");
  if (carrier.argvSha256 !== argvSha256(args)) bad("argv");
  const named = acceptedWorkspace(carrier.primaryCwd, bad);
  if (fs.realpathSync(process.cwd()) !== fs.realpathSync(fx.checkout) && !fs.realpathSync(process.cwd()).startsWith(`${fs.realpathSync(fx.checkout)}${path.sep}`)) bad("the request's workspace is not inside the primary checkout");
  // The request environment: what the client forwards (shims.rs adapter_environment) and the broker's check_environment (a reserved or
  // unregistered name is refused; the registration is `behavior.registeredEnv[adapter]`, empty by default).
  // GB-82: the same judgement as the production parser (adapter-env.mjs adapterEnvRefusal, a port exercised against RustRight's vector): a reserved
  // key is broker-controlled except the two signed-artifact handoff names, a handoff key must be an absolute path, and a key the registration does not list refuses,
  // naming every unregistered key and the registered set.
  const forwarded = {};
  for (const key of String(envVar("ADAPTER_ENV") ?? "").split(",").map((k) => k.trim()).filter(Boolean)) {
    if (isReservedAdapterEnvKey(key)) { process.stderr.write(`RUSTRIGHT_ADAPTER_ENV names ${key}, which is broker-controlled and cannot be forwarded to an adapter\n`); process.exit(2); }
    if (env[key] === undefined) { process.stderr.write(`RUSTRIGHT_ADAPTER_ENV names ${key}, which is not set\n`); process.exit(2); }
    forwarded[key] = env[key];
  }
  const envProblem = adapterEnvRefusal(forwarded, behavior.registeredEnv?.[adapter] ?? []);
  if (envProblem) { process.stderr.write(`RK_ADAPTER_ENV_DENIED: ${envProblem.message}\n`); process.exit(2); }
  const denied = snapshotArgvProblem(fx, adapter, args);
  if (denied) { process.stderr.write(`RK_ADAPTER_ARGV_DENIED: ${denied}\n`); process.exit(2); }
  const used = path.join(stateDir, "nonces"); fs.mkdirSync(used, { recursive: true });
  if (fs.existsSync(path.join(used, carrier.nonce))) bad("nonce replayed");
  claimHandle(stateDir, carrier, bad);
  fs.writeFileSync(path.join(used, carrier.nonce), "");
  const id = crypto.randomUUID();
  // The request workspace of a snapshot-bound adapter job is the cwd the client ran in: the primary counterpart of the step's snapshot directory.
  const clientCwd = named;
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
  fs.appendFileSync(path.join(stateDir, "adaptercalls.jsonl"), `${JSON.stringify({ id, adapter, argv: args, cwd: clientCwd, workspace, primaryCwd: carrier.primaryCwd, jobCwd, jobEnvNames: Object.keys(jobEnv).sort(), forwardedEnv: forwarded, forwardedEnvNames: Object.keys(forwarded).sort(), signingEnvNames: Object.keys(env).filter((k) => /^(TAURI_SIGNING_|TAURI_PRIVATE_KEY|TAURI_KEY_PASSWORD)/i.test(k)).sort(), debugEnvSeen: env.TAURI_DEBUG !== undefined, jobPath: jobEnv.PATH, callerEnvNamesDropped: Object.keys(env).filter((k) => !(k in jobEnv) && /^(RUSTRIGHT|RIGHTKIT)_/.test(k)).sort(), stageNonceSeen: env.RIGHTKIT_ATTEST_STAGE_NONCE !== undefined })}\n`);
  { const entryFile = path.join(stateDir, "db", `${id}.json`); const entry = JSON.parse(fs.readFileSync(entryFile, "utf8")); fs.writeFileSync(entryFile, JSON.stringify({ ...entry, exit })); }
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
