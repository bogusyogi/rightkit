// The live place of a carried scan in the broker's queue (rightkit#1367 step 2).
//
// The broker, not the gate, decides how many builds run (Adrian, 2026-11-01), so a carried scan can wait in the broker's queue. That wait is not the
// scan's own run, so the scan's run limit is a stopwatch that does not tick while the broker holds every request of the scan QUEUED. The gate asks the
// broker (`rustright status <id>`: BROKER-CONTRACT BR7 reports live broker state in every status answer, state QUEUED until a permit is held, then RUNNING)
// about the requests the scan has made. It gives queue time no allowance of its own: how long a job may stay queued is the broker's rule (BR7 reports a queued job past
// its deadline with the holder that blocks it). The one bound the gate does add is a wall-clock ceiling (QUEUED_CEILING_FACTOR x the scan's limit) that is not a build
// count: a safety stop for a wedged broker queue (the broker's job deadlines are off by default, so nothing else would end a scan stuck QUEUED).
//
// What the client does not offer: its operation deadline (RUSTRIGHT_OPERATION_DEADLINE_MS, client.rs, BROKER-CONTRACT GB-52) is an absolute deadline from the
// client's start, so it would charge queue time to the scan. A watched scan therefore sends no operation deadline; at the end of its own run time the gate
// cancels the requests by id (`rustright cancel <id>`) and stops the process tree only after a short grace.
//
// Fail closed: a request the gate cannot read (no request id yet, a status that fails or is malformed, a state other than QUEUED) is counted as running, so
// an unreadable broker never makes a scan immortal.
import fs from "node:fs";
import { spawnBrokerClient } from "./broker-client.mjs";
import path from "node:path";

const REQUEST_REF_SCHEMA = "rustright.request-ref.v1";
/** The request reference the client writes at submit (RUSTRIGHT_REQUEST_ID_FILE: `{ schema, requestId, workspace }`), or a throw (moved here from snapshot-request.mjs, deleted at plan step 5). */
function readRequestRef(file) {
  let ref;
  try { ref = JSON.parse(fs.readFileSync(file, "utf8")); } catch (e) { throw new Error(`no request reference at ${file} (${e.message})`); }
  if (ref?.schema !== REQUEST_REF_SCHEMA || typeof ref.requestId !== "string" || !ref.requestId || typeof ref.workspace !== "string" || !path.isAbsolute(ref.workspace)) throw new Error(`${file} is not a ${REQUEST_REF_SCHEMA}`);
  return ref;
}

/** How long a cancelled carried scan may take to end by itself before its process tree is stopped. */
export const CARRIED_CANCEL_GRACE_MS = 15_000;
/** How often the gate asks the broker for the state of its requests (the first read comes after 1 s, then every 5 s). */
export const ADMISSION_POLL_MS = 5_000;
const FIRST_POLL_MS = 1_000;
/** How many `rustright status` reads the gate has in flight at once, however many scans it watches. */
export const STATUS_POLL_CONCURRENCY = 2;
/**
 * A watched scan is stopped when its WALL-CLOCK time (run time plus queue time) reaches its own run limit times this factor. It is a safety stop for a wedged broker queue,
 * not a count: the broker decides how many builds run and in what order, and its job deadlines are off by default, so without this a scan stuck QUEUED would wait forever.
 */
export const QUEUED_CEILING_FACTOR = 4;

/**
 * A run stopwatch: `onExpire` fires once, when `limitMs` of RUNNING time has passed. `pause()` stops it (the scan is queued at the broker) and `resume()`
 * restarts it with the time left; `stop()` ends it for good. Time spent paused is `queuedMs`.
 * With `ceilingMs` (a wall-clock bound that includes the paused time) `onCeiling` fires once when it passes before the stopwatch expired or stopped, and `ceilingHit` is true.
 */
