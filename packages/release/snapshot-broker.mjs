// The gate's production seam for a broker snapshot compile (item 7 rows 7.20 to 7.24; GATE-CONTRACT "Broker interface").
//
//   createSnapshotBroker   builds `deps.snapshotBroker` for runScans on a broker host (Mac, Dell): the commit, the snapshot, the
//                          build-ownership handle, the declared inputs READ FROM THE SNAPSHOT, the host's enrolled pins (broker-pins.mjs,
//                          never the receipt, an environment variable or a flag), the receipt reader and the managed output roots.
//   runBrokerCargo         one cargo call reached by a test on the Mac, through the runtime wrapper's broker mode: its own fresh
//                          nonce and carrier, `primaryCwd` from the gate's snapshot-to-checkout map, `callingGate` inherited and never
//                          forged, the carrier removed after the call, the signed receipt verified, and a ledger record either way.
//   carryBrokerAdapter     one pnpm, node or QA step of a snapshot stage (a broker ADAPTER job, rustright#190 snapshot-adapter-v1), carried
//                          the same way: its own carrier and nonce, the cwd of the primary checkout that matches the step's snapshot cwd,
//                          the signed receipt and its signed list of nested children (the Tauri CLI's compiles) verified, a ledger record.
//   verifyBrokerLedger     the gate's own re-verification of every ledgered call from the broker's signed receipt, with the gate's
//                          own commit, snapshot, ownership handle and pins (never the wrapper's say-so), and one use per carrier.
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { loadBrokerPins } from "./broker-pins.mjs";
import { ATTEST_STAGE_NONCE_ENV, canonicalJson, carriedCallNonce } from "./provenance-verify.mjs";
import { resolveManagedRoots } from "./managed-roots.mjs";
import { forwardedAdapterEnv } from "./adapter-env.mjs";
import {
  SnapshotRequestError, argvSha256, isCallHandleOf, lockedArgv, observedEnvOf, readDeclaredInputs, readRequestRef, verifySnapshotReceipt, writeSnapshotRequest,
} from "./snapshot-request.mjs";

/** The status a refused or unverified call returns to its caller: cargo's own error status. */
export const BROKER_REFUSAL_STATUS = 101;
export const CALLING_GATE_ENV = "RUSTRIGHT_CALLING_GATE";

/** The managed `ws` roots a broker pins its generation targets under (BROKER-CONTRACT output pinning). */
export const brokerOutputRoots = ({ dataRoot }) => [path.join(dataRoot, "rightkit-build-control", "ws")];

/** The broker's receipt of one request, read through the RustRight client (`rustright status <id>`), synchronously. */
export function receiptSync({ client, requestId, env, spawn = spawnSync }) {
  const r = spawn(client, ["status", requestId], { encoding: "utf8", env, timeout: 60_000, maxBuffer: 64 * 1024 * 1024 });
  if (r.error || r.status !== 0) throw new SnapshotRequestError("receipt", `rustright status ${requestId} failed (${r.error?.message ?? `exit ${r.status}`})`);
  let doc;
  try { doc = JSON.parse(String(r.stdout)); } catch { throw new SnapshotRequestError("receipt", `rustright status ${requestId} printed no JSON`); }
  // The status envelope is `{type:"STATUS", id, state, ..., receipt}`: `receipt` is null while the job is live and full once it is
  // terminal. Polling is not allowed, so a null receipt after the cargo process exited is a clear error, never a retry.
  if (!doc || typeof doc !== "object" || !Object.hasOwn(doc, "receipt")) throw new SnapshotRequestError("receipt", `rustright status ${requestId} returned no receipt field (not a STATUS envelope)`);
  if (doc.receipt === null) throw new SnapshotRequestError("receipt-not-ready", `rustright status ${requestId} reports the job ${doc.state ?? "state unknown"} with no receipt yet, although its cargo process exited; the gate does not poll, so this call is unverified`);
  if (typeof doc.receipt !== "object" || Array.isArray(doc.receipt)) throw new SnapshotRequestError("receipt", `rustright status ${requestId} returned a receipt that is not an object`);
  return doc.receipt;
}

