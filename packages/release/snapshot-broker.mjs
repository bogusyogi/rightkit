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
import { BROKER_CLIENT_UNRESOLVED, resolveBrokerClient, spawnBrokerClient } from "./broker-client.mjs";
import { loadBrokerPins } from "./broker-pins.mjs";
import { ATTEST_STAGE_NONCE_ENV, canonicalJson, carriedCallNonce } from "./provenance-verify.mjs";
import { resolveManagedRoots } from "./managed-roots.mjs";
import { forwardedAdapterEnv, withoutSigningEnv } from "./adapter-env.mjs";
import { carriedShapeDecision } from "./cargo-shapes.mjs";
import { swiftScratchRefusal } from "./swift-scratch.mjs";
import { removeCarrier, sweepSnapshotRequests } from "./carrier-census.mjs";
import { DEFAULT_RETRY_WINDOW_MS, RETRYABLE_OUTCOME_CODES, clientGuardMs, diagnoseControlReply, drainFrames, operationDeadlineEnv, operationOutcomeInOutput, overCapProblem, refusalInCapturedOutput, retryPauseMs, sleepSync } from "./broker-wire.mjs";
import { OWNERSHIP_ENV, OWNERSHIP_FILE_ENV, markCarriedSend, markCarriedUnsent } from "./ownership-mark.mjs";
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
  const out = withoutSigningEnv(env); // GB-06: signing material (TAURI_SIGNING_*, TAURI_PRIVATE_KEY, TAURI_KEY_PASSWORD) never reaches the client of a carried call
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
  const r = spawnBrokerClient(spawn, client, ["status", requestId], { encoding: "utf8", env, timeout: 60_000, maxBuffer: 64 * 1024 * 1024 }, { platform });
  const failedRun = () => new SnapshotRequestError("receipt", `rustright status ${requestId} failed (${r.error?.message ?? `exit ${r.status}`})`);
  if (r.error) throw failedRun();
  let doc;
  // A refused or unavailable reply may arrive with a nonzero exit (the client exits 125 on a REJECTED result): its typed body is still diagnosed (GB-84).
  try { doc = JSON.parse(String(r.stdout)); } catch { throw r.status !== 0 ? failedRun() : new SnapshotRequestError("receipt", `rustright status ${requestId} printed no JSON`); }
  // The status envelope is `{type:"STATUS", id, state, ..., receipt}`: `receipt` is null while the job is live and full once it is
  // terminal. Polling is not allowed, so a null receipt after the cargo process exited is a clear error, never a retry.
  // GB-53: an over-cap outcome is a typed truncation, never a missing-field mystery.
  const over = overCapProblem(doc);
  if (over) throw new SnapshotRequestError(over.code, `rustright status ${requestId}: ${over.message}`);
  // GB-84: the typed envelope is judged BEFORE any receipt validation (diagnoseControlReply, RustRight's own rule, vector control-status-vectors.v1.json). Only a terminal
  // reply with a receipt of this request is a build outcome; a live, unavailable, refused or malformed reply keeps the broker's state, code and identity, is never green, and
  // is reconciled by the durable request id (the gate never resubmits because a diagnosis improved).
  const diagnosis = diagnoseControlReply(doc, requestId);
  const typed = (code, message) => Object.assign(new SnapshotRequestError(code, message), { diagnosis });
  switch (diagnosis.kind) {
    case "terminal": if (r.status !== 0) throw failedRun(); return diagnosis.receipt;
    case "live": throw typed("receipt-not-ready", `rustright status ${requestId} reports the job ${diagnosis.state} with no receipt yet, although its cargo process exited; the gate does not poll or resubmit, so this call is unverified and the request id ${requestId} is the durable handle to reconcile it`);
    case "unavailable": throw typed("broker-unavailable", `rustright status ${requestId}: the broker's front gate cannot serve now (${diagnosis.reason}); the request id ${requestId} is kept for reconciliation, nothing is resubmitted`);
    case "refused": throw typed(`broker-refused-${diagnosis.refusal}`, `rustright status ${requestId}: the broker refused the reply (${diagnosis.code ?? "no code"}: ${diagnosis.message || diagnosis.refusal})${diagnosis.refusal === "suspended" ? "; the job runs again after the broker restarts: ask again then, by this id" : diagnosis.refusal === "paused" ? "; admission is paused, nothing was spawned" : ""}`);
    default: throw typed("receipt", `rustright status ${requestId}: the reply is not a valid status for this request (${diagnosis.message})`);
  }
}

/**
 * `deps.snapshotBroker` for a production run on a broker host. Throws when anything the compile owes cannot be established:
 * no build ownership, no RustRight client, no pins (an app repository without an enrollment), a malformed declared-inputs manifest.
 * There is no way to hand it pins: tests build their own object and inject it as `deps.snapshotBroker`.
 */
export function createSnapshotBroker({ env = process.env, platform = process.platform, checkout, commit, snapshot, ownership, client, gateFile, dataRoot, spawn = spawnSync, loadPins = loadBrokerPins, lease = null }) {
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
  // GB-76: carriers a crashed gate left behind are retired on proof (creator dead, nothing live or unknown at the broker under their sub-handle), counted otherwise. Best-effort: a
  // sweep that cannot run never stops a build, and nothing is deleted without proof.
  let carrierSweep = null;
  if (typeof lease === "function") { try { carrierSweep = sweepSnapshotRequests({ controlRoot: roots.controlRoot, snapshot: snapshot.path, lease: (q) => lease({ ...q, env, platform, spawn, client }) }); } catch { carrierSweep = null; } }
  return {
    carrierSweep,
    controlRoot: roots.controlRoot, commit: { sha: commit.sha, tree: commit.tree }, snapshot: { path: snapshot.path, expectedTree: snapshot.expectedTree },
    declaredInputs: readDeclaredInputs(snapshot.path), handle: ownership.handle, markBrokerSubmit: (callHandle) => ownership.markBrokerSubmit(callHandle), markBrokerUnsent: typeof ownership.markBrokerUnsent === "function" ? (callHandle, reason) => ownership.markBrokerUnsent(callHandle, reason) : undefined,
    pins: { host: pins.host, keyId: pins.keyId, publicKey: pins.publicKey, brokerDigest: pins.brokerDigest }, pinsSource: pins.source,
    checkout: fs.realpathSync.native(checkout), outputRoots: brokerOutputRoots({ dataRoot: managedData }), platform, client,
    receiptOf: async (requestId) => receiptSync({ client, requestId, env, spawn, platform }),
    // The broker's lease status of one sub-handle (SNAPSHOT_LEASE_STATUS, with the bounded wait): what a cancelled call is reconciled against.
    ...(typeof lease === "function" ? { leaseStatus: ({ handle, waitMs = 0 }) => lease({ snapshot: snapshot.path, handle, env, platform, spawn, client, waitMs }) } : {}),
  };
}