export function createRunClock({ limitMs, onExpire, ceilingMs = null, onCeiling = () => {}, setTimer = setTimeout, clearTimer = clearTimeout, now = Date.now }) {
  let remaining = Math.max(0, Number(limitMs) || 0);
  let handle = null; let since = 0; let pausedAt = null; let ceilingHandle = null;
  let expired = false; let stopped = false; let queuedMs = 0; let ceilingHit = false;
  const clearCeiling = () => { if (ceilingHandle !== null) { clearTimer(ceilingHandle); ceilingHandle = null; } };
  const arm = () => {
    since = now();
    handle = setTimer(() => { handle = null; expired = true; clearCeiling(); onExpire(); }, remaining);
  };
  arm();
  if (ceilingMs !== null && Number.isFinite(ceilingMs) && ceilingMs > 0) {
    ceilingHandle = setTimer(() => {
      ceilingHandle = null;
      if (stopped || expired) return;
      ceilingHit = true; expired = true;
      if (handle !== null) { clearTimer(handle); handle = null; }
      if (pausedAt !== null) { queuedMs += now() - pausedAt; pausedAt = null; }
      onCeiling();
    }, ceilingMs);
    ceilingHandle?.unref?.();
  }
  return {
    pause() { if (stopped || expired || handle === null) return; clearTimer(handle); handle = null; remaining = Math.max(0, remaining - (now() - since)); pausedAt = now(); },
    resume() { if (stopped || expired || handle !== null) return; if (pausedAt !== null) { queuedMs += now() - pausedAt; pausedAt = null; } arm(); },
    stop() { if (handle !== null) { clearTimer(handle); handle = null; } clearCeiling(); if (pausedAt !== null) { queuedMs += now() - pausedAt; pausedAt = null; } stopped = true; },
    get expired() { return expired; },
    get ceilingHit() { return ceilingHit; },
    get queuedMs() { return queuedMs + (pausedAt !== null ? now() - pausedAt : 0); },
    get running() { return handle !== null; },
    get remainingMs() { return handle !== null ? Math.max(0, remaining - (now() - since)) : remaining; },
  };
}

/** The live state word (`QUEUED`, `RUNNING`, ...) of one `rustright status` reply, or null when it is not a valid STATUS of this request. */
export function brokerStateOf(stdout, requestId) {
  let doc;
  try { doc = JSON.parse(String(stdout)); } catch { return null; }
  const diagnosis = diagnoseControlReply(doc, requestId);
  return diagnosis.kind === "live" || diagnosis.kind === "terminal" ? diagnosis.state : null;
}

/**
 * The gate's reader of the broker's live state, through the RustRight client with an ASYNCHRONOUS spawn (nothing here blocks the gate's event loop):
 * `state(id)` is the request's state word or null; `cancel(id)` is `rustright cancel <id>`.
 */
export function createBrokerLive({ client, env = process.env, spawn, platform = process.platform }) {
  const call = async (args) => {
    try {
      const r = await spawnBrokerClient(spawn, client, args, { encoding: "utf8", env, timeout: 30_000, maxBuffer: 1 << 24, windowsHide: true }, { platform });
      return r && !r.error ? r : null;
    } catch { return null; }
  };
  return {
    async state(requestId) { const r = await call(["status", requestId]); return r ? brokerStateOf(r.stdout, requestId) : null; },
    async cancel(requestId) { await call(["cancel", requestId]); },
  };
}

/** The request ids the reference files name (a file not written yet, or already removed, names none). */
export function requestIdsOf(files) {
  const ids = [];
  for (const file of files) {
    try { if (fs.existsSync(file)) ids.push(readRequestRef(file).requestId); } catch { /* not a complete reference yet */ }
  }
  return [...new Set(ids)];
}

/**
 * ONE reader of the broker's live state per gate run, shared by every watched scan (rightkit#1367 step 2): a scan registers a member (`ids()` lists its request ids, `apply(states)` takes
 * the answers) and the poller asks for each distinct id with at most `concurrency` reads in flight, the first read `firstMs` after the first member arrives and then every `intervalMs`
 * while any member is registered. Passes never overlap. `pass(extra)` is one read pass (a member not registered can be passed in `extra`; tests use it). Status reads go to the client
 * directly: they are not builds and are never recorded as such.
 */