/**
 * `deps.snapshotBroker` for a production run on a broker host. Throws when anything the compile owes cannot be established:
 * no build ownership, no RustRight client, no pins (an app repository without an enrollment), a malformed declared-inputs manifest.
 * There is no way to hand it pins: tests build their own object and inject it as `deps.snapshotBroker`.
 */
export function createSnapshotBroker({ env = process.env, platform = process.platform, checkout, commit, snapshot, ownership, client, gateFile, dataRoot, spawn = spawnSync, loadPins = loadBrokerPins }) {
  const need = (ok, why) => { if (!ok) throw new SnapshotRequestError("broker", why); };
  need(checkout && path.isAbsolute(checkout), "the primary checkout must be an absolute path");
  need(commit?.sha && commit?.tree, "the commit and its tree are required");
  need(snapshot?.path && path.isAbsolute(snapshot.path) && snapshot.expectedTree === commit.tree, "the snapshot needs an absolute path and the commit's tree as expectedTree");
  need(ownership && typeof ownership.handle === "string" && ownership.handle && typeof ownership.markBrokerSubmit === "function", "a broker snapshot compile needs the gate's build ownership (handle and submit hook)");
  need(typeof client === "string" && path.isAbsolute(client), "a broker snapshot compile needs the RustRight client (`rustright`) by absolute path");
  const roots = resolveManagedRoots({ env, platform });
  const managedData = dataRoot ?? roots.dataRoot;
  const pins = loadPins({ dataRoot: managedData, platform, gateFile, checkout, spawn });
  return {
    controlRoot: roots.controlRoot, commit: { sha: commit.sha, tree: commit.tree }, snapshot: { path: snapshot.path, expectedTree: snapshot.expectedTree },
    declaredInputs: readDeclaredInputs(snapshot.path), handle: ownership.handle, markBrokerSubmit: () => ownership.markBrokerSubmit(),
    pins: { host: pins.host, keyId: pins.keyId, publicKey: pins.publicKey, brokerDigest: pins.brokerDigest }, pinsSource: pins.source,
    checkout: fs.realpathSync.native(checkout), outputRoots: brokerOutputRoots({ dataRoot: managedData }), platform, client,
    receiptOf: async (requestId) => receiptSync({ client, requestId, env, spawn }),
  };
}

/**
 * What the production gate hands the door as `o.snapshotBroker`: the statement that this gate carries the RustRight snapshot carrier.
 * The door only asks whether a broker host may take the snapshot route; the broker itself (pins, declared inputs, ownership, receipt
 * reader) is built by `createSnapshotBroker` where the snapshot compile runs.
 */
export const PRODUCTION_SNAPSHOT_CARRIER = Object.freeze({ kind: "production", create: createSnapshotBroker });

/** The part of a snapshot broker the wrapper process needs, as the map file carries it (public values only). */
export const brokerSection = (sb, { sentinel = null } = {}) => ({
  client: sb.client, controlRoot: sb.controlRoot, commit: sb.commit, snapshot: sb.snapshot, handle: sb.handle, checkout: sb.checkout,
  pins: sb.pins, outputRoots: sb.outputRoots, platform: sb.platform, sentinel,
});

const escapes = (rel) => rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel);
const inside = (child, parent) => { const rel = path.relative(parent, child); return rel === "" || !escapes(rel); };

/** Per-call carrier variables, in both spellings (rustright-cli shims.rs LEGACY_ENV_SPELLINGS: the current RUSTRIGHT_* name wins over the legacy one). */
const CARRIER_CALL_VARIABLES = ["SNAPSHOT_REQUEST", "ATTEST_NONCE", "REQUEST_ID_FILE", "PARENT_REQUEST_ID", "PARENT_CAPABILITY_TOKEN", "SNAPSHOT_ADMISSION"];
/** The carrier variables of an environment, by name: a call's carrier is set by the gate for that one call and never inherited. */
export function inheritedCarrierEnv(env) {
  const found = [];
  for (const [key, value] of Object.entries(env ?? {})) {
    const upper = key.toUpperCase();
    const bare = upper.startsWith("RUSTRIGHT_") ? upper.slice("RUSTRIGHT_".length) : upper.startsWith("RIGHTKIT_") ? upper.slice("RIGHTKIT_".length) : null;
    if (bare === null) continue;
    if (CARRIER_CALL_VARIABLES.includes(bare) || (bare === "CHECK_ADMISSION" && String(value).trim() === "1")) found.push(key);
  }
  return found;
}

