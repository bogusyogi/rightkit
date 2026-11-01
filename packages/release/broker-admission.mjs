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
import { diagnoseControlReply } from "./broker-wire.mjs";
import { readRequestRef } from "./snapshot-request.mjs";

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