export function createStatusPoller({ live, intervalMs = ADMISSION_POLL_MS, firstMs = Math.min(FIRST_POLL_MS, intervalMs), concurrency = STATUS_POLL_CONCURRENCY, setTimer = setTimeout, clearTimer = clearTimeout }) {
  const members = new Set();
  let timer = null; let tail = Promise.resolve(); let passes = 0;
  const width = Math.max(1, Math.trunc(Number(concurrency)) || 1);
  async function readAll(extra) {
    const snapshot = [...new Set([...members, ...extra])];
    const wanted = [...new Set(snapshot.flatMap((m) => m.ids()))];
    const states = new Map();
    const queue = [...wanted];
    await Promise.all(Array.from({ length: Math.min(width, queue.length) }, async () => {
      while (queue.length) {
        const id = queue.shift();
        states.set(id, await Promise.resolve().then(() => live.state(id)).catch(() => null));
      }
    }));
    for (const member of snapshot) if (members.has(member) || extra.includes(member)) member.apply(member.ids().map((id) => (states.has(id) ? states.get(id) : null)));
  }
  function pass(extra = []) {
    passes += 1;
    const run = tail.then(() => readAll(extra));
    tail = run.catch(() => {}).finally(() => { passes -= 1; });
    return run;
  }
  const schedule = (ms) => {
    timer = setTimer(() => { timer = null; pass().catch(() => {}).finally(() => { if (members.size > 0 && timer === null) schedule(intervalMs); }); }, ms);
    timer?.unref?.();
  };
  return {
    pass,
    /** Resolves when the passes started so far have ended. */
    idle: () => tail,
    /** Adds a member; returns the function that removes it. The poller stops its own timer when no member is left. */
    register(member) {
      members.add(member);
      if (timer === null && passes === 0) schedule(firstMs);
      return () => { members.delete(member); if (members.size === 0 && timer !== null) { clearTimer(timer); timer = null; } };
    },
    get members() { return members.size; },
  };
}

/**
 * Watches the requests one scan makes and keeps its `clock` honest: paused while every request the scan has is QUEUED at the broker, running otherwise.
 * `refFiles()` lists the request reference files (RUSTRIGHT_REQUEST_ID_FILE, written by the client at submit) the scan's carried calls have; `live` is a
 * createBrokerLive (or any object with `state(id)` and `cancel(id)`). The reads come from a shared `poller` (createStatusPoller, one per gate run); with none a private one is made
 * from `live`. `poll()` is one pass (tests call it directly) and resolves to this scan's states; `start()` registers with the poller until `stop()`.
 * `cancelKnown()` cancels, by id, every request seen so far.
 */
export function watchBrokerAdmission({ clock, refFiles, live, poller = null, intervalMs = ADMISSION_POLL_MS, firstMs = Math.min(FIRST_POLL_MS, intervalMs), setTimer = setTimeout, clearTimer = clearTimeout }) {
  let stopped = false; let unregister = null; let last = [];
  const known = new Set();
  const own = poller ?? createStatusPoller({ live, intervalMs, firstMs, setTimer, clearTimer });
  const member = {
    ids() { const ids = requestIdsOf(refFiles()); for (const id of ids) known.add(id); return ids; },
    apply(states) {
      last = states;
      if (stopped) return;
      if (states.length === 0) { clock.resume(); return; }
      if (states.every((state) => state === "QUEUED")) clock.pause(); else clock.resume();
    },
  };
  return {
    async poll() { if (stopped) return null; await own.pass([member]); return last; },
    done: () => own.idle(),
    start() { if (!stopped && unregister === null) unregister = own.register(member); },
    stop() { stopped = true; if (unregister) { unregister(); unregister = null; } },
    known: () => [...known],
    async cancelKnown() {
      for (const id of requestIdsOf(refFiles())) known.add(id);
      await Promise.allSettled([...known].map((id) => live.cancel(id)));
    },
  };
}

// ---- moved from broker-wire.mjs (deleted at reset step 5): the parts the seam's run clock still uses ----
const TERMINAL_STATES = new Set(["SUCCEEDED", "FAILED", "CANCELLED", "TIMED_OUT", "REJECTED"]);
const REFUSAL_CLASSES = Object.freeze({ RK_ADMISSION_PAUSED: "paused", RK_JOB_SUSPENDED: "suspended", RK_JOB_NOT_FOUND: "not-found", RK_ACTIVATION_PENDING: "activation-pending", RK_CUTOVER_DRAIN_PENDING: "cutover-drain" });
const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