/**
 * One carried cargo call: its own fresh nonce and carrier, `primaryCwd` from the gate's snapshot-to-checkout map, `callingGate`
 * inherited and never forged, the carrier removed after the call, the signed receipt verified, and a ledger record either way.
 * `workspace` is the real snapshot path of the cargo workspace the call resolved; `checkoutWorkspace` its counterpart in the primary
 * checkout, from the gate's own map. `stdio` is the client's (`inherit`, or `capture` for a caller that reads cargo's output).
 * Returns `{ status, outcome, stdout, stderr }`; `outcome` is the ledgered record's outcome (`verified` with `outputDir`, or not).
 * Refused before anything runs: a carrier or per-call carrier variable already in the environment, no calling gate, a working
 * directory with no counterpart in the checkout. An output directory of the caller's is dropped (the broker pins it).
 */
export function carryBrokerCargo({ words, cwd, env, section, workspace, checkoutWorkspace, record, spawn = spawnSync, err, tmp = os.tmpdir(), stdio = "inherit" }) {
  words = lockedArgv(words); // a build or run is always locked (snapshot-request.mjs lockedArgv)
  const id = crypto.randomUUID();
  const base = { id, mode: "broker", cwd, argv: [...words] };
  const refuse = (code, message) => { record({ ...base, status: "refused", code, message }); err(`rightkit-gate: ${code}: ${message}`); return { status: BROKER_REFUSAL_STATUS, outcome: { status: "refused", code, message }, stdout: "", stderr: `rightkit-gate: ${code}: ${message}\n` }; };
  const gate = env[CALLING_GATE_ENV];
  if (typeof gate !== "string" || !gate.trim()) return refuse("RK_CARGO_BROKER_NO_GATE", `${CALLING_GATE_ENV} is not set: the call names no gate, and the wrapper never forges one`);
  const inherited = inheritedCarrierEnv(env);
  if (inherited.length) return refuse("RK_CARGO_BROKER_INHERITED_ENV", `${inherited.join(", ")} ${inherited.length > 1 ? "are" : "is"} already in the environment; a carrier is set by the gate for one call only, so a nested or replayed call is refused`);
  // The broker pins the output directory, so a carried call passes none: the caller's CARGO_TARGET_DIR (and the gate's unbound sentinel)
  // are dropped here. The receipt's `outputDir` is verified to be the broker's generation target under the managed `ws` root, outside the
  // snapshot and the checkout, whatever the environment held, so dropping it can only make the call safer.
  const clientEnv = { ...env };
  for (const key of Object.keys(clientEnv)) if (/^(CARGO_TARGET_DIR|CARGO_BUILD_TARGET_DIR)$/i.test(key)) delete clientEnv[key];
  // There is no stage grant any more (one model: a snapshot stage's compiles run nested under an adapter job); an inherited one is dropped, never forwarded.
  for (const key of Object.keys(clientEnv)) if (/^(RUSTRIGHT|RIGHTKIT)_SNAPSHOT_GRANT$/i.test(key)) delete clientEnv[key];
  // A release-bound stage's nonce (RIGHTKIT_ATTEST_STAGE_NONCE) is read here and never reaches the client: this call's nonce is derived from it.
  const stageNonce = env[ATTEST_STAGE_NONCE_ENV];
  for (const key of Object.keys(clientEnv)) if (key.toUpperCase() === ATTEST_STAGE_NONCE_ENV) delete clientEnv[key];
  let realCwd; let checkoutCwd;
  try {
    realCwd = fs.realpathSync.native(cwd);
    const rel = path.relative(workspace, realCwd);
    if (escapes(rel)) throw new Error(`${realCwd} is not inside the workspace ${workspace}`);
    checkoutCwd = fs.realpathSync.native(path.join(checkoutWorkspace, rel));
    if (!fs.statSync(checkoutCwd).isDirectory()) throw new Error(`${checkoutCwd} is not a directory`);
    if (!inside(checkoutCwd, section.checkout)) throw new Error(`${checkoutCwd} is outside the primary checkout ${section.checkout}`);
  } catch (e) { return refuse("RK_CARGO_BROKER_CWD", `the call's working directory has no counterpart in the primary checkout (${e.message}); the broker runs it where the checkout has it`); }
  let inputs;
  try { inputs = readDeclaredInputs(section.snapshot.path); } catch (e) { return refuse("RK_CARGO_BROKER_DECLARED_INPUTS", e.message); }
  const refDir = fs.mkdtempSync(path.join(tmp, "rk-snapreq-"));
  let made = null;
  try {
    try {
      // Under a release-bound stage the nonce is derived from the stage's, this call's directory (relative to the primary checkout, posix) and
      // its argv, so the hosted admission can recompute it from the stage's rows (provenance-verify.mjs carriedCallNonce); otherwise it is fresh.
      const nonce = /^[0-9a-f]{64}$/.test(stageNonce ?? "") ? carriedCallNonce({ stageNonce, cwd: path.relative(section.checkout, checkoutCwd).split(path.sep).join("/") || ".", argv: words }) : undefined;
      made = writeSnapshotRequest({ controlRoot: section.controlRoot, commit: section.commit, snapshot: section.snapshot, declaredInputs: inputs, handle: section.handle, primaryCwd: checkoutCwd, argv: words, nonce, requestIdFile: path.join(refDir, "request.json") });
    } catch (e) { return refuse("RK_CARGO_BROKER_CARRIER", e.message); }
    try { record({ ...base, status: "pending", nonce: made.carrier.nonce, ownershipHandle: made.carrier.ownership.handle, checkoutCwd }); } catch (e) { fs.rmSync(made.file, { force: true }); return refuse("RK_CARGO_LEDGER_ERROR", `the call cannot be recorded (${e.message}); it does not run`); }
    const childEnv = { ...clientEnv, ...made.env };
    const capture = stdio === "capture";
    const r = spawn(section.client, ["cargo", ...words], capture ? { cwd: checkoutCwd, env: childEnv, encoding: "utf8", maxBuffer: 1 << 28, windowsHide: true } : { cwd: checkoutCwd, env: childEnv, stdio: "inherit" });
    fs.rmSync(made.file, { force: true });
    let outcome;
    let ref = null;
    try {
      if (r.error) throw new SnapshotRequestError("spawn", `the RustRight client did not start (${r.error.message})`);
      ref = readRequestRef(path.join(refDir, "request.json"));
      const receipt = receiptSync({ client: section.client, requestId: ref.requestId, env: clientEnv, spawn });
      const observedEnv = observedEnvOf(clientEnv, section.platform);
      const { evidence, attribution, toolchain } = verifySnapshotReceipt(receipt, { pins: section.pins, jobId: ref.requestId, carrier: made.carrier, argv: words, workspace: ref.workspace, observedEnv, checkout: section.checkout, outputRoots: section.outputRoots, platform: section.platform });
      // attribution and toolchain are recorded as the broker signed them (a consumer that needs per-package attribution treats available:false as FAILED).
      outcome = { status: "verified", requestId: ref.requestId, workspace: ref.workspace, observedEnv, outputDir: evidence.outputDir, treeAtStart: evidence.snapshot.treeAtStart, treeAtExit: evidence.snapshot.treeAtExit, attribution, toolchain };
    } catch (e) {
      outcome = { status: "unverified", code: e.code ?? "error", message: e.message, ...(ref ? { requestId: ref.requestId, workspace: ref.workspace } : {}) };
    }
    record({ ...base, ...outcome, nonce: made.carrier.nonce, ownershipHandle: made.carrier.ownership.handle, checkoutCwd, clientExit: r.status ?? null });
    const out = capture ? { stdout: String(r.stdout ?? ""), stderr: String(r.stderr ?? "") } : { stdout: "", stderr: "" };
    if (outcome.status !== "verified") {
      const message = `rightkit-gate: RK_CARGO_BROKER_UNVERIFIED: \`cargo ${words.join(" ")}\` has no verified signed receipt (${outcome.code}: ${outcome.message}); the scan fails`;
      err(message);
      return { status: r.status ? r.status : BROKER_REFUSAL_STATUS, outcome, ...out, stderr: `${out.stderr}${message}\n` };
    }
    return { status: r.status ?? (r.signal ? 128 : BROKER_REFUSAL_STATUS), outcome, ...out };
  } finally {
    if (made) fs.rmSync(made.file, { force: true });
    fs.rmSync(refDir, { recursive: true, force: true });
  }
}