/**
 * What the production gate hands the door as `o.snapshotBroker`: the statement that this gate carries the RustRight snapshot carrier.
 * The door only asks whether a broker host may take the snapshot route; the broker itself (pins, declared inputs, ownership, receipt
 * reader) is built by `createSnapshotBroker` where the snapshot compile runs.
 */
export const PRODUCTION_SNAPSHOT_CARRIER = Object.freeze({ kind: "production", create: createSnapshotBroker });

/** What a typed refusal means for the caller, appended to the outcome's message: nothing was spawned, and whether waiting helps (BROKER-CONTRACT section 16). */
function refusalAdvice(refused) {
  if (refused.refusal === "paused") return ": admission is paused and nothing was spawned";
  if (refused.refusal === "recovery-pending") return ": restart recovery of the workspace is still running and nothing was admitted; the call is retried within its deadline";
  if (refused.refusal === "storage-budget-unavailable") return ": the storage budget could not be settled (a maintenance pass is running) and nothing was admitted; the call is retried within its deadline";
  if (refused.refusal === "adapter-pin-required") return ": nothing was spawned; the commit's gradle/wrapper/gradle-wrapper.properties must carry distributionSha256Sum=<64 lowercase hex> for its distribution (the pin is the commit's, so this is not retried)";
  return "";
}

/** The part of a snapshot broker the wrapper process needs, as the map file carries it (public values only). */
export const brokerSection = (sb, { sentinel = null } = {}) => {
  const section = {
    client: sb.client, controlRoot: sb.controlRoot, commit: sb.commit, snapshot: sb.snapshot, handle: sb.handle, checkout: sb.checkout,
    pins: sb.pins, outputRoots: sb.outputRoots, platform: sb.platform, sentinel,
  };
  // The gate's own carried calls record their sub-handle through the ownership object it holds. Not enumerable: it never reaches the carrier
  // map a child reads (JSON), whose carried calls record through the inherited ownership file instead (markCarriedRequest).
  if (typeof sb.markBrokerSubmit === "function") Object.defineProperty(section, "markSend", { value: (callHandle) => sb.markBrokerSubmit(callHandle), enumerable: false });
  if (typeof sb.markBrokerUnsent === "function") Object.defineProperty(section, "markUnsent", { value: (callHandle, reason) => sb.markBrokerUnsent(callHandle, reason), enumerable: false });
  // GB-52: the broker's lease status of one sub-handle, asked after a cancelled call (not part of the map a child reads).
  if (typeof sb.leaseStatus === "function") Object.defineProperty(section, "leaseStatus", { value: (query) => sb.leaseStatus(query), enumerable: false });
  return section;
};

/**
 * Records in the shared build-ownership record, immediately before the carried request `callHandle` is sent, that broker work may exist
 * (build-ownership.mjs markBrokerSubmit: `submitted` and the sub-handle). This is the only place a carried call marks, whichever process
 * it runs in: the gate's own, or a right-release child that inherited RIGHTKIT_BUILD_OWNERSHIP (+ _FILE) from it. `section.markSend` is a
 * seam for a caller that holds the ownership object (and for fixtures). Throws when the mark cannot land; the request is then not sent.
 */
function markCarriedRequest({ section, env, callHandle }) {
  if (typeof section.markSend === "function") {
    // GB-69: a direct hook's return value is never trusted unchecked: the same assertion the env-file path makes. A hook that did nothing, returned another handle's record, or
    // returned no record of this call's sub-handle refuses the send (nothing is sent), whatever the hook is.
    const marked = section.markSend(callHandle);
    if (marked?.broker?.submitted !== true || !Array.isArray(marked.broker.handles) || !marked.broker.handles.includes(callHandle) || (marked.handle !== undefined && marked.handle !== section.handle)) throw new SnapshotRequestError("ownership", `the build ownership did not record the carried request ${callHandle} under ${section.handle} (the submit hook returned ${marked?.broker ? "another record" : "no record"}); the request is not sent`);
    return marked;
  }
  const handle = env[OWNERSHIP_ENV] ?? process.env[OWNERSHIP_ENV];
  if (handle !== section.handle) throw new SnapshotRequestError("ownership", `the build ownership in the environment (${handle ?? "none"}) is not this call's (${section.handle}); the carried request is not sent`);
  const file = env[OWNERSHIP_FILE_ENV] ?? process.env[OWNERSHIP_FILE_ENV];
  if (!file) throw new SnapshotRequestError("ownership", `${OWNERSHIP_FILE_ENV} is not set: the carried request cannot be recorded in the build ownership, so it is not sent`);
  const marked = markCarriedSend({ file, handle, callHandle });
  if (marked?.broker?.submitted !== true || !marked.broker.handles?.includes(callHandle)) throw new SnapshotRequestError("ownership", `the build ownership did not record the carried request ${callHandle}`);
  return marked;
}

/** The spawn errors that prove the client process never started, so no byte reached the broker (GB-21). Anything else after a start is ambiguous and is never marked unsent. */
const NO_SPAWN_CODES = new Set(["ENOENT", "EACCES", "EPERM", "EINVAL", "ENOEXEC", "ENOTDIR", BROKER_CLIENT_UNRESOLVED]);
export const provenNoSpawn = (error) => Boolean(error) && NO_SPAWN_CODES.has(error.code);

