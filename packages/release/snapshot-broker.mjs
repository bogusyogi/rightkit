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
import { BROKER_CLIENT_QUERY_TIMEOUT_MS, BROKER_CLIENT_UNRESOLVED, CARRIED_CALL_TIMEOUT_MS, resolveBrokerClient, spawnBrokerClient, spawnBrokerClientAsync, withOutageReconnectBound } from "./broker-client.mjs";
import { loadBrokerPins } from "./broker-pins.mjs";
import { ATTEST_STAGE_NONCE_ENV, canonicalJson, carriedCallNonce } from "./provenance-verify.mjs";
import { resolveManagedRoots } from "./managed-roots.mjs";
import { forwardedAdapterEnv } from "./adapter-env.mjs";
import { OWNERSHIP_ENV, OWNERSHIP_FILE_ENV, markCarriedSend, unmarkCarriedSend } from "./ownership-mark.mjs";
import {
  MANAGED_RUST_ENVIRONMENT_KEYS, SNAPSHOT_REQUEST_SCHEMA, SnapshotRequestError, argvSha256, isCallHandleOf, lockedArgv, observedEnvOf, readDeclaredInputs, readRequestRef, verifySnapshotReceipt, writeSnapshotRequest,
} from "./snapshot-request.mjs";

/**
 * `env` without any managed Rust variable (MANAGED_RUST_ENVIRONMENT_KEYS, the list the client records as `observedEnv`; case-insensitive,
 * as Windows environments are). The broker refuses a carried request whose observed value of one of them differs from the one it manages
 * itself (service.rs validate_managed_rust_environment), and it supplies its own: so a carried call never forwards one, whatever the
 * caller's shell held (CARGO_HOME, RUSTFLAGS, RUSTC_WRAPPER, ...).
 */
export function withoutManagedRustEnv(env) {
  const out = { ...env };
  for (const key of Object.keys(out)) if (MANAGED_RUST_ENVIRONMENT_KEYS.includes(key.toUpperCase())) delete out[key];
  return out;
}

/** The status a refused or unverified call returns to its caller: cargo's own error status. */
export const BROKER_REFUSAL_STATUS = 101;
export const CALLING_GATE_ENV = "RUSTRIGHT_CALLING_GATE";

/** The managed `ws` roots a broker pins its generation targets under (BROKER-CONTRACT output pinning). */
export const brokerOutputRoots = ({ dataRoot }) => [path.join(dataRoot, "rightkit-build-control", "ws")];

/** The broker's receipt of one request, read through the RustRight client (`rustright status <id>`), synchronously. */
export function receiptSync({ client, requestId, env, spawn = spawnSync, platform = process.platform }) {
  const r = spawnBrokerClient(spawn, client, ["status", requestId], { encoding: "utf8", env, timeout: BROKER_CLIENT_QUERY_TIMEOUT_MS, maxBuffer: 64 * 1024 * 1024 }, { platform });
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
  need(typeof client === "string" && (platform === "win32" ? path.win32 : path).isAbsolute(client), "a broker snapshot compile needs the RustRight client (`rustright`) by absolute path");
  // The client the carried map hands every child is the executable itself: on win32 the `rustright.cmd` shim is resolved here, once,
  // so no child re-resolves (or spawns) a .cmd (broker-client.mjs). A shim of any other shape refuses with RK_BROKER_CLIENT_UNRESOLVED.
  try { client = resolveBrokerClient(client, { platform }).file; } catch (e) { throw new SnapshotRequestError(e.code ?? BROKER_CLIENT_UNRESOLVED, e.message); }
  const roots = resolveManagedRoots({ env, platform });
  const managedData = dataRoot ?? roots.dataRoot;
  const pins = loadPins({ dataRoot: managedData, platform, gateFile, checkout, spawn });
  return {
    controlRoot: roots.controlRoot, commit: { sha: commit.sha, tree: commit.tree }, snapshot: { path: snapshot.path, expectedTree: snapshot.expectedTree },
    declaredInputs: readDeclaredInputs(snapshot.path), handle: ownership.handle, markBrokerSubmit: (callHandle) => ownership.markBrokerSubmit(callHandle),
    unmarkBrokerSubmit: typeof ownership.unmarkBrokerSubmit === "function" ? (callHandle) => ownership.unmarkBrokerSubmit(callHandle) : undefined,
    pins: { host: pins.host, keyId: pins.keyId, publicKey: pins.publicKey, brokerDigest: pins.brokerDigest, ...(pins.brokerVersion !== undefined ? { brokerVersion: pins.brokerVersion } : {}), ...(pins.brokerSourceRevision !== undefined ? { brokerSourceRevision: pins.brokerSourceRevision } : {}) }, pinsSource: pins.source,
    checkout: fs.realpathSync.native(checkout), outputRoots: brokerOutputRoots({ dataRoot: managedData }), platform, client,
    receiptOf: async (requestId) => receiptSync({ client, requestId, env, spawn, platform }),
  };
}