/**
 * One carried adapter call: a pnpm, node or QA step of a snapshot stage as a broker adapter job (`rustright run <adapter> -- <words>`), with
 * its own fresh (or, under a release-bound stage, derived) nonce and carrier. The step runs from `cwd`, a directory of the snapshot; the broker
 * runs it from the primary checkout's counterpart of that directory (`checkout` + the path of `cwd` below the snapshot root, both from the gate's
 * own `section`, never a Cargo workspace), and that directory is the carrier's `primaryCwd` and the client's own cwd (a per-call carrier, never a
 * grant: a grant applies only to a call whose cwd is inside the grant's snapshot); the request's workspace is the primary toplevel. The carrier is removed after the call, the signed receipt is verified (its snapshotEvidence, its
 * request digest over the adapter core, and its signed and mirrored `nestedChildren` list: every child exited 0 inside the snapshot, each with a managed
 * output directory), and a ledger record is written either way. Returns `{ status, outcome, stdout, stderr }`; `outcome` is `verified` with the
 * job's `outputDir` and the verified `nestedChildren`, or not.
 * Refused before anything runs: a carrier or per-call carrier variable already in the environment (so a call from inside a broker job, which
 * has a parent request, is refused), no calling gate, a working directory outside the snapshot or with no counterpart in the checkout.
 */