/**
 * GB-21: whether the RustRight client can be started at all, checked BEFORE the ownership is marked (a mark records that broker work may exist, so a call that cannot start
 * must not leave one). An absolute client path must exist and (on POSIX) be executable; a bare name is resolved by the spawn itself and is left to it.
 */
export function clientStartable(client, platform = process.platform) {
  if (typeof client !== "string" || client === "") return "no RustRight client is named";
  if (!(platform === "win32" ? path.win32 : path).isAbsolute(client)) return null;
  if (platform !== process.platform) return null; // a simulated platform (a test) has no file system of its own to probe
  try { const meta = fs.statSync(client); if (!meta.isFile()) return `${client} is not a file`; if (platform !== "win32") fs.accessSync(client, fs.constants.X_OK); return null; } catch (e) { return `${client} cannot be started (${e.code ?? e.message})`; }
}

/** Records, after a client that provably never started, that this sub-handle was never sent (ownership: broker.unsent). Best-effort: a mark that cannot land leaves the record held, never released. */
function markCarriedUnsentRequest({ section, env, callHandle, reason }) {
  try {
    if (typeof section.markUnsent === "function") { section.markUnsent(callHandle, reason); return true; }
    const handle = env[OWNERSHIP_ENV] ?? process.env[OWNERSHIP_ENV]; const file = env[OWNERSHIP_FILE_ENV] ?? process.env[OWNERSHIP_FILE_ENV];
    if (handle !== section.handle || !file) return false;
    markCarriedUnsent({ file, handle, callHandle, reason });
    return true;
  } catch { return false; }
}

const escapes = (rel) => rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel);
const inside = (child, parent) => { const rel = path.relative(parent, child); return rel === "" || !escapes(rel); };

/**
 * The primary-checkout counterpart of a snapshot directory, spelled lexically (BROKER-CONTRACT section 16, GB-16): `base` (the logical checkout path) joined with the snapshot-relative `rel`,
 * which `join` normalises, so the result is absolute and holds no `.` or `..`. Nothing about the checkout's present state decides it. `pathApi` is the platform's path module (a Windows
 * spelling is judged with `path.win32`).
 */
export const lexicalCounterpart = ({ base, rel, pathApi = path }) => pathApi.join(base, rel);

/**
 * The nearest directory at or above `lexical` that exists (`isDirectory(p)`), or null when none does. A path that became a plain file is passed over: the nearest DIRECTORY is what a client
 * can start in.
 */
export function nearestExistingDirectory(lexical, { isDirectory = (p) => { try { return fs.statSync(p).isDirectory(); } catch { return false; } }, pathApi = path } = {}) {
  for (let p = lexical; ; p = pathApi.dirname(p)) {
    if (isDirectory(p)) return p;
    if (pathApi.dirname(p) === p) return null;
  }
}

/** `p` as the file system names it when it exists; when it is gone, the real path of its nearest existing directory plus what is left below it (a lexical path stays one). */
export function canonicalCounterpart(p) {
  try { return fs.realpathSync.native(p); } catch (e) { if (e.code !== "ENOENT" && e.code !== "ENOTDIR") throw e; }
  const ancestor = nearestExistingDirectory(p);
  if (ancestor === null) throw Object.assign(new Error(`${p} has no existing ancestor`), { code: "ENOENT" });
  return path.join(fs.realpathSync.native(ancestor), path.relative(ancestor, p));
}

/**
 * The primary-checkout counterpart of a directory of the snapshot (GB-16). The carrier's `primaryCwd` is that counterpart spelled lexically; the RustRight client accepts it when its own working
 * directory is it, or, when it is gone (an older commit built after the primary checkout removed the directory), the nearest directory of it that still exists, and then names the
 * counterpart as the request's workspace (the attestation hashes that workspace). The broker maps by the snapshot-relative path. So a removed counterpart is no refusal: this returns
 * `{ dir, spawnCwd, removed }`: `dir` the lexical counterpart (what the carrier names), `spawnCwd` the real directory the client is started in (the counterpart itself when it exists, else the
 * real path of its nearest existing ancestor inside the checkout). An existing counterpart, and the ancestor of a removed one, are realpathed, so a link that leaves the checkout is still
 * refused (`..._ESCAPE`); anything else the file system will not say (permission) is `..._UNREADABLE`. Returns `{ dir, spawnCwd, removed }` or `{ refusal: { code, message } }`.
 */
export function checkoutCounterpart({ base, rel, checkout, codePrefix }) {
  const escapeCode = `${codePrefix}_ESCAPE`; const unreadableCode = `${codePrefix}_UNREADABLE`;
  // The logical base may be spelled through a link the checkout path is not (a map built from a realpath): resolve the base alone when it exists.
  let root = base; try { root = fs.realpathSync.native(base); } catch { /* an absent base is judged by its spelling */ }
  const lexical = lexicalCounterpart({ base: root, rel });
  if (!inside(lexical, checkout)) return { refusal: { code: escapeCode, message: `${lexical} is outside the primary checkout ${checkout}` } };
  let real;
  try { real = fs.realpathSync.native(lexical); }
  catch (e) {
    if (e.code !== "ENOENT" && e.code !== "ENOTDIR") return { refusal: { code: unreadableCode, message: `${lexical} cannot be resolved (${e.code ?? e.message})` } };
    const ancestor = nearestExistingDirectory(lexical);
    let spawnCwd = null;
    try { spawnCwd = ancestor === null ? null : fs.realpathSync.native(ancestor); } catch (error) { return { refusal: { code: unreadableCode, message: `${ancestor} cannot be resolved (${error.code ?? error.message})` } }; }
    if (spawnCwd === null || !inside(spawnCwd, checkout)) return { refusal: { code: escapeCode, message: `${spawnCwd ?? ancestor ?? lexical} is outside the primary checkout ${checkout}` } };
    return { dir: lexical, spawnCwd, removed: true };
  }
  try { if (!fs.statSync(real).isDirectory()) return { refusal: { code: unreadableCode, message: `${real} is not a directory` } }; } catch (e) { return { refusal: { code: unreadableCode, message: `${real} cannot be read (${e.code ?? e.message})` } }; }
  if (!inside(real, checkout)) return { refusal: { code: escapeCode, message: `${real} is outside the primary checkout ${checkout}` } };
  return { dir: real, spawnCwd: real, removed: false };
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
 * GB-52: a call that ran out its operation deadline is cancelled BY ID by the client, which then polls the terminal state. The gate does not take that for a release: it asks the broker once, with
 * a bounded wait, what it holds under the call's sub-handle (SNAPSHOT_LEASE_STATUS members, GB-23/GB-85). `reaped` is true only for a well-formed REAPED answer (every submission under the
 * handle durably complete); ACTIVE, UNKNOWN, a wait that timed out, a reader that is absent or throws all leave ownership held, and the outcome says which. Never resubmits.
 */
