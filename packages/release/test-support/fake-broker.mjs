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
import { SNAPSHOT_MUST_COMPILE, acceptedAfterFix, classifyCargo } from "./cargo-classification.mjs";
import { argvSha256, cargoSubcommandOf, declaredInputDigestsOf, observedEnvOf, readDeclaredInputs, requestDigestOf, snapshotRequestCore } from "../snapshot-request.mjs";

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
 * A fixture broker host under a fresh temp root (`readOnlySnapshots: true` models the post-fix broker that accepts read-only snapshot requests): a checkout and the gate's snapshot (two Cargo workspaces, the root and `native`), a
 * data root with the managed `ws` generation target, keys and pins, a control root, and a fake `rustright` client script whose
 * behaviour a test sets with `setBehavior({ tamper, exit, refuse })` (see client below). `sb` is the `deps.snapshotBroker` the gate builds.
 */
export function makeFakeBroker(t, { inputs = null, readOnlySnapshots = false } = {}) {
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
  fs.writeFileSync(path.join(state, "fixture.json"), JSON.stringify({ root, checkout, dataRoot, controlRoot, snapshot, wsRoot, outputDir, pins, readOnlySnapshots }));
  const client = path.join(root, "rustright");
  fs.writeFileSync(client, `#!/bin/sh\nexec ${JSON.stringify(process.execPath)} ${JSON.stringify(fileURLToPath(import.meta.url))} --client ${JSON.stringify(state)} "$@"\n`, { mode: 0o755 });
  const setBehavior = (b) => fs.writeFileSync(path.join(state, "behavior.json"), JSON.stringify(b));
  setBehavior({});
  const grantCalls = () => (fs.existsSync(path.join(state, "grantcalls.jsonl")) ? fs.readFileSync(path.join(state, "grantcalls.jsonl"), "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)) : []);
  const calls = () => (fs.existsSync(path.join(state, "calls.jsonl")) ? fs.readFileSync(path.join(state, "calls.jsonl"), "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l)) : []);
  const submits = [];
  const sb = {
    controlRoot, commit: { sha: COMMIT, tree: TREE }, snapshot: { path: snapshot, expectedTree: TREE }, declaredInputs: inputs ?? [], handle: HANDLE, pins,
    markBrokerSubmit: () => { submits.push(Date.now()); return { handle: HANDLE, broker: { submitted: true } }; },
    checkout, outputRoots: [wsRoot], platform: process.platform, client,
    receiptOf: async (id) => JSON.parse(spawnStatus(client, id)).receipt,
  };
  return { ...fx, client, state, setBehavior, calls, grantCalls, sb, submits };
}
import { spawnSync } from "../background-process.mjs";
const spawnStatus = (client, id) => { const r = spawnSync(client, ["status", id], { encoding: "utf8" }); if (r.status !== 0) throw new Error(`status ${id}: ${r.stderr}`); return r.stdout; };

// ---- the fake client: `node fake-broker.mjs --client <state dir> cargo <args>` | `status <id>` ----
async function client(stateDir, [command, ...args]) {
  const read = (f, d = null) => { try { return JSON.parse(fs.readFileSync(path.join(stateDir, f), "utf8")); } catch { return d; } };
  const fx = { ...read("fixture.json"), privateKey: crypto.createPrivateKey(fs.readFileSync(path.join(stateDir, "key.pem"))) };
  const behavior = read("behavior.json", {});
  if (command === "cargo" && (process.env.RUSTRIGHT_SNAPSHOT_GRANT !== undefined || process.env.RIGHTKIT_SNAPSHOT_GRANT !== undefined)) return await grantCall(stateDir, fx, behavior, args);
  if (command === "cargo") {
    const env = process.env;
    // The reader's own rules (rustright-cli shims.rs snapshot_carrier and the attested-run check): a current RUSTRIGHT_* name wins over the
    // legacy RIGHTKIT_* spelling, the descriptor is at most 4096 bytes with exactly `path` and `sha256`, the request file is a regular
    // file (never a link) directly in <control root>/snapshot-requests of at most 4 MiB whose fields are exactly the carrier's, cargo
    // only, no parent request, no admission-check call, a calling gate, and an attested call never rewrites its invocation.
    const envVar = (name) => env[`RUSTRIGHT_${name}`] ?? env[`RIGHTKIT_${name}`];
    const bad = (why) => { process.stderr.write(`invalid snapshot request carrier (${why})\n`); process.exit(2); };
    const raw = envVar("SNAPSHOT_REQUEST");
    if (raw === undefined) bad("no descriptor");
    if (Buffer.byteLength(raw) > 4096) bad("descriptor exceeds 4096 bytes");
    let descriptor; try { descriptor = JSON.parse(raw); } catch { bad("descriptor is not JSON"); }
    if (!descriptor || typeof descriptor !== "object" || Object.keys(descriptor).sort().join() !== "path,sha256" || !/^[0-9a-f]{64}$/.test(descriptor.sha256) || !path.isAbsolute(descriptor.path)) bad("descriptor fields");
    if (behavior.refuse) bad("refused by the broker");
    if (envVar("PARENT_REQUEST_ID") !== undefined) bad("a parent request is set");
    if (String(envVar("CHECK_ADMISSION") ?? "").trim() === "1") bad("an admission check is not a snapshot compile");
    if (!String(envVar("CALLING_GATE") ?? "").trim()) bad("no calling gate");
    if (args.some((a) => a === "--manifest-path" || a.startsWith("--manifest-path="))) bad("attestation: the client would rewrite this invocation (--manifest-path); run from the manifest's directory without it");
    // The broker admits a snapshot request only when it compiles (job_service/service.rs: "a snapshot request must compile"); read-only
    // subcommands (metadata, locate-project, --version, ...) are refused today. `readOnlySnapshots` models the broker after RustRight's change (the lock-bound allowlist, acceptedAfterFix).
    if (!(fx.readOnlySnapshots ? acceptedAfterFix(args) : classifyCargo(args) === "compile")) { process.stderr.write(`${SNAPSHOT_MUST_COMPILE}\n`); process.exit(2); }
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
    fs.writeFileSync(path.join(used, carrier.nonce), "");
    const id = crypto.randomUUID();
    const workspace = fs.realpathSync(process.cwd());
    fs.writeFileSync(envVar("REQUEST_ID_FILE"), JSON.stringify({ schema: "rustright.request-ref.v1", requestId: id, submittedAt: new Date().toISOString(), workspace }));
    fs.writeFileSync(path.join(stateDir, "db", `${id}.json`), JSON.stringify({ id, carrier, argv: args, workspace, observedEnv: observedEnvOf(env, process.platform), tamper: behavior.tamper ?? "none" }));
    fs.appendFileSync(path.join(stateDir, "calls.jsonl"), `${JSON.stringify({ id, carrierEnvNames: Object.keys(env).filter((k) => /^(RUSTRIGHT|RIGHTKIT)_(SNAPSHOT_REQUEST|ATTEST_NONCE|REQUEST_ID_FILE|CALLING_GATE|PARENT_REQUEST_ID)$/.test(k)).sort(), argv: args, cwd: workspace, env: { CARGO_TARGET_DIR: env.CARGO_TARGET_DIR ?? null, CARGO_BUILD_TARGET_DIR: env.CARGO_BUILD_TARGET_DIR ?? null } })}\n`);
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

// ---- the stage grant, as the broker enforces it (rustright.snapshot-grant/1; rightkit#1301) ----
// A third-party child's call: `RUSTRIGHT_SNAPSHOT_GRANT={"path","sha256"}` (current spelling only), no SNAPSHOT_REQUEST, no ATTEST_NONCE (the
// client mints one per call), a calling gate. The file is `grant-<uuid v4>.json`, a regular file directly in <control root>/snapshot-requests
// of at most 4 MiB, whose bytes hash to the descriptor's sha256; its body has exactly the schema's fields. A call is allowed when it is
// build or run with --locked or --frozen, `metadata --no-deps` or a carried read-only command, with no --config or -Z, every path inside
// the snapshot, its cwd inside an allowed workspace, before expiry, one at a time, and at most maxCalls distinct nonces.
const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const GRANT_FIELDS = ["schema", "grantId", "primary", "commit", "snapshot", "declaredInputs", "ownership", "allowedWorkspaces", "maxCalls", "expiresAt"];
async function grantCall(stateDir, fx, behavior, args) {
  const env = process.env;
  const refuse = (why) => { process.stderr.write(`invalid snapshot grant (${why})\n`); process.exit(2); };
  const inside = (child, parent) => { const rel = path.relative(parent, child); return rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel)); };
  const exactly = (value, keys, what) => { if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).sort().join() !== [...keys].sort().join()) refuse(`unknown or missing fields in ${what}`); };
  if (env.RIGHTKIT_SNAPSHOT_GRANT !== undefined) refuse("the legacy spelling");
  if (env.RUSTRIGHT_SNAPSHOT_REQUEST !== undefined || env.RIGHTKIT_SNAPSHOT_REQUEST !== undefined) refuse("a grant child sets no snapshot request");
  if (env.RUSTRIGHT_ATTEST_NONCE !== undefined || env.RIGHTKIT_ATTEST_NONCE !== undefined) refuse("a grant child leaves the nonce unset: the client mints one per call");
  if (!String(env.RUSTRIGHT_CALLING_GATE ?? "").trim()) refuse("no calling gate");
  const raw = env.RUSTRIGHT_SNAPSHOT_GRANT;
  if (Buffer.byteLength(raw) > 4096) refuse("descriptor exceeds 4096 bytes");
  let descriptor; try { descriptor = JSON.parse(raw); } catch { refuse("descriptor is not JSON"); }
  exactly(descriptor, ["path", "sha256"], "the descriptor");
  if (!/^[0-9a-f]{64}$/.test(descriptor.sha256) || !path.isAbsolute(descriptor.path)) refuse("descriptor fields");
  const dirPath = path.join(fx.controlRoot, "snapshot-requests");
  if (fs.lstatSync(dirPath).isSymbolicLink()) refuse("snapshot-requests is a link");
  if (fs.realpathSync(path.dirname(descriptor.path)) !== fs.realpathSync(dirPath)) refuse("not directly in snapshot-requests");
  const name = path.basename(descriptor.path);
  const named = /^grant-(.+)\.json$/.exec(name);
  if (!named || !UUID_V4.test(named[1])) refuse("the file is not named grant-<uuid v4>.json");
  const meta = fs.lstatSync(descriptor.path);
  if (!meta.isFile() || meta.isSymbolicLink()) refuse("the grant file is not a regular file");
  if (meta.size > 4 * 1024 * 1024) refuse("the grant file exceeds 4 MiB");
  const bytes = fs.readFileSync(descriptor.path);
  if (sha(bytes) !== descriptor.sha256) refuse("the grant file does not match its sha256");
  const body = JSON.parse(bytes);
  exactly(body, GRANT_FIELDS, "the grant");
  if (body.schema !== "rustright.snapshot-grant/1") refuse("schema");
  if (body.grantId !== named[1]) refuse("grantId is not the file's");
  if (!path.isAbsolute(body.primary)) refuse("primary is not absolute");
  exactly(body.commit, ["sha", "tree"], "commit"); exactly(body.snapshot, ["path", "expectedTree"], "snapshot"); exactly(body.declaredInputs, ["inputs"], "declaredInputs"); exactly(body.ownership, ["handle"], "ownership");
  if (body.snapshot.expectedTree !== body.commit.tree) refuse("expectedTree is not the commit's tree");
  if (!Array.isArray(body.allowedWorkspaces) || !body.allowedWorkspaces.length || !body.allowedWorkspaces.every((w) => path.isAbsolute(w) && inside(w, body.snapshot.path))) refuse("allowedWorkspaces must be non-empty absolute directories inside the snapshot");
  if (!Number.isInteger(body.maxCalls) || body.maxCalls < 1 || body.maxCalls > 4096) refuse("maxCalls is 1 to 4096");
  if (!/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?Z$/.test(body.expiresAt)) refuse("expiresAt is not RFC3339 UTC");
  if (Date.parse(body.expiresAt) <= Date.now()) refuse("the grant has expired");
  // The call itself.
  const own = args.includes("--") ? args.slice(0, args.indexOf("--")) : args;
  if (own.some((w) => w === "--config" || w.startsWith("--config=") || w === "-Z" || (/^-Z/.test(w)))) refuse("--config and -Z are never allowed");
  const sub = cargoSubcommandOf(args);
  const locked = own.includes("--locked") || own.includes("--frozen");
  const allowed = ((sub === "build" || sub === "run") && locked) || (sub === "metadata" && own.includes("--no-deps")) || ["locate-project", "version", "help"].includes(sub) || args[0] === "--version";
  if (!allowed) refuse(`cargo ${args.join(" ")} is not a grant-carried command (build and run need --locked or --frozen)`);
  const cwd = fs.realpathSync(process.cwd());
  if (!body.allowedWorkspaces.some((w) => inside(cwd, w))) refuse("the cwd is outside every allowed workspace");
  for (let i = 0; i < own.length; i += 1) {
    const opt = ["--manifest-path", "--target-dir"].find((o) => own[i] === o || own[i].startsWith(`${o}=`));
    if (!opt) continue;
    const value = own[i].includes("=") ? own[i].slice(opt.length + 1) : own[i + 1];
    if (!value || !inside(path.resolve(cwd, value), body.snapshot.path)) refuse(`${opt} ${value} is outside the snapshot`);
  }
  // One at a time, and at most maxCalls distinct nonces (the client mints a fresh one for each call).
  const dir = path.join(stateDir, "grants"); fs.mkdirSync(dir, { recursive: true });
  const busy = path.join(dir, `${body.grantId}.busy`);
  try { fs.writeFileSync(busy, "", { flag: "wx" }); } catch { refuse("a call of this grant is already running: calls run one at a time"); }
  try {
    const usedFile = path.join(dir, `${body.grantId}.nonces.json`);
    const used = fs.existsSync(usedFile) ? JSON.parse(fs.readFileSync(usedFile, "utf8")) : [];
    if (used.length >= body.maxCalls) refuse(`the grant's ${body.maxCalls} calls are used`);
    const nonce = crypto.randomBytes(32).toString("hex");
    fs.writeFileSync(usedFile, JSON.stringify([...used, nonce]));
    fs.appendFileSync(path.join(stateDir, "grantcalls.jsonl"), `${JSON.stringify({ grantId: body.grantId, argv: args, cwd, nonce, envNames: Object.keys(env).filter((k) => /^(RUSTRIGHT|RIGHTKIT)_/.test(k)).sort() })}\n`);
    if (behavior.holdMs) await new Promise((resolve) => setTimeout(resolve, behavior.holdMs));
  } finally { fs.rmSync(busy, { force: true }); }
  process.exit(0);
}

if (process.argv[1] && (() => { try { return fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; } })() && process.argv[2] === "--client") {
  client(process.argv[3], process.argv.slice(4)).catch((e) => { process.stderr.write(`${e.stack}\n`); process.exit(3); });
}
export { canonicalJson };