export function carryBrokerAdapter({ adapter, words, cwd, env, section, record, spawn = spawnSync, err, tmp = os.tmpdir(), stdio = "inherit" }) {
  const id = crypto.randomUUID();
  words = [...words].map(String);
  const base = { id, mode: "broker", shim: "adapter", adapter, cwd, argv: [...words] };
  const refuse = (code, message) => { record({ ...base, status: "refused", code, message }); err(`rightkit-gate: ${code}: ${message}`); return { status: BROKER_REFUSAL_STATUS, outcome: { status: "refused", code, message }, stdout: "", stderr: `rightkit-gate: ${code}: ${message}\n` }; };
  const describe = `${adapter} ${words.join(" ")}`.trim();
  if (typeof adapter !== "string" || !/^[a-z][a-z0-9-]*$/.test(adapter)) return refuse("RK_ADAPTER_BROKER_ADAPTER", `${JSON.stringify(adapter)} is not a registered adapter name`);
  const gate = env[CALLING_GATE_ENV];
  if (typeof gate !== "string" || !gate.trim()) return refuse("RK_ADAPTER_BROKER_NO_GATE", `${CALLING_GATE_ENV} is not set: the call names no gate, and the carrier never forges one`);
  const inherited = inheritedCarrierEnv(env);
  if (inherited.length) return refuse("RK_ADAPTER_BROKER_INHERITED_ENV", `${inherited.join(", ")} ${inherited.length > 1 ? "are" : "is"} already in the environment; a carrier is set by the gate for one call only, so a nested or replayed call (or one from inside a broker job) is refused`);
  // The client's environment: the stage nonce is read here and never forwarded (this call's nonce is derived from it); no output directory
  // and no stage grant is forwarded either (the broker pins the output, and no grant exists).
  const clientEnv = { ...env };
  for (const key of Object.keys(clientEnv)) if (/^(CARGO_TARGET_DIR|CARGO_BUILD_TARGET_DIR)$/i.test(key) || /^(RUSTRIGHT|RIGHTKIT)_SNAPSHOT_GRANT$/i.test(key)) delete clientEnv[key];
  const stageNonce = env[ATTEST_STAGE_NONCE_ENV];
  for (const key of Object.keys(clientEnv)) if (key.toUpperCase() === ATTEST_STAGE_NONCE_ENV) delete clientEnv[key];
  // The request environment the client will forward (RUSTRIGHT_ADAPTER_ENV and the variables it names): bound into the request core below and
  // recorded in the ledger so the gate's own re-verification recomputes the same digest. A reserved or unset name is refused here, as the client would.
  let adapterEnv;
  try { adapterEnv = forwardedAdapterEnv(env); } catch (e) { return refuse(e.code ?? "RK_ADAPTER_ENV_INVALID", e.message); }
  base.adapterEnv = adapterEnv;
  let realCwd; let checkoutCwd;
  try {
    const snapshotRoot = fs.realpathSync.native(section.snapshot.path);
    realCwd = fs.realpathSync.native(cwd);
    const rel = path.relative(snapshotRoot, realCwd);
    if (escapes(rel)) throw new Error(`${realCwd} is not inside the snapshot ${snapshotRoot}`);
    checkoutCwd = fs.realpathSync.native(path.join(section.checkout, rel));
    if (!fs.statSync(checkoutCwd).isDirectory()) throw new Error(`${checkoutCwd} is not a directory`);
    if (!inside(checkoutCwd, section.checkout)) throw new Error(`${checkoutCwd} is outside the primary checkout ${section.checkout}`);
  } catch (e) { return refuse("RK_ADAPTER_BROKER_CWD", `the step's working directory has no counterpart in the primary checkout (${e.message}); the broker runs it where the checkout has it`); }
  let inputs;
  try { inputs = readDeclaredInputs(section.snapshot.path); } catch (e) { return refuse("RK_ADAPTER_BROKER_DECLARED_INPUTS", e.message); }
  const refDir = fs.mkdtempSync(path.join(tmp, "rk-snapreq-"));
  let made = null;
  try {
    try {
      const nonce = /^[0-9a-f]{64}$/.test(stageNonce ?? "") ? carriedCallNonce({ stageNonce, cwd: path.relative(section.checkout, checkoutCwd).split(path.sep).join("/") || ".", argv: words }) : undefined;
      made = writeSnapshotRequest({ controlRoot: section.controlRoot, commit: section.commit, snapshot: section.snapshot, declaredInputs: inputs, handle: section.handle, primaryCwd: checkoutCwd, argv: words, nonce, requestIdFile: path.join(refDir, "request.json") });
    } catch (e) { return refuse("RK_ADAPTER_BROKER_CARRIER", e.message); }
    try { record({ ...base, status: "pending", nonce: made.carrier.nonce, ownershipHandle: made.carrier.ownership.handle, checkoutCwd }); } catch (e) { fs.rmSync(made.file, { force: true }); return refuse("RK_CARGO_LEDGER_ERROR", `the call cannot be recorded (${e.message}); it does not run`); }
    const childEnv = { ...clientEnv, ...made.env };
    const capture = stdio === "capture";
    const r = spawn(section.client, ["run", adapter, "--", ...words], capture ? { cwd: checkoutCwd, env: childEnv, encoding: "utf8", maxBuffer: 1 << 28, windowsHide: true } : { cwd: checkoutCwd, env: childEnv, stdio: "inherit" });
    fs.rmSync(made.file, { force: true });
    let outcome;
    let ref = null;
    try {
      if (r.error) throw new SnapshotRequestError("spawn", `the RustRight client did not start (${r.error.message})`);
      ref = readRequestRef(path.join(refDir, "request.json"));
      // The workspace root of a snapshot-bound adapter job is the bound primary toplevel (no Cargo.toml needed), never the step's own directory.
      if (fs.realpathSync.native(ref.workspace) !== fs.realpathSync.native(section.checkout)) throw new SnapshotRequestError("workspace", `the request's workspace ${ref.workspace} is not the primary toplevel ${section.checkout}`);
      const receipt = receiptSync({ client: section.client, requestId: ref.requestId, env: clientEnv, spawn });
      const { evidence, attribution, toolchain, nestedChildren } = verifySnapshotReceipt(receipt, { pins: section.pins, jobId: ref.requestId, carrier: made.carrier, argv: words, workspace: ref.workspace, adapter, adapterEnv, checkout: section.checkout, outputRoots: section.outputRoots, platform: section.platform });
      outcome = { status: "verified", requestId: ref.requestId, workspace: ref.workspace, outputDir: evidence.outputDir, nestedChildren, treeAtStart: evidence.snapshot.treeAtStart, treeAtExit: evidence.snapshot.treeAtExit, attribution, toolchain };
    } catch (e) {
      outcome = { status: "unverified", code: e.code ?? "error", message: e.message, ...(ref ? { requestId: ref.requestId, workspace: ref.workspace } : {}) };
    }
    record({ ...base, ...outcome, nonce: made.carrier.nonce, ownershipHandle: made.carrier.ownership.handle, checkoutCwd, clientExit: r.status ?? null });
    const out = capture ? { stdout: String(r.stdout ?? ""), stderr: String(r.stderr ?? "") } : { stdout: "", stderr: "" };
    if (outcome.status !== "verified") {
      const message = `rightkit-gate: RK_ADAPTER_BROKER_UNVERIFIED: \`${describe}\` has no verified signed receipt (${outcome.code}: ${outcome.message}); the stage fails`;
      err(message);
      return { status: r.status ? r.status : BROKER_REFUSAL_STATUS, outcome, ...out, stderr: `${out.stderr}${message}\n` };
    }
    return { status: r.status ?? (r.signal ? 128 : BROKER_REFUSAL_STATUS), outcome, ...out };
  } finally {
    if (made) fs.rmSync(made.file, { force: true });
    fs.rmSync(refDir, { recursive: true, force: true });
  }
}