// ---- the typed refusals of BROKER-CONTRACT section 16: each its own diagnosis, and whether waiting clears it ----
//   RK_BROKER_BUSY                  GB-52  every pipe instance stayed busy: the connection never opened, no byte was sent (a wait, not an outage)           retryable
//   RK_RECOVERY_PENDING             GB-78  restart recovery of the workspace is still running after the bounded wait: nothing was admitted                  retryable
//   RK_STORAGE_BUDGET_UNAVAILABLE   GB-39  the storage budget could not be settled within bounds.budgetAdmissionWaitMs: the root was not admitted           retryable
//   RK_ADAPTER_PIN_REQUIRED         GB-51  a Gradle wrapper without a pinned distributionSha256Sum: START failed before any spawn                         not retryable (the commit must pin it)
//   RK_OPERATION_DEADLINE           GB-52  the client's operation deadline passed: it cancelled by id and watched for the terminal state                   not retryable (never resubmitted)
// A retryable refusal is a wait the gate makes within its own deadline (each retry a new carrier and sub-handle, a refused submission being a settled, spawned:false member); it is never
// success and never a generic failure. `refusal` is the class name a diagnosis carries.
export const TYPED_REFUSALS = Object.freeze({
  RK_BROKER_BUSY: Object.freeze({ refusal: "broker-busy", retryable: true }),
  RK_RECOVERY_PENDING: Object.freeze({ refusal: "recovery-pending", retryable: true }),
  RK_STORAGE_BUDGET_UNAVAILABLE: Object.freeze({ refusal: "storage-budget-unavailable", retryable: true }),
  RK_ADAPTER_PIN_REQUIRED: Object.freeze({ refusal: "adapter-pin-required", retryable: false }),
  RK_OPERATION_DEADLINE: Object.freeze({ refusal: "operation-deadline", retryable: false }),
});

/**
 * The typed refusal one reply names, or null: the event's (or its receipt's) `code`, else a message that STARTS with the code (`RK_RECOVERY_PENDING: restart recovery ...`, which is how the
 * broker words them). A code merely mentioned inside other text is not a refusal. `{ code, refusal, retryable }`.
 */
export function typedRefusalOf(...sources) {
  for (const source of sources) {
    if (!isObject(source)) continue;
    const named = typeof source.code === "string" && Object.hasOwn(TYPED_REFUSALS, source.code) ? source.code : null;
    const prefixed = named === null ? ["error", "message"].map((key) => (typeof source[key] === "string" ? /^\s*(?:error:\s*)?(RK_[A-Z0-9_]+)\b/.exec(source[key])?.[1] : null)).find((code) => code && Object.hasOwn(TYPED_REFUSALS, code)) ?? null : null;
    const code = named ?? prefixed;
    if (code !== null) return { code, ...TYPED_REFUSALS[code] };
  }
  return null;
}

/**
 * What a client concludes from one STATUS or RESULT line about a request it knows by id. Only `terminal` is a build outcome, and only after the receipt's own signed
 * validation; every other kind keeps the broker's code, state and identity instead of collapsing into a generic receipt failure, and a known request is reconciled by
 * its durable id, never resubmitted because a diagnosis improved:
 *   { kind: "live", id, state }                  non-terminal with an absent or null receipt (pending; never a result, never green)
 *   { kind: "terminal", id, state, receipt }      a terminal state with a receipt object whose id is the request's
 *   { kind: "unavailable", id, reason }           the front gate's STATUS with state UNAVAILABLE and its diagnostic code
 *   { kind: "refused", code, refusal, message }   a RESULT with status REJECTED: paused, suspended, not-found, activation-pending, cutover-drain, other
 *   { kind: "malformed", message }                not a typed object, another request's id or receipt, no state, a terminal state without a receipt, a receipt that is not an object
 */