/**
 * What the production gate hands the door as `o.snapshotBroker`: the statement that this gate carries the RustRight snapshot carrier.
 * The door only asks whether a broker host may take the snapshot route; the broker itself (pins, declared inputs, ownership, receipt
 * reader) is built by `createSnapshotBroker` where the snapshot compile runs.
 */
export const PRODUCTION_SNAPSHOT_CARRIER = Object.freeze({ kind: "production", create: createSnapshotBroker });

/** The part of a snapshot broker the wrapper process needs, as the map file carries it (public values only). */
export const brokerSection = (sb, { sentinel = null } = {}) => {
  const section = {
    client: sb.client, controlRoot: sb.controlRoot, commit: sb.commit, snapshot: sb.snapshot, handle: sb.handle, checkout: sb.checkout,
    pins: sb.pins, outputRoots: sb.outputRoots, platform: sb.platform, sentinel,
  };
  // The gate's own carried calls record their sub-handle through the ownership object it holds. Not enumerable: it never reaches the carrier
  // map a child reads (JSON), whose carried calls record through the inherited ownership file instead (markCarriedRequest).
  if (typeof sb.markBrokerSubmit === "function") Object.defineProperty(section, "markSend", { value: (callHandle) => sb.markBrokerSubmit(callHandle), enumerable: false });
  if (typeof sb.unmarkBrokerSubmit === "function") Object.defineProperty(section, "unmarkSend", { value: (callHandle) => sb.unmarkBrokerSubmit(callHandle), enumerable: false });
  return section;
};

/**
 * Records in the shared build-ownership record, immediately before the carried request `callHandle` is sent, that broker work may exist
 * (build-ownership.mjs markBrokerSubmit: `submitted` and the sub-handle). This is the only place a carried call marks, whichever process
 * it runs in: the gate's own, or a right-release child that inherited RIGHTKIT_BUILD_OWNERSHIP (+ _FILE) from it. `section.markSend` is a
 * seam for a caller that holds the ownership object (and for fixtures). Throws when the mark cannot land; the request is then not sent.
 */
function markCarriedRequest({ section, env, callHandle }) {
  // The gate's own hook is held to the same two assertions as the inherited-file path: a hook that returns without recording (a silent no-op) sends nothing.
  if (typeof section.markSend === "function") return assertMarked(section.markSend(callHandle), callHandle);
  const handle = env[OWNERSHIP_ENV] ?? process.env[OWNERSHIP_ENV];
  if (handle !== section.handle) throw new SnapshotRequestError("ownership", `the build ownership in the environment (${handle ?? "none"}) is not this call's (${section.handle}); the carried request is not sent`);
  const file = env[OWNERSHIP_FILE_ENV] ?? process.env[OWNERSHIP_FILE_ENV];
  if (!file) throw new SnapshotRequestError("ownership", `${OWNERSHIP_FILE_ENV} is not set: the carried request cannot be recorded in the build ownership, so it is not sent`);
  return assertMarked(markCarriedSend({ file, handle, callHandle }), callHandle);
}

/** The record a mark returned must say that broker work may exist and list this call's sub-handle; anything else is refused (N8). */
function assertMarked(marked, callHandle) {
  if (marked?.broker?.submitted !== true || !marked.broker.handles?.includes(callHandle)) throw new SnapshotRequestError("ownership", `the build ownership did not record the carried request ${callHandle}`);
  return marked;
}