function reconcileAfterCancel({ section, callHandle, childEnv }) {
  const cancelWait = Number(childEnv?.RUSTRIGHT_CANCEL_WAIT_MS);
  const waitMs = Math.min(30_000, Number.isFinite(cancelWait) && cancelWait >= 0 ? Math.trunc(cancelWait) : 30_000);
  if (typeof section.leaseStatus !== "function") return { reaped: false, subHandle: { handle: callHandle, state: "UNKNOWN", why: "this gate has no lease reader" }, note: `the gate cannot ask the broker about sub-handle ${callHandle}, so ownership stays UNKNOWN until every sub-handle is REAPED` };
  let status;
  try { status = section.leaseStatus({ handle: callHandle, waitMs }); } catch (e) { status = { state: "UNKNOWN", why: e.message }; }
  const state = status?.state ?? "UNKNOWN";
  const subHandle = { handle: callHandle, state, why: status?.why ?? null, ...(status?.waitedMs !== undefined ? { waitedMs: status.waitedMs } : {}), ...(status?.waitTimedOut !== undefined ? { waitTimedOut: status.waitTimedOut } : {}), ...(Array.isArray(status?.members) ? { members: status.members } : {}) };
  if (state === "REAPED") return { reaped: true, subHandle, note: `after the cancel the broker reports sub-handle ${callHandle} REAPED (every submission under it is settled)` };
  return { reaped: false, subHandle, note: `after the cancel the broker reports sub-handle ${callHandle} still ${state}${status?.why ? ` (${status.why})` : ""}: ownership stays held until it is REAPED, and ownership stays UNKNOWN meanwhile; the job is not resubmitted` };
}

/**
 * GB-52, GB-78, GB-39 (BROKER-CONTRACT section 16): a carried call the broker answered with a refusal that clears on its own (RK_RECOVERY_PENDING, RK_STORAGE_BUDGET_UNAVAILABLE) or whose
 * connection never opened (RK_BROKER_BUSY: every pipe instance busy, no byte sent) is a wait. The gate pauses (250 ms doubling to 5 s) and sends the call again within the call's own
 * deadline (`deadlineMs`, else DEFAULT_RETRY_WINDOW_MS), each attempt with its own carrier, nonce and sub-handle (a handle is spent once; the refused attempt is a settled, spawned:false
 * submission, a busy one is recorded unsent) and under ONE ledger id, so the final record is the call's. It is never success and never a generic failure: when the deadline passes the
 * outcome keeps the broker's own code (`broker-refused-recovery-pending`, `broker-refused-storage-budget-unavailable`, `broker-busy`) and says how long it waited. Every other outcome, a
 * verified one, RK_ADAPTER_PIN_REQUIRED and RK_OPERATION_DEADLINE included, is returned at once. `now` and `sleep` are seams for tests.
 */