export function diagnoseControlReply(event, expectedId = null) {
  const malformed = (message) => ({ kind: "malformed", message });
  if (!isObject(event) || typeof event.type !== "string") return malformed("the reply is not a typed object");
  const id = typeof event.id === "string" ? event.id : null;
  if (expectedId !== null && id !== null && expectedId !== id) return malformed("the reply names another request");
  const receiptOf = (state) => {
    const receipt = event.receipt;
    if (isObject(receipt)) {
      const receiptId = typeof receipt.id === "string" ? receipt.id : null;
      const want = expectedId ?? id;
      if (want !== null && receiptId !== null && want !== receiptId) return malformed("the receipt is for another request");
      if (want !== null && receiptId === null) return malformed("the receipt names no request");
      // A root the broker refused before it ran (a storage budget that could not be settled, a recovery still running, a wrapper without a pinned distribution) ends FAILED with its code:
      // it is the typed refusal, never a receipt to validate and never a build outcome. Only a failed or rejected state is read so; a SUCCEEDED receipt is never turned into a refusal.
      const typed = state === "FAILED" || state === "REJECTED" ? typedRefusalOf(receipt, { error: event.error }) : null;
      if (typed) return { kind: "refused", id, state, code: typed.code, refusal: typed.refusal, retryable: typed.retryable, message: typeof receipt.error === "string" ? receipt.error : typeof event.error === "string" ? event.error : "" };
      return { kind: "terminal", id, state, receipt };
    }
    if (receipt === undefined || receipt === null) return TERMINAL_STATES.has(state) ? malformed("a finished job's reply carries no receipt") : { kind: "live", id, state };
    return malformed("the receipt is not an object");
  };
  if (event.type === "STATUS") {
    if (typeof event.state !== "string" || event.state === "") return malformed("the status carries no state");
    if (event.state === "UNAVAILABLE") {
      const reason = (isObject(event.diagnostic) && typeof event.diagnostic.code === "string" ? event.diagnostic.code : null) ?? (typeof event.code === "string" ? event.code : null) ?? "unavailable";
      return { kind: "unavailable", id, reason };
    }
    return receiptOf(event.state);
  }
  if (event.type === "RESULT") {
    if (event.status === "REJECTED") {
      const code = typeof event.code === "string" ? event.code : null;
      const typed = typedRefusalOf(event);
      if (typed) return { kind: "refused", code: typed.code, refusal: typed.refusal, retryable: typed.retryable, message: typeof event.error === "string" ? event.error : "" };
      return { kind: "refused", code, refusal: (code !== null && Object.hasOwn(REFUSAL_CLASSES, code) ? REFUSAL_CLASSES[code] : "other"), retryable: false, message: typeof event.error === "string" ? event.error : "" };
    }
    return typeof event.status === "string" && TERMINAL_STATES.has(event.status) ? receiptOf(event.status) : malformed("the result carries no terminal status");
  }
  return malformed("the reply is not a STATUS or RESULT");
}


// ---- GB-52: an end-to-end operation deadline and explicit cancellation for carried calls (BROKER-CONTRACT section 16) ----
// RustRight's client bounds one submit-and-watch operation with RUSTRIGHT_OPERATION_DEADLINE_MS (absent or 0: none; a user-set test cap or stall limit is never replaced
// by a universal duration). Past it the client does not abandon or resubmit the job: it sends CANCEL by id and polls STATUS for the terminal state within
// RUSTRIGHT_CANCEL_WAIT_MS (default 30000), failing with `operation deadline of N ms passed; request <id>: cancelConfirmed: <bool>, terminal state: <state | not observed>`.
// An unobserved state keeps ownership unknown. `RK_BROKER_BUSY` (every pipe instance busy: a wait) is distinct from `RK_BROKER_UNAVAILABLE`.
export const OPERATION_DEADLINE_ENV = "RUSTRIGHT_OPERATION_DEADLINE_MS";
export const CANCEL_WAIT_ENV = "RUSTRIGHT_CANCEL_WAIT_MS";
export const DEFAULT_CANCEL_WAIT_MS = 30_000;
/** The gate's remaining stage budget, as an absolute epoch-ms instant a stage sets for its children (unset: no budget). */
export const STAGE_DEADLINE_AT_ENV = "RK_GATE_STAGE_DEADLINE_AT";

/**
 * The environment delta that gives one carried call its operation deadline: the caller's own RUSTRIGHT_OPERATION_DEADLINE_MS is never replaced (a user-set cap wins);
 * otherwise `deadlineMs` (a scan's or stage's own limit) or the time left to `deadlineAt` (STAGE_DEADLINE_AT_ENV in `env`), at least 1 ms. `{}` when there is no budget.
 */
export function operationDeadlineEnv({ env = {}, deadlineMs = null, deadlineAt = null, now = Date.now() } = {}) {
  const set = Object.entries(env).find(([key]) => key.toUpperCase() === OPERATION_DEADLINE_ENV);
  if (set && String(set[1]).trim() !== "") return {};
  const at = deadlineAt ?? (Number.isFinite(Number(env[STAGE_DEADLINE_AT_ENV])) && Number(env[STAGE_DEADLINE_AT_ENV]) > 0 ? Number(env[STAGE_DEADLINE_AT_ENV]) : null);
  const budget = deadlineMs ?? (at !== null ? at - now : null);
  if (!Number.isFinite(budget) || budget === null) return {};
  return { [OPERATION_DEADLINE_ENV]: String(Math.max(1, Math.trunc(budget))) };
}