/**
 * Takes back the mark of a request whose client provably never started (M5): the broker has no record of it and could never answer REAPED for
 * it, so leaving it marked would hold the host until a manual clear. Returns whether the mark was taken back; a failure leaves it marked (held,
 * never released early).
 */
function unmarkCarriedRequest({ section, env, callHandle }) {
  try {
    if (typeof section.unmarkSend === "function") { section.unmarkSend(callHandle); return true; }
    const handle = env[OWNERSHIP_ENV] ?? process.env[OWNERSHIP_ENV];
    const file = env[OWNERSHIP_FILE_ENV] ?? process.env[OWNERSHIP_FILE_ENV];
    if (handle !== section.handle || !file) return false;
    unmarkCarriedSend({ file, handle, callHandle });
    return true;
  } catch { return false; }
}

/** A client that failed to START (no pid): spawn error, a refused shim. A timeout or an output overflow means it ran, so it is never "unsent". */
export const clientNeverStarted = (r) => Boolean(r?.error) && !r.pid && r.error.code !== "ETIMEDOUT" && r.error.code !== "ENOBUFS" && !r.timedOut && !r.aborted;

/**
 * Cancels the broker request a killed client had submitted (`rustright cancel <id>`, the id read from the request-reference file the client
 * wrote when the broker accepted the job). On POSIX the client's own SIGTERM handler already sends one CANCEL; on Windows nothing reaches the
 * client, and a client that died first cannot send one, so the gate sends it itself (cancel is idempotent). Returns `{ cancelled, requestId?, why }`.
 */
export function cancelBrokerRequest({ client, requestIdFile, env, spawn = spawnSync, platform = process.platform }) {
  let ref;
  try { ref = readRequestRef(requestIdFile); } catch { return { cancelled: false, why: "the client had written no request id, so the broker has nothing of this call to cancel" }; }
  const r = spawnBrokerClient(spawn, client, ["cancel", ref.requestId], { encoding: "utf8", env, timeout: BROKER_CLIENT_QUERY_TIMEOUT_MS, maxBuffer: 1 << 24, windowsHide: true }, { platform });
  if (r.error || r.status !== 0) return { cancelled: false, requestId: ref.requestId, why: `rustright cancel ${ref.requestId} failed (${r.error?.message ?? `exit ${r.status}`}); the job may run until the broker's disconnect grace ends it` };
  return { cancelled: true, requestId: ref.requestId, why: `rustright cancel ${ref.requestId} was sent` };
}

