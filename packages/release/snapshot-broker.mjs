// The gate's production seam for a broker snapshot compile (item 7 rows 7.20 to 7.24; GATE-CONTRACT "Broker interface").
//
//   createSnapshotBroker   builds `deps.snapshotBroker` for runScans on a broker host (Mac, Dell): the commit, the snapshot, the
//                          build-ownership handle, the declared inputs READ FROM THE SNAPSHOT, the host's enrolled pins (broker-pins.mjs,
//                          never the receipt, an environment variable or a flag), the receipt reader and the managed output roots.
//   runBrokerCargo         one cargo call reached by a test on the Mac, through the runtime wrapper's broker mode: its own fresh
//                          nonce and carrier, `primaryCwd` from the gate's snapshot-to-checkout map, `callingGate` inherited and never
//                          forged, the carrier removed after the call, the signed receipt verified, and a ledger record either way.
//   verifyBrokerLedger     the gate's own re-verification of every ledgered call from the broker's signed receipt, with the gate's
//                          own commit, snapshot, ownership handle and pins (never the wrapper's say-so), and one use per carrier.
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { loadBrokerPins } from "./broker-pins.mjs";
import { resolveManagedRoots } from "./managed-roots.mjs";
import {
  SnapshotRequestError, argvSha256, observedEnvOf, readDeclaredInputs, readRequestRef, verifySnapshotReceipt, writeSnapshotRequest,
} from "./snapshot-request.mjs";

/** The status a refused or unverified call returns to its caller: cargo's own error status. */
export const BROKER_REFUSAL_STATUS = 101;
export const CALLING_GATE_ENV = "RIGHTKIT_CALLING_GATE";

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

/**
 * One cargo call of a test, carried through the broker. `workspace` is the real snapshot path of the cargo workspace the call
 * resolved; `checkoutWorkspace` its counterpart in the primary checkout, from the gate's own map. Returns cargo's exit status.
 * Every outcome is ledgered through `record` first or after, never skipped.
 */
export function runBrokerCargo({ words, cwd, env, section, workspace, checkoutWorkspace, record, spawn = spawnSync, err, tmp = os.tmpdir() }) {
  const id = crypto.randomUUID();
  const base = { id, mode: "broker", cwd, argv: [...words] };
  const refuse = (code, message) => { record({ ...base, status: "refused", code, message }); err(`rightkit-gate: ${code}: ${message}`); return BROKER_REFUSAL_STATUS; };
  const gate = env[CALLING_GATE_ENV];
  if (typeof gate !== "string" || !gate.trim()) return refuse("RK_CARGO_BROKER_NO_GATE", `${CALLING_GATE_ENV} is not set: the call names no gate, and the wrapper never forges one`);
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
  // The environment the client sees: the caller's, without the gate's unbound-sentinel output directory (the broker pins the output).
  const clientEnv = { ...env };
  for (const key of Object.keys(clientEnv)) {
    if (/^(CARGO_TARGET_DIR|CARGO_BUILD_TARGET_DIR)$/i.test(key) && section.sentinel && String(clientEnv[key]).startsWith(section.sentinel)) delete clientEnv[key];
  }
  const refDir = fs.mkdtempSync(path.join(tmp, "rk-snapreq-"));
  let made = null;
  try {
    try {
      made = writeSnapshotRequest({ controlRoot: section.controlRoot, commit: section.commit, snapshot: section.snapshot, declaredInputs: inputs, handle: section.handle, primaryCwd: checkoutCwd, argv: words, requestIdFile: path.join(refDir, "request.json") });
    } catch (e) { return refuse("RK_CARGO_BROKER_CARRIER", e.message); }
    try { record({ ...base, status: "pending", nonce: made.carrier.nonce, checkoutCwd }); } catch (e) { fs.rmSync(made.file, { force: true }); return refuse("RK_CARGO_LEDGER_ERROR", `the call cannot be recorded (${e.message}); it does not run`); }
    const childEnv = { ...clientEnv, ...made.env };
    const r = spawn(section.client, ["cargo", ...words], { cwd: checkoutCwd, env: childEnv, stdio: "inherit" });
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
    record({ ...base, ...outcome, nonce: made.carrier.nonce, checkoutCwd, clientExit: r.status ?? null });
    if (outcome.status !== "verified") {
      err(`rightkit-gate: RK_CARGO_BROKER_UNVERIFIED: \`cargo ${words.join(" ")}\` has no verified signed receipt (${outcome.code}: ${outcome.message}); the scan fails`);
      return r.status ? r.status : BROKER_REFUSAL_STATUS;
    }
    return r.status ?? (r.signal ? 128 : BROKER_REFUSAL_STATUS);
  } finally {
    if (made) fs.rmSync(made.file, { force: true });
    fs.rmSync(refDir, { recursive: true, force: true });
  }
}

/**
 * The gate's own check of every ledgered broker call of one scan. A call counts only when the broker's signed receipt, fetched
 * again now, verifies against THIS gate's commit, snapshot, ownership handle and pins, and its carrier was used once: a nonce or a
 * request id that appears twice in the run (`seen`) is a replay. Returns the problems; none means every call verified.
 */
export async function verifyBrokerLedger(entries, sb, { seen = { nonces: new Set(), ids: new Set() } } = {}) {
  const problems = [];
  for (const e of entries) {
    if (e.mode !== "broker" || e.status !== "verified") continue;
    const call = `cargo ${(e.argv ?? []).join(" ")}`.trim();
    try {
      if (typeof e.nonce !== "string" || typeof e.requestId !== "string" || !Array.isArray(e.argv) || !e.argv.every((a) => typeof a === "string") || typeof e.workspace !== "string" || typeof e.checkoutCwd !== "string") throw new SnapshotRequestError("ledger", "the record is incomplete");
      if (seen.nonces.has(e.nonce) || seen.ids.has(e.requestId)) throw new SnapshotRequestError("replay", "a carrier or request was used twice; each call carries its own fresh nonce and is used once");
      seen.nonces.add(e.nonce); seen.ids.add(e.requestId);
      const workspace = fs.realpathSync.native(e.workspace);
      if (!inside(workspace, sb.checkout)) throw new SnapshotRequestError("ledger", `the request ran in ${workspace}, outside the primary checkout`);
      if (fs.realpathSync.native(e.checkoutCwd) !== workspace) throw new SnapshotRequestError("ledger", "the request's working directory is not the one the call recorded");
      // The gate's own truth: its commit, snapshot, handle and declared inputs; only the call's own argv, nonce and cwd come from the ledger.
      const carrier = {
        schema: "rightkit.snapshot-request/1", commit: sb.commit, snapshot: sb.snapshot, declaredInputs: { inputs: readDeclaredInputs(sb.snapshot.path) },
        ownership: { handle: sb.handle }, primaryCwd: workspace, argvSha256: argvSha256(e.argv), nonce: e.nonce,
      };
      const receipt = await sb.receiptOf(e.requestId);
      verifySnapshotReceipt(receipt, { pins: sb.pins, jobId: e.requestId, carrier, argv: e.argv, workspace: e.workspace, observedEnv: e.observedEnv ?? {}, checkout: sb.checkout, outputRoots: sb.outputRoots, platform: sb.platform });
    } catch (error) { problems.push(`${call} (from ${e.cwd}): the gate's own verification of the broker's receipt failed (${error.code ?? "error"}: ${error.message})`); }
  }
  return problems;
}