function carryWithRetries(once, call) {
  const { deadlineMs = null, now = Date.now, sleep = sleepSync, err = () => {}, record = () => {}, ...rest } = call;
  const window = deadlineMs !== null && Number.isFinite(deadlineMs) ? Math.max(1, Math.trunc(deadlineMs)) : DEFAULT_RETRY_WINDOW_MS;
  const started = now();
  const ledgerId = crypto.randomUUID();
  let attempt = 0; let held = []; let result;
  for (;;) {
    attempt += 1;
    held = [];
    const remaining = Math.max(1, window - (now() - started));
    result = once({ ...rest, record, err: (m) => held.push(m), deadlineMs: deadlineMs === null ? null : remaining, ledgerId });
    const code = result.outcome?.status === "unverified" ? result.outcome.code : null;
    if (code === null || !RETRYABLE_OUTCOME_CODES.has(code)) break;
    const pause = retryPauseMs(attempt);
    if (window - (now() - started) <= pause) {
      const elapsed = now() - started;
      const message = `${result.outcome.message}; retried ${attempt - 1} time(s) over ${elapsed} ms and the call's ${window} ms deadline passed before the broker would take it: it is a wait that did not clear, never a result`;
      result = { ...result, outcome: { ...result.outcome, message, attempts: attempt, retried: attempt - 1 } };
      try { record({ id: ledgerId, mode: "broker", status: "unverified", code: result.outcome.code, message }); } catch { /* the final record above stands */ }
      held.push(`rightkit-gate: ${result.outcome.code}: ${message}`);
      break;
    }
    sleep(pause);
    // The next attempt's pending record replaces this attempt's outcome fields in the ledger (one id).
    try { record({ id: ledgerId, mode: "broker", status: "pending", code: null, message: null }); } catch { /* the attempt records its own pending state */ }
  }
  for (const m of held) err(m);
  return result;
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
export function carryBrokerCargo(call) { return carryWithRetries(carryBrokerCargoOnce, call); }

function carryBrokerCargoOnce({ words, cwd, env, section, workspace, checkoutWorkspace, record, spawn = spawnSync, err, tmp = os.tmpdir(), stdio = "inherit", deadlineMs = null, ledgerId = null }) {
  words = lockedArgv(words); // a build or run is always locked, and a standard alias is expanded first (snapshot-request.mjs lockedArgv)
  const id = ledgerId ?? crypto.randomUUID();
  const base = { id, mode: "broker", cwd, argv: [...words] };
  const refuse = (code, message) => { record({ ...base, status: "refused", code, message }); err(`rightkit-gate: ${code}: ${message}`); return { status: BROKER_REFUSAL_STATUS, outcome: { status: "refused", code, message }, stdout: "", stderr: `rightkit-gate: ${code}: ${message}\n` }; };
  // GB-01: the one command-shape table (cargo-shapes.mjs, exercised against RustRight's cargo-shapes vector) decides BEFORE anything is marked or sent:
  // a shape the broker refuses inside a snapshot (tree, pkgid, verify-project, a bare metadata, a write-form fmt, help, ...) launches zero processes.
  const shape = carriedShapeDecision(words);
  if (!shape.ok) return refuse(shape.code, shape.message);
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
    if (escapes(rel)) throw new Error(`${realCwd} is not inside the workspace ${workspace}`);
  } catch (e) { return refuse("RK_CARGO_BROKER_CWD", `the call's working directory is not inside its workspace (${e.message})`); }
  const counterpart = checkoutCounterpart({ base: checkoutWorkspace, rel, checkout: section.checkout, codePrefix: "RK_CARGO_BROKER_COUNTERPART" });
  if (counterpart.refusal) return refuse(counterpart.refusal.code, counterpart.refusal.message);
  checkoutCwd = counterpart.dir; const spawnCwd = counterpart.spawnCwd;
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
    try { record({ ...base, status: "pending", nonce: made.carrier.nonce, ownershipHandle: made.carrier.ownership.handle, checkoutCwd, spawnCwd }); } catch (e) { removeCarrier(made.file); return refuse("RK_CARGO_LEDGER_ERROR", `the call cannot be recorded (${e.message}); it does not run`); }
    // GB-52: the call's operation deadline (the caller's own cap is never replaced) and an outer guard past the client's own cancel wait.
    const childEnv = { ...clientEnv, ...made.env, ...operationDeadlineEnv({ env: clientEnv, deadlineMs }) };
    const guard = clientGuardMs(childEnv);
    const capture = stdio === "capture";
    // Broker work may exist from the next statement on: the ownership records this request's sub-handle first (a mark that does not land sends nothing).
    { const startable = clientStartable(section.client, section.platform); if (startable) { removeCarrier(made.file); return refuse("RK_CARGO_BROKER_CLIENT", `${startable}; nothing was marked or sent`); } }
    try { markCarriedRequest({ section, env, callHandle: made.carrier.ownership.handle }); } catch (e) { removeCarrier(made.file); return refuse("RK_CARGO_BROKER_OWNERSHIP", e.message); }
    const r = spawnBrokerClient(spawn, section.client, ["cargo", ...words], capture ? { cwd: spawnCwd, env: childEnv, encoding: "utf8", maxBuffer: 1 << 28, windowsHide: true, ...(guard ? { timeout: guard } : {}) } : { cwd: spawnCwd, env: childEnv, stdio: "inherit", ...(guard ? { timeout: guard } : {}) }, { platform: section.platform });
    removeCarrier(made.file);
    // GB-21: a client that provably never started sent nothing: that sub-handle is recorded unsent under the ownership lock, so it does not wedge a later reclaim. Any other
    // failure after a start (a lost connection, a timeout, an exit before a reference) is ambiguous and stays held, reconciled by the request id.
    if (r.error && provenNoSpawn(r.error)) markCarriedUnsentRequest({ section, env, callHandle: made.carrier.ownership.handle, reason: `the RustRight client did not start (${r.error.code})` });
    let outcome;
    let ref = null;
    try {
      if (r.error) throw new SnapshotRequestError(r.error.code === BROKER_CLIENT_UNRESOLVED ? BROKER_CLIENT_UNRESOLVED : "spawn", `the RustRight client did not start (${r.error.message})`);
      ref = readRequestRef(path.join(refDir, "request.json"));
      const receipt = receiptSync({ client: section.client, requestId: ref.requestId, env: clientEnv, spawn, platform: section.platform });
      const observedEnv = observedEnvOf(clientEnv, section.platform);
      const { evidence, attribution, toolchain, requestDigest } = verifySnapshotReceipt(receipt, { pins: section.pins, jobId: ref.requestId, carrier: made.carrier, argv: words, workspace: ref.workspace, observedEnv, checkout: section.checkout, outputRoots: section.outputRoots, platform: section.platform });
      // attribution and toolchain are recorded as the broker signed them (a consumer that needs per-package attribution treats available:false as FAILED).
      outcome = { status: "verified", requestId: ref.requestId, requestDigest, workspace: ref.workspace, observedEnv, outputDir: evidence.outputDir, treeAtStart: evidence.snapshot.treeAtStart, treeAtExit: evidence.snapshot.treeAtExit, attribution, toolchain };
    } catch (e) {
      outcome = { status: "unverified", code: e.code ?? "error", message: e.message, ...(ref ? { requestId: ref.requestId, workspace: ref.workspace } : {}) };
    }
    const out = capture ? { stdout: String(r.stdout ?? ""), stderr: String(r.stderr ?? "") } : { stdout: "", stderr: "" };
    // GB-54: a request the broker REFUSED (admission paused, a suspended or unknown job, a cutover drain) never started: the client's typed REJECTED result is kept as the outcome's
    // code ("admission is paused; nothing was spawned"), not folded into a missing request reference. It is never a build outcome.
    if (outcome.status !== "verified" && capture) {
      const refused = refusalInCapturedOutput(`${out.stdout}\n${out.stderr}`);
      if (refused?.code === "RK_TOOLCHAIN_IDENTITY_UNAVAILABLE") outcome = { status: "unverified", code: "toolchain-unavailable", message: `the broker refused the compile before it started: it could not build the toolchain identity (RK_TOOLCHAIN_IDENTITY_UNAVAILABLE: ${refused.message}); nothing ran`, ...(outcome.requestId ? { requestId: outcome.requestId, workspace: outcome.workspace } : {}) };
      else if (refused) outcome = { status: "unverified", code: `broker-refused-${refused.refusal}`, message: `the broker refused the request (${refused.code ?? "no code"}: ${refused.message || refused.refusal})${refusalAdvice(refused)}`, ...(outcome.requestId ? { requestId: outcome.requestId, workspace: outcome.workspace } : {}) };
    }
    // GB-52: a call that ran out its operation deadline, or whose client was stopped by the outer guard, is NOT a build outcome: the client cancelled by id and watched for the terminal
    // state, and an unobserved state keeps ownership UNKNOWN (the sub-handle stays recorded; reconciled by the request id, never resubmitted). Busy and unavailable are named apart.
    if (outcome.status !== "verified") {
      const seen = operationOutcomeInOutput(`${out.stdout}\n${out.stderr}`) ?? (r.error?.code === "ETIMEDOUT" ? { code: "operation-deadline", deadlineMs: null, requestId: ref?.requestId ?? null, cancelConfirmed: null, terminalState: null, guard: true } : null);
      if (seen) outcome = { status: "unverified", code: seen.code, message: seen.code === "operation-deadline" ? `the operation deadline${seen.deadlineMs ? ` of ${seen.deadlineMs} ms` : ""} passed${seen.guard ? " and the client was stopped by the gate's outer guard" : ""}; request ${seen.requestId ?? ref?.requestId ?? "unknown"}: cancelConfirmed: ${seen.cancelConfirmed ?? "unknown"}, terminal state: ${seen.terminalState ?? "not observed"}; the job is neither abandoned nor resubmitted and ownership stays UNKNOWN until every sub-handle is REAPED` : seen.code === "broker-busy" ? "every broker connection was busy (RK_BROKER_BUSY: a wait, not an outage); nothing was admitted" : "the broker was unavailable (RK_BROKER_UNAVAILABLE)", ...(ref ? { requestId: ref.requestId, workspace: ref.workspace } : {}) };
    }
    if (outcome.status === "unverified" && outcome.code === "operation-deadline") {
      const settled = reconcileAfterCancel({ section, callHandle: made.carrier.ownership.handle, childEnv });
      outcome = { ...outcome, message: `${outcome.message}; ${settled.note}`, reaped: settled.reaped, subHandle: settled.subHandle };
    }
    // RK_BROKER_BUSY with no request reference: the connection never opened, so no byte reached the broker. That is the gate's own proof that this sub-handle was never sent (GB-21), recorded under
    // the ownership lock so the retry's own sub-handle, not this one, is what the stage waits on; a busy answer after a reference exists (a reconnect) proves nothing of the kind.
    if (outcome.status === "unverified" && outcome.code === "broker-busy" && ref === null) markCarriedUnsentRequest({ section, env, callHandle: made.carrier.ownership.handle, reason: "every broker connection was busy (RK_BROKER_BUSY): the connection never opened and no byte was sent" });
    // GB-53: captured output that lost a frame to FRAME_OVER_CAP (or a result the broker replaced with RK_RESPONSE_OVER_CAP) is truncated: refused, never read as complete.
    if (outcome.status === "verified" && capture) {
      const drained = drainFrames(`${out.stdout}\n${out.stderr}`);
      const lost = drained.error ? { code: "frame-over-cap", message: drained.error } : drained.overCap.map(overCapProblem)[0];
      if (lost) outcome = { status: "unverified", code: lost.code, message: lost.message, requestId: outcome.requestId, workspace: outcome.workspace };
    }
    record({ ...base, ...outcome, nonce: made.carrier.nonce, ownershipHandle: made.carrier.ownership.handle, checkoutCwd, spawnCwd, clientExit: r.status ?? null });
    if (outcome.status !== "verified") {
      const message = `rightkit-gate: RK_CARGO_BROKER_UNVERIFIED: \`cargo ${words.join(" ")}\` has no verified signed receipt (${outcome.code}: ${outcome.message}); the scan fails`;
      err(message);
      return { status: r.status ? r.status : BROKER_REFUSAL_STATUS, outcome, ...out, stderr: `${out.stderr}${message}\n` };
    }
    return { status: r.status ?? (r.signal ? 128 : BROKER_REFUSAL_STATUS), outcome, ...out };
  } finally {
    if (made) removeCarrier(made.file);
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
export function carryBrokerAdapter(call) { return carryWithRetries(carryBrokerAdapterOnce, call); }

function carryBrokerAdapterOnce({ adapter, words, cwd, env, section, record, spawn = spawnSync, err, tmp = os.tmpdir(), stdio = "inherit", deadlineMs = null, ledgerId = null }) {
  const id = ledgerId ?? crypto.randomUUID();
  words = [...words].map(String);
  const base = { id, mode: "broker", shim: "adapter", adapter, cwd, argv: [...words] };
  const refuse = (code, message) => { record({ ...base, status: "refused", code, message }); err(`rightkit-gate: ${code}: ${message}`); return { status: BROKER_REFUSAL_STATUS, outcome: { status: "refused", code, message }, stdout: "", stderr: `rightkit-gate: ${code}: ${message}\n` }; };
  const describe = `${adapter} ${words.join(" ")}`.trim();
  // GB-67: the broker owns SwiftPM's scratch and build path; a caller-named one is refused before the carrier, the mark and the send.
  if (adapter === "rightkit-swift") { const owned = swiftScratchRefusal(words); if (owned) return refuse(owned.code, owned.message); }
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
    if (escapes(rel)) throw new Error(`${realCwd} is not inside the snapshot ${snapshotRoot}`);
  } catch (e) { return refuse("RK_ADAPTER_BROKER_CWD", `the step's working directory is not inside the snapshot (${e.message})`); }
  const counterpart = checkoutCounterpart({ base: section.checkout, rel, checkout: section.checkout, codePrefix: "RK_ADAPTER_BROKER_COUNTERPART" });
  if (counterpart.refusal) return refuse(counterpart.refusal.code, counterpart.refusal.message);
  checkoutCwd = counterpart.dir; const spawnCwd = counterpart.spawnCwd;
  let inputs;
  try { inputs = readDeclaredInputs(section.snapshot.path); } catch (e) { return refuse("RK_ADAPTER_BROKER_DECLARED_INPUTS", e.message); }
  const refDir = fs.mkdtempSync(path.join(tmp, "rk-snapreq-"));
  let made = null;
  try {
    try {
      const nonce = /^[0-9a-f]{64}$/.test(stageNonce ?? "") ? carriedCallNonce({ stageNonce, cwd: path.relative(section.checkout, checkoutCwd).split(path.sep).join("/") || ".", argv: words }) : undefined;
      made = writeSnapshotRequest({ controlRoot: section.controlRoot, commit: section.commit, snapshot: section.snapshot, declaredInputs: inputs, handle: section.handle, primaryCwd: checkoutCwd, argv: words, nonce, requestIdFile: path.join(refDir, "request.json") });
    } catch (e) { return refuse("RK_ADAPTER_BROKER_CARRIER", e.message); }
    try { record({ ...base, status: "pending", nonce: made.carrier.nonce, ownershipHandle: made.carrier.ownership.handle, checkoutCwd, spawnCwd }); } catch (e) { removeCarrier(made.file); return refuse("RK_CARGO_LEDGER_ERROR", `the call cannot be recorded (${e.message}); it does not run`); }
    // GB-52: the call's operation deadline (the caller's own cap is never replaced) and an outer guard past the client's own cancel wait.
    const childEnv = { ...clientEnv, ...made.env, ...operationDeadlineEnv({ env: clientEnv, deadlineMs }) };
    const guard = clientGuardMs(childEnv);
    const capture = stdio === "capture";
    // Broker work may exist from the next statement on: the ownership records this request's sub-handle first (a mark that does not land sends nothing).
    { const startable = clientStartable(section.client, section.platform); if (startable) { removeCarrier(made.file); return refuse("RK_ADAPTER_BROKER_CLIENT", `${startable}; nothing was marked or sent`); } }
    try { markCarriedRequest({ section, env, callHandle: made.carrier.ownership.handle }); } catch (e) { removeCarrier(made.file); return refuse("RK_ADAPTER_BROKER_OWNERSHIP", e.message); }
    const r = spawnBrokerClient(spawn, section.client, ["run", adapter, "--", ...words], capture ? { cwd: spawnCwd, env: childEnv, encoding: "utf8", maxBuffer: 1 << 28, windowsHide: true, ...(guard ? { timeout: guard } : {}) } : { cwd: spawnCwd, env: childEnv, stdio: "inherit", ...(guard ? { timeout: guard } : {}) }, { platform: section.platform });
    removeCarrier(made.file);
    // GB-21: a client that provably never started sent nothing: that sub-handle is recorded unsent under the ownership lock, so it does not wedge a later reclaim. Any other
    // failure after a start (a lost connection, a timeout, an exit before a reference) is ambiguous and stays held, reconciled by the request id.
    if (r.error && provenNoSpawn(r.error)) markCarriedUnsentRequest({ section, env, callHandle: made.carrier.ownership.handle, reason: `the RustRight client did not start (${r.error.code})` });
    let outcome;
    let ref = null;
    try {
      if (r.error) throw new SnapshotRequestError(r.error.code === BROKER_CLIENT_UNRESOLVED ? BROKER_CLIENT_UNRESOLVED : "spawn", `the RustRight client did not start (${r.error.message})`);
      ref = readRequestRef(path.join(refDir, "request.json"));
      // The request's workspace of a snapshot-bound adapter job is the directory the client ran in: the primary checkout's counterpart of the step's
      // snapshot directory (the broker, START: "the job runs where it was asked to (request.workspace, already the primary's counterpart of a
      // directory in the snapshot), which must be inside that root"). It is the carrier's primaryCwd, never the toplevel for a step run from a subfolder
      // (an app whose root is `app/`), and never a directory outside the primary checkout or one that is not this call's counterpart.
      if (canonicalCounterpart(ref.workspace) !== canonicalCounterpart(checkoutCwd) || !inside(canonicalCounterpart(ref.workspace), section.checkout)) throw new SnapshotRequestError("workspace", `the request's workspace ${ref.workspace} is not the primary checkout counterpart ${checkoutCwd} of the step's directory ${realCwd} (primary toplevel ${section.checkout})`);
      const receipt = receiptSync({ client: section.client, requestId: ref.requestId, env: clientEnv, spawn, platform: section.platform });
      const { evidence, attribution, toolchain, nestedChildren } = verifySnapshotReceipt(receipt, { pins: section.pins, jobId: ref.requestId, carrier: made.carrier, argv: words, workspace: ref.workspace, adapter, adapterEnv, checkout: section.checkout, outputRoots: section.outputRoots, platform: section.platform });
      outcome = { status: "verified", requestId: ref.requestId, workspace: ref.workspace, outputDir: evidence.outputDir, nestedChildren, treeAtStart: evidence.snapshot.treeAtStart, treeAtExit: evidence.snapshot.treeAtExit, attribution, toolchain };
    } catch (e) {
      outcome = { status: "unverified", code: e.code ?? "error", message: e.message, ...(ref ? { requestId: ref.requestId, workspace: ref.workspace } : {}) };
    }
    const out = capture ? { stdout: String(r.stdout ?? ""), stderr: String(r.stderr ?? "") } : { stdout: "", stderr: "" };
    // GB-54: a request the broker REFUSED (admission paused, a suspended or unknown job, a cutover drain) never started: the client's typed REJECTED result is kept as the outcome's
    // code ("admission is paused; nothing was spawned"), not folded into a missing request reference. It is never a build outcome.
    if (outcome.status !== "verified" && capture) {
      const refused = refusalInCapturedOutput(`${out.stdout}\n${out.stderr}`);
      if (refused?.code === "RK_TOOLCHAIN_IDENTITY_UNAVAILABLE") outcome = { status: "unverified", code: "toolchain-unavailable", message: `the broker refused the compile before it started: it could not build the toolchain identity (RK_TOOLCHAIN_IDENTITY_UNAVAILABLE: ${refused.message}); nothing ran`, ...(outcome.requestId ? { requestId: outcome.requestId, workspace: outcome.workspace } : {}) };
      else if (refused) outcome = { status: "unverified", code: `broker-refused-${refused.refusal}`, message: `the broker refused the request (${refused.code ?? "no code"}: ${refused.message || refused.refusal})${refusalAdvice(refused)}`, ...(outcome.requestId ? { requestId: outcome.requestId, workspace: outcome.workspace } : {}) };
    }
    // GB-52: a call that ran out its operation deadline, or whose client was stopped by the outer guard, is NOT a build outcome: the client cancelled by id and watched for the terminal
    // state, and an unobserved state keeps ownership UNKNOWN (the sub-handle stays recorded; reconciled by the request id, never resubmitted). Busy and unavailable are named apart.
    if (outcome.status !== "verified") {
      const seen = operationOutcomeInOutput(`${out.stdout}\n${out.stderr}`) ?? (r.error?.code === "ETIMEDOUT" ? { code: "operation-deadline", deadlineMs: null, requestId: ref?.requestId ?? null, cancelConfirmed: null, terminalState: null, guard: true } : null);
      if (seen) outcome = { status: "unverified", code: seen.code, message: seen.code === "operation-deadline" ? `the operation deadline${seen.deadlineMs ? ` of ${seen.deadlineMs} ms` : ""} passed${seen.guard ? " and the client was stopped by the gate's outer guard" : ""}; request ${seen.requestId ?? ref?.requestId ?? "unknown"}: cancelConfirmed: ${seen.cancelConfirmed ?? "unknown"}, terminal state: ${seen.terminalState ?? "not observed"}; the job is neither abandoned nor resubmitted and ownership stays UNKNOWN until every sub-handle is REAPED` : seen.code === "broker-busy" ? "every broker connection was busy (RK_BROKER_BUSY: a wait, not an outage); nothing was admitted" : "the broker was unavailable (RK_BROKER_UNAVAILABLE)", ...(ref ? { requestId: ref.requestId, workspace: ref.workspace } : {}) };
    }
    if (outcome.status === "unverified" && outcome.code === "operation-deadline") {
      const settled = reconcileAfterCancel({ section, callHandle: made.carrier.ownership.handle, childEnv });
      outcome = { ...outcome, message: `${outcome.message}; ${settled.note}`, reaped: settled.reaped, subHandle: settled.subHandle };
    }
    // RK_BROKER_BUSY with no request reference: the connection never opened, so no byte reached the broker. That is the gate's own proof that this sub-handle was never sent (GB-21), recorded under
    // the ownership lock so the retry's own sub-handle, not this one, is what the stage waits on; a busy answer after a reference exists (a reconnect) proves nothing of the kind.
    if (outcome.status === "unverified" && outcome.code === "broker-busy" && ref === null) markCarriedUnsentRequest({ section, env, callHandle: made.carrier.ownership.handle, reason: "every broker connection was busy (RK_BROKER_BUSY): the connection never opened and no byte was sent" });
    // GB-53: captured output that lost a frame to FRAME_OVER_CAP (or a result the broker replaced with RK_RESPONSE_OVER_CAP) is truncated: refused, never read as complete.
    if (outcome.status === "verified" && capture) {
      const drained = drainFrames(`${out.stdout}\n${out.stderr}`);
      const lost = drained.error ? { code: "frame-over-cap", message: drained.error } : drained.overCap.map(overCapProblem)[0];
      if (lost) outcome = { status: "unverified", code: lost.code, message: lost.message, requestId: outcome.requestId, workspace: outcome.workspace };
    }
    record({ ...base, ...outcome, nonce: made.carrier.nonce, ownershipHandle: made.carrier.ownership.handle, checkoutCwd, spawnCwd, clientExit: r.status ?? null });
    if (outcome.status !== "verified") {
      const message = `rightkit-gate: RK_ADAPTER_BROKER_UNVERIFIED: \`${describe}\` has no verified signed receipt (${outcome.code}: ${outcome.message}); the stage fails`;
      err(message);
      return { status: r.status ? r.status : BROKER_REFUSAL_STATUS, outcome, ...out, stderr: `${out.stderr}${message}\n` };
    }
    return { status: r.status ?? (r.signal ? 128 : BROKER_REFUSAL_STATUS), outcome, ...out };
  } finally {
    if (made) removeCarrier(made.file);
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
      // GB-16: a counterpart the primary checkout no longer has is the lexical path the carrier named (the client names it as the workspace); it is judged by its nearest existing ancestor.
      const workspace = canonicalCounterpart(e.workspace);
      if (!inside(workspace, sb.checkout)) throw new SnapshotRequestError("ledger", `the request ran in ${workspace}, outside the primary checkout`);
      if (adapter === null && canonicalCounterpart(e.checkoutCwd) !== workspace) throw new SnapshotRequestError("ledger", "the request's working directory is not the one the call recorded");
      if (adapter !== null) {
        // The workspace is the directory the client ran in, the carrier's primaryCwd; the step's snapshot directory and that checkout directory are the
        // same path below their two roots.
        if (workspace !== canonicalCounterpart(e.checkoutCwd)) throw new SnapshotRequestError("ledger", `the adapter request's workspace ${workspace} is not the primary checkout counterpart ${e.checkoutCwd} of the step's directory`);
        const below = path.relative(fs.realpathSync.native(sb.snapshot.path), fs.realpathSync.native(e.cwd));
        if (escapes(below)) throw new SnapshotRequestError("ledger", `the step ran from ${e.cwd}, outside the snapshot`);
        if (path.relative(sb.checkout, canonicalCounterpart(e.checkoutCwd)) !== below) throw new SnapshotRequestError("ledger", `the carrier named ${e.checkoutCwd}, not the checkout counterpart of ${e.cwd}`);
      }
      // The gate's own truth: its commit, snapshot, handle and declared inputs; only the call's own argv, nonce and cwd come from the ledger.
      const carrier = {
        schema: SNAPSHOT_REQUEST_SCHEMA, commit: sb.commit, snapshot: sb.snapshot, declaredInputs: { inputs: readDeclaredInputs(sb.snapshot.path) },
        ownership: { handle: e.ownershipHandle }, primaryCwd: e.checkoutCwd, argvSha256: argvSha256(e.argv), nonce: e.nonce,
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