const LEASE_STATUS_SCHEMA = "rustright.snapshot-lease-status.v1";
const defaultSleep = (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
/**
 * After a kill and a cancel, whether the broker job is PROVEN stopped: its status reports a terminal receipt AND the snapshot lease of this call's
 * sub-handle answers REAPED, both polled within `waitMs`. Anything less (the broker unreachable, still running, a lease UNKNOWN or ACTIVE) is not
 * proof, and the call's submit mark stays: the ownership is held (never released on a client kill alone) and the next gate reclaims it on REAPED.
 * Returns `{ terminal, reaped, why }`.
 */
export function awaitCancelledReaped({ client, snapshot, requestId, callHandle, env, spawn = spawnSync, platform = process.platform, waitMs = 60_000, sleep = defaultSleep }) {
  const until = Date.now() + waitMs;
  let terminal = false; let reaped = false; let why = "not asked";
  for (;;) {
    if (!terminal) {
      const r = spawnBrokerClient(spawn, client, ["status", requestId], { encoding: "utf8", env, timeout: BROKER_CLIENT_QUERY_TIMEOUT_MS, maxBuffer: 64 * 1024 * 1024 }, { platform });
      try { const doc = JSON.parse(String(r.stdout ?? "")); terminal = !r.error && r.status === 0 && doc?.receipt !== null && typeof doc?.receipt === "object"; why = terminal ? "the job is terminal" : `the job is ${doc?.state ?? "not terminal"}`; } catch { why = `rustright status ${requestId} gave no answer`; }
    }
    if (terminal && !reaped) {
      const r = spawnBrokerClient(spawn, client, ["snapshot-lease", "status", "--snapshot", snapshot, "--ownership", callHandle], { encoding: "utf8", env, timeout: BROKER_CLIENT_QUERY_TIMEOUT_MS, maxBuffer: 1 << 24 }, { platform });
      let a = null; try { a = JSON.parse(String(r.stdout ?? "").trim().split(/\r?\n/).pop()); } catch { a = null; }
      reaped = !r.error && r.status === 0 && a?.schema === LEASE_STATUS_SCHEMA && a.state === "REAPED" && a.snapshot === snapshot && a.ownership === callHandle;
      why = reaped ? "the lease is REAPED" : `the lease of ${callHandle} is ${a?.state ?? "unreadable"}`;
    }
    if (terminal && reaped) return { terminal, reaped, why: `${requestId}: terminal and ${why}` };
    if (Date.now() >= until) return { terminal, reaped, why: `${requestId} not proven stopped within ${waitMs} ms (${why})` };
    sleep(Math.min(1000, Math.max(0, until - Date.now())));
  }
}

/**
 * Brokers found unreachable by a carried call of this process, by ownership handle. A later call of the same ownership starts its client with a short
 * reconnect budget (withOutageReconnectBound), so N calls against a dead broker cost N short waits, not N x the client's 15 minutes (M47). A verified call clears it.
 */
const outages = new Set();
export const resetBrokerOutages = () => outages.clear();
export const brokerOutageObserved = (handle) => outages.has(handle);

const flavour = (platform) => (platform === "win32" ? path.win32 : path);
const escapes = (rel, platform = process.platform) => { const p = flavour(platform); return rel === ".." || rel.startsWith(`..${p.sep}`) || p.isAbsolute(rel); };
/**
 * Whether `child` is `parent` or below it, spelled by `platform`'s rules. No extra case fold: path.win32.relative already compares case-insensitively
 * (N36d is falsified: `D:\Claude\App` and `d:\claude\app` are one directory to it), so a fold here would only repeat what node does.
 */
export function inside(child, parent, platform = process.platform) {
  const rel = flavour(platform).relative(parent, child);
  return rel === "" || !escapes(rel, platform);
}

/**
 * The primary-checkout counterpart of a directory of the snapshot, computed LEXICALLY: the logical checkout path `base` plus the snapshot-relative
 * `rel`, checked to stay inside `checkout` by its spelling alone. Nothing about the checkout's present state decides the mapping. The directory is then
 * required to exist because RustRight requires it: the client canonicalizes the carrier's primaryCwd and compares it with its own working directory
 * (rustright-cli shims.rs:241, b84ae605), and the broker canonicalizes the request workspace for an adapter root (service.rs:4116-4119), so a counterpart
 * the newer checkout deleted or moved cannot be served today. That is reported as a typed BROKER-SIDE refusal naming the missing directory, never as a
 * vague cwd failure; the fix is RustRight's (map by the snapshot-relative cwd the carrier already binds). An existing counterpart is realpathed, so a link
 * that leaves the checkout is still refused. Returns `{ dir }` or `{ refusal: { code, message } }`.
 */
export function checkoutCounterpart({ base, rel, checkout, absentCode, platform = process.platform }) {
  // The logical base may be spelled through a link the checkout path is not (a map built from a realpath): resolve the base alone when it exists.
  let root = base; try { root = fs.realpathSync.native(base); } catch { /* an absent base is judged by its spelling */ }
  const lexical = path.join(root, rel);
  if (!inside(lexical, checkout, platform)) return { refusal: { code: absentCode.replace(/_ABSENT$/, "_ESCAPE"), message: `${lexical} is outside the primary checkout ${checkout}` } };
  let real;
  try { real = fs.realpathSync.native(lexical); }
  catch (e) {
    if (e.code === "ENOENT" || e.code === "ENOTDIR") return { refusal: { code: absentCode, message: `the selected commit's directory ${rel === "" ? "." : rel.split(path.sep).join("/")} has no counterpart ${lexical} in the primary checkout (deleted or moved since the commit); RustRight's client requires the carrier's primaryCwd to exist and be its working directory (shims.rs:241), so this is a broker-side limit: the snapshot-relative cwd is already bound by the carrier and RustRight must map by it. The gate does not recreate the directory` } };
    return { refusal: { code: absentCode, message: `${lexical} cannot be resolved (${e.code ?? e.message})` } };
  }
  try { if (!fs.statSync(real).isDirectory()) return { refusal: { code: absentCode, message: `${real} is not a directory` } }; } catch (e) { return { refusal: { code: absentCode, message: `${real} cannot be read (${e.code ?? e.message})` } }; }
  if (!inside(real, checkout, platform)) return { refusal: { code: absentCode.replace(/_ABSENT$/, "_ESCAPE"), message: `${real} is outside the primary checkout ${checkout}` } };
  return { dir: real };
}

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
 * What happened to the client of one carried call, after it returned: whether it was killed (`interrupted`: "timeout" after `timeoutMs`, "aborted" by the gate's
 * signal), the explicit cancel of its request (a killed client may not have sent one, and a Windows client never could), and, for a client that never started,
 * whether the call's submit mark was taken back (M5). A timed-out or aborted call is never left to run on at the broker.
 */
function afterClient({ r, section, env, clientEnv, requestIdFile, callHandle, spawn, reapWaitMs, sleep }) {
  const interrupted = r.aborted ? "aborted" : (r.timedOut || r.error?.code === "ETIMEDOUT") ? "timeout" : null;
  const settled = { interrupted, cancel: null, reap: null, unmarked: false, unsent: false };
  if (interrupted) {
    settled.cancel = cancelBrokerRequest({ client: section.client, requestIdFile, env: clientEnv, spawn, platform: section.platform });
    // Killing the client proves nothing about the broker job: wait, bounded, for its terminal state and the lease's REAPED. The mark stays either way.
    settled.reap = settled.cancel.requestId
      ? awaitCancelledReaped({ client: section.client, snapshot: section.snapshot.path, requestId: settled.cancel.requestId, callHandle, env: clientEnv, spawn, platform: section.platform, ...(reapWaitMs !== undefined ? { waitMs: reapWaitMs } : {}), ...(sleep ? { sleep } : {}) })
      : { terminal: false, reaped: false, why: "no request id, so no job to prove stopped" };
  }
  else if (clientNeverStarted(r)) { settled.unsent = true; settled.unmarked = unmarkCarriedRequest({ section, env, callHandle }); }
  return settled;
}
const interruption = (settled, timeoutMs) => `the RustRight client ${settled.interrupted === "timeout" ? `did not finish within ${timeoutMs} ms` : "was stopped with the gate"} and was killed; ${settled.cancel.why}; ${settled.reap.reaped ? settled.reap.why : `${settled.reap.why}: the build ownership stays held until the broker proves the job reaped`}`;
/** Records, from a finished call, whether the broker looked unreachable (no request id ever came back, or the status could not be read): the next call of this ownership gets a short reconnect budget. */
function noteOutage(handle, outcome, ref, settled) {
  if (outcome.status === "verified") { outages.delete(handle); return; }
  if (settled.interrupted || settled.unsent) return;
  if (!ref || outcome.code === "receipt" || outcome.code === "receipt-not-ready") outages.add(handle);
}

/** Runs a carried call whose single `yield` is the client spawn, with spawnSync, bounded by `timeoutMs` (killed with SIGTERM, which the client turns into one CANCEL). */
function driveSync(gen, { spawn = spawnSync, section, timeoutMs = CARRIED_CALL_TIMEOUT_MS }) {
  let step = gen.next();
  try {
    while (!step.done) {
      const q = step.value;
      step = gen.next(spawnBrokerClient(spawn, section.client, q.args, { ...q.options, timeout: timeoutMs, killSignal: "SIGTERM" }, { platform: section.platform }));
    }
  } catch (e) { gen.return(); throw e; }
  return step.value;
}
/** `driveSync` with the client as an async child (`spawnAsync`, default spawnClientProcess), killed on `timeoutMs` or when `signal` aborts. */
async function driveAsync(gen, { spawnAsync, section, timeoutMs = CARRIED_CALL_TIMEOUT_MS, signal }) {
  let step = gen.next();
  try {
    while (!step.done) {
      const q = step.value;
      step = gen.next(await spawnBrokerClientAsync(spawnAsync, section.client, q.args, { ...q.options, timeout: timeoutMs, killSignal: "SIGTERM", signal }, { platform: section.platform }));
    }
  } catch (e) { gen.return(); throw e; }
  return step.value;
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
export function carryBrokerCargo(call) { return driveSync(cargoCore(call), call); }
/** `carryBrokerCargo` with the client as an async child: `signal` (an AbortSignal) kills it and cancels its request; the gate's own signal handlers keep running meanwhile (M45). */
export function carryBrokerCargoAsync(call) { return driveAsync(cargoCore(call), call); }
function* cargoCore({ words, cwd, env, section, workspace, checkoutWorkspace, record, spawn = spawnSync, err, tmp = os.tmpdir(), stdio = "inherit", timeoutMs = CARRIED_CALL_TIMEOUT_MS, signal, reapWaitMs, sleep }) {
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
  const clientEnv = withoutManagedRustEnv(env);
  // There is no stage grant any more (one model: a snapshot stage's compiles run nested under an adapter job); an inherited one is dropped, never forwarded.
  for (const key of Object.keys(clientEnv)) if (/^(RUSTRIGHT|RIGHTKIT)_SNAPSHOT_GRANT$/i.test(key)) delete clientEnv[key];
  // A release-bound stage's nonce (RIGHTKIT_ATTEST_STAGE_NONCE) is read here and never reaches the client: this call's nonce is derived from it.
  const stageNonce = env[ATTEST_STAGE_NONCE_ENV];
  for (const key of Object.keys(clientEnv)) if (key.toUpperCase() === ATTEST_STAGE_NONCE_ENV) delete clientEnv[key];
  let realCwd; let checkoutCwd; let rel;
  try {
    realCwd = fs.realpathSync.native(cwd);
    rel = path.relative(workspace, realCwd);
    if (escapes(rel, section.platform)) throw new Error(`${realCwd} is not inside the workspace ${workspace}`);
  } catch (e) { return refuse("RK_CARGO_BROKER_CWD", `the call's working directory is not inside its workspace (${e.message})`); }
  const counterpart = checkoutCounterpart({ base: checkoutWorkspace, rel, checkout: section.checkout, absentCode: "RK_CARGO_BROKER_COUNTERPART_ABSENT", platform: section.platform });
  if (counterpart.refusal) return refuse(counterpart.refusal.code, counterpart.refusal.message);
  checkoutCwd = counterpart.dir;
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
    // Broker work may exist from the next statement on: the ownership records this request's sub-handle first (a mark that does not land sends nothing).
    if (signal?.aborted) { fs.rmSync(made.file, { force: true }); return refuse("RK_CARGO_BROKER_ABORTED", "the gate was stopped before this request was sent"); }
    try { markCarriedRequest({ section, env, callHandle: made.carrier.ownership.handle }); } catch (e) { fs.rmSync(made.file, { force: true }); return refuse("RK_CARGO_BROKER_OWNERSHIP", e.message); }
    // After a broker outage this ownership has already seen, the client's reconnect budget is short (M47); the first call of an outage keeps the client's own.
    const r = yield { args: ["cargo", ...words], options: capture ? { cwd: checkoutCwd, env: outages.has(section.handle) ? withOutageReconnectBound(childEnv) : childEnv, encoding: "utf8", maxBuffer: 1 << 28, windowsHide: true } : { cwd: checkoutCwd, env: outages.has(section.handle) ? withOutageReconnectBound(childEnv) : childEnv, stdio: "inherit" } };
    fs.rmSync(made.file, { force: true });
    const settled = afterClient({ r, section, env, clientEnv, requestIdFile: path.join(refDir, "request.json"), callHandle: made.carrier.ownership.handle, spawn, reapWaitMs, sleep });
    let outcome;
    let ref = null;
    try {
      if (settled.interrupted) throw new SnapshotRequestError(settled.interrupted, interruption(settled, timeoutMs));
      if (r.error) throw new SnapshotRequestError(r.error.code === BROKER_CLIENT_UNRESOLVED ? BROKER_CLIENT_UNRESOLVED : "spawn", `the RustRight client did not start (${r.error.message})${settled.unmarked ? "; its submit mark was taken back" : ""}`);
      ref = readRequestRef(path.join(refDir, "request.json"));
      const receipt = receiptSync({ client: section.client, requestId: ref.requestId, env: clientEnv, spawn, platform: section.platform });
      const observedEnv = observedEnvOf(clientEnv, section.platform);
      const { evidence, attribution, toolchain } = verifySnapshotReceipt(receipt, { pins: section.pins, jobId: ref.requestId, carrier: made.carrier, argv: words, workspace: ref.workspace, observedEnv, checkout: section.checkout, outputRoots: section.outputRoots, platform: section.platform });
      // attribution and toolchain are recorded as the broker signed them (a consumer that needs per-package attribution treats available:false as FAILED).
      outcome = { status: "verified", requestId: ref.requestId, workspace: ref.workspace, observedEnv, outputDir: evidence.outputDir, treeAtStart: evidence.snapshot.treeAtStart, treeAtExit: evidence.snapshot.treeAtExit, attribution, toolchain };
    } catch (e) {
      outcome = { status: "unverified", code: e.code ?? "error", message: e.message, ...(ref ? { requestId: ref.requestId, workspace: ref.workspace } : {}) };
    }
    noteOutage(section.handle, outcome, ref, settled);
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
export function carryBrokerAdapter(call) { return driveSync(adapterCore(call), call); }
/** `carryBrokerAdapter` with the client as an async child: `signal` (an AbortSignal) kills it and cancels its request (M45). */
export function carryBrokerAdapterAsync(call) { return driveAsync(adapterCore(call), call); }
function* adapterCore({ adapter, words, cwd, env, section, record, spawn = spawnSync, err, tmp = os.tmpdir(), stdio = "inherit", timeoutMs = CARRIED_CALL_TIMEOUT_MS, signal, reapWaitMs, sleep }) {
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
  const clientEnv = withoutManagedRustEnv(env);
  for (const key of Object.keys(clientEnv)) if (/^(RUSTRIGHT|RIGHTKIT)_SNAPSHOT_GRANT$/i.test(key)) delete clientEnv[key];
  const stageNonce = env[ATTEST_STAGE_NONCE_ENV];
  for (const key of Object.keys(clientEnv)) if (key.toUpperCase() === ATTEST_STAGE_NONCE_ENV) delete clientEnv[key];
  // The request environment the client will forward (RUSTRIGHT_ADAPTER_ENV and the variables it names): bound into the request core below and
  // recorded in the ledger so the gate's own re-verification recomputes the same digest. A reserved or unset name is refused here, as the client would.
  let adapterEnv;
  try { adapterEnv = forwardedAdapterEnv(env); } catch (e) { return refuse(e.code ?? "RK_ADAPTER_ENV_INVALID", e.message); }
  base.adapterEnv = adapterEnv;
  let realCwd; let checkoutCwd; let rel;
  try {
    const snapshotRoot = fs.realpathSync.native(section.snapshot.path);
    realCwd = fs.realpathSync.native(cwd);
    rel = path.relative(snapshotRoot, realCwd);
    if (escapes(rel, section.platform)) throw new Error(`${realCwd} is not inside the snapshot ${snapshotRoot}`);
  } catch (e) { return refuse("RK_ADAPTER_BROKER_CWD", `the step's working directory is not inside the snapshot (${e.message})`); }
  const counterpart = checkoutCounterpart({ base: section.checkout, rel, checkout: section.checkout, absentCode: "RK_ADAPTER_BROKER_COUNTERPART_ABSENT", platform: section.platform });
  if (counterpart.refusal) return refuse(counterpart.refusal.code, counterpart.refusal.message);
  checkoutCwd = counterpart.dir;
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
    // Broker work may exist from the next statement on: the ownership records this request's sub-handle first (a mark that does not land sends nothing).
    if (signal?.aborted) { fs.rmSync(made.file, { force: true }); return refuse("RK_ADAPTER_BROKER_ABORTED", "the gate was stopped before this request was sent"); }
    try { markCarriedRequest({ section, env, callHandle: made.carrier.ownership.handle }); } catch (e) { fs.rmSync(made.file, { force: true }); return refuse("RK_ADAPTER_BROKER_OWNERSHIP", e.message); }
    const r = yield { args: ["run", adapter, "--", ...words], options: capture ? { cwd: checkoutCwd, env: outages.has(section.handle) ? withOutageReconnectBound(childEnv) : childEnv, encoding: "utf8", maxBuffer: 1 << 28, windowsHide: true } : { cwd: checkoutCwd, env: outages.has(section.handle) ? withOutageReconnectBound(childEnv) : childEnv, stdio: "inherit" } };
    fs.rmSync(made.file, { force: true });
    const settled = afterClient({ r, section, env, clientEnv, requestIdFile: path.join(refDir, "request.json"), callHandle: made.carrier.ownership.handle, spawn, reapWaitMs, sleep });
    let outcome;
    let ref = null;
    try {
      if (settled.interrupted) throw new SnapshotRequestError(settled.interrupted, interruption(settled, timeoutMs));
      if (r.error) throw new SnapshotRequestError(r.error.code === BROKER_CLIENT_UNRESOLVED ? BROKER_CLIENT_UNRESOLVED : "spawn", `the RustRight client did not start (${r.error.message})${settled.unmarked ? "; its submit mark was taken back" : ""}`);
      ref = readRequestRef(path.join(refDir, "request.json"));
      // The request's workspace of a snapshot-bound adapter job is the directory the client ran in: the primary checkout's counterpart of the step's
      // snapshot directory (the broker, START: "the job runs where it was asked to (request.workspace, already the primary's counterpart of a
      // directory in the snapshot), which must be inside that root"). It is the carrier's primaryCwd, never the toplevel for a step run from a subfolder
      // (an app whose root is `app/`), and never a directory outside the primary checkout or one that is not this call's counterpart.
      if (fs.realpathSync.native(ref.workspace) !== checkoutCwd || !inside(fs.realpathSync.native(ref.workspace), section.checkout, section.platform)) throw new SnapshotRequestError("workspace", `the request's workspace ${ref.workspace} is not the primary checkout counterpart ${checkoutCwd} of the step's directory ${realCwd} (primary toplevel ${section.checkout})`);
      const receipt = receiptSync({ client: section.client, requestId: ref.requestId, env: clientEnv, spawn, platform: section.platform });
      const { evidence, attribution, toolchain, nestedChildren } = verifySnapshotReceipt(receipt, { pins: section.pins, jobId: ref.requestId, carrier: made.carrier, argv: words, workspace: ref.workspace, adapter, adapterEnv, checkout: section.checkout, outputRoots: section.outputRoots, platform: section.platform });
      outcome = { status: "verified", requestId: ref.requestId, workspace: ref.workspace, outputDir: evidence.outputDir, nestedChildren, treeAtStart: evidence.snapshot.treeAtStart, treeAtExit: evidence.snapshot.treeAtExit, attribution, toolchain };
    } catch (e) {
      outcome = { status: "unverified", code: e.code ?? "error", message: e.message, ...(ref ? { requestId: ref.requestId, workspace: ref.workspace } : {}) };
    }
    noteOutage(section.handle, outcome, ref, settled);
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
      if (!inside(workspace, sb.checkout, sb.platform)) throw new SnapshotRequestError("ledger", `the request ran in ${workspace}, outside the primary checkout`);
      if (adapter === null && fs.realpathSync.native(e.checkoutCwd) !== workspace) throw new SnapshotRequestError("ledger", "the request's working directory is not the one the call recorded");
      if (adapter !== null) {
        // The workspace is the directory the client ran in, the carrier's primaryCwd; the step's snapshot directory and that checkout directory are the
        // same path below their two roots.
        if (workspace !== fs.realpathSync.native(e.checkoutCwd)) throw new SnapshotRequestError("ledger", `the adapter request's workspace ${workspace} is not the primary checkout counterpart ${e.checkoutCwd} of the step's directory`);
        const below = path.relative(fs.realpathSync.native(sb.snapshot.path), fs.realpathSync.native(e.cwd));
        if (escapes(below, sb.platform)) throw new SnapshotRequestError("ledger", `the step ran from ${e.cwd}, outside the snapshot`);
        if (path.relative(sb.checkout, fs.realpathSync.native(e.checkoutCwd)) !== below) throw new SnapshotRequestError("ledger", `the carrier named ${e.checkoutCwd}, not the checkout counterpart of ${e.cwd}`);
      }
      // The gate's own truth: its commit, snapshot, handle and declared inputs; only the call's own argv, nonce and cwd come from the ledger.
      const carrier = {
        schema: SNAPSHOT_REQUEST_SCHEMA, commit: sb.commit, snapshot: sb.snapshot, declaredInputs: { inputs: readDeclaredInputs(sb.snapshot.path) },
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