/** One cargo call of a test, carried through the broker (`carryBrokerCargo`). Returns cargo's exit status. */
export const runBrokerCargo = (call) => carryBrokerCargo(call).status;

/**
 * The gate's own check of every ledgered broker call of one scan. A call counts only when the broker's signed receipt, fetched
 * again now, verifies against THIS gate's commit, snapshot, ownership handle and pins, and its carrier was used once: a nonce or a
 * request id that appears twice in the run (`seen`) is a replay. Returns the problems; none means every call verified.
 */
export async function verifyBrokerLedger(entries, sb, { seen = { nonces: new Set(), ids: new Set(), handles: new Set() } } = {}) {
  seen.handles ??= new Set();
  const problems = [];
  for (const e of entries) {
    if (e.mode !== "broker" || e.status !== "verified") continue;
    const adapter = typeof e.adapter === "string" && e.adapter ? e.adapter : null;
    const call = `${adapter ?? "cargo"} ${(e.argv ?? []).join(" ")}`.trim();
    try {
      if (typeof e.nonce !== "string" || typeof e.requestId !== "string" || !Array.isArray(e.argv) || !e.argv.every((a) => typeof a === "string") || typeof e.workspace !== "string" || typeof e.checkoutCwd !== "string") throw new SnapshotRequestError("ledger", "the record is incomplete");
      if (seen.nonces.has(e.nonce) || seen.ids.has(e.requestId)) throw new SnapshotRequestError("replay", "a carrier or request was used twice; each call carries its own fresh nonce and is used once");
      if (e.adapterEnv !== undefined && !(e.adapterEnv && typeof e.adapterEnv === "object" && !Array.isArray(e.adapterEnv) && Object.values(e.adapterEnv).every((v) => typeof v === "string"))) throw new SnapshotRequestError("ledger", "the record's adapter environment is not a map of strings");
      if (adapter === null && e.adapterEnv !== undefined && Object.keys(e.adapterEnv).length) throw new SnapshotRequestError("ledger", "a cargo call carries no request environment");
      // One lease handle per call: the sub-handle the gate sent (recorded with the call) is exactly one of this ownership's, used once in the run;
      // the receipt must sign that very handle (verifySnapshotReceipt binds equality, never a prefix).
      if (!isCallHandleOf(e.ownershipHandle, sb.handle)) throw new SnapshotRequestError("ledger", `the record names no sub-handle of the build ownership ${sb.handle}`);
      if (seen.handles.has(e.ownershipHandle)) throw new SnapshotRequestError("replay", `lease handle ${e.ownershipHandle} was used by two calls; each carried request has its own`);
      seen.nonces.add(e.nonce); seen.ids.add(e.requestId); seen.handles.add(e.ownershipHandle);
      const workspace = fs.realpathSync.native(e.workspace);
      if (!inside(workspace, sb.checkout)) throw new SnapshotRequestError("ledger", `the request ran in ${workspace}, outside the primary checkout`);
      if (adapter === null && fs.realpathSync.native(e.checkoutCwd) !== workspace) throw new SnapshotRequestError("ledger", "the request's working directory is not the one the call recorded");
      if (adapter !== null) {
        // The workspace is the primary toplevel; the step's snapshot directory and the checkout directory the carrier named (primaryCwd) are the
        // same path below their two roots.
        if (workspace !== fs.realpathSync.native(sb.checkout)) throw new SnapshotRequestError("ledger", `the adapter request's workspace ${workspace} is not the primary toplevel ${sb.checkout}`);
        const below = path.relative(fs.realpathSync.native(sb.snapshot.path), fs.realpathSync.native(e.cwd));
        if (escapes(below)) throw new SnapshotRequestError("ledger", `the step ran from ${e.cwd}, outside the snapshot`);
        if (path.relative(sb.checkout, fs.realpathSync.native(e.checkoutCwd)) !== below) throw new SnapshotRequestError("ledger", `the carrier named ${e.checkoutCwd}, not the checkout counterpart of ${e.cwd}`);
      }
      // The gate's own truth: its commit, snapshot, handle and declared inputs; only the call's own argv, nonce and cwd come from the ledger.
      const carrier = {
        schema: "rightkit.snapshot-request/1", commit: sb.commit, snapshot: sb.snapshot, declaredInputs: { inputs: readDeclaredInputs(sb.snapshot.path) },
        ownership: { handle: e.ownershipHandle }, primaryCwd: fs.realpathSync.native(e.checkoutCwd), argvSha256: argvSha256(e.argv), nonce: e.nonce,
      };
      const receipt = await sb.receiptOf(e.requestId);
      const verified = verifySnapshotReceipt(receipt, { pins: sb.pins, jobId: e.requestId, carrier, argv: e.argv, workspace: e.workspace, observedEnv: e.observedEnv ?? {}, adapter, adapterEnv: e.adapterEnv ?? {}, checkout: sb.checkout, outputRoots: sb.outputRoots, platform: sb.platform });
      if (adapter !== null) {
        // The children the ledger records (the bundle root and the stage's output directories are read from them) are the signed ones, and no more.
        if (canonicalJson(e.nestedChildren ?? null) !== canonicalJson(verified.nestedChildren)) throw new SnapshotRequestError("nested", "the nested children the call recorded are not the ones the broker's receipt signs");
        for (const child of verified.nestedChildren) {
          if (seen.ids.has(child.requestId)) throw new SnapshotRequestError("replay", `nested child ${child.requestId} was reported twice in this stage`);
          seen.ids.add(child.requestId);
        }
      }
    } catch (error) { problems.push(`${call} (from ${e.cwd}): the gate's own verification of the broker's receipt failed (${error.code ?? "error"}: ${error.message})`); }
  }
  return problems;
}
