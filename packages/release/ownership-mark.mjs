// The build-ownership record's broker-submit mark and the constants every process under an ownership shares. A LEAF module: it imports
// only node builtins, so snapshot-broker.mjs (carried sends, in the gate and in its children) and build-ownership.mjs can both use it
// without an import cycle.
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import { LOCK_SAME_START_MS, lockBootId, lockProcessIdentity, selfLockIdentity } from "./lock-identity.mjs";

export const OWNERSHIP_ENV = "RIGHTKIT_BUILD_OWNERSHIP";
/** The owner record's path, exported beside the handle so every process under the ownership records detached children there. */
export const OWNERSHIP_FILE_ENV = "RIGHTKIT_BUILD_OWNERSHIP_FILE";

/**
 * Another build holds the host, or its liveness is not provable. `retryable` is true only when the holder is a LIVE gate (or a live reclaimer): a wait
 * can end it. An unreadable record, another host, an unknown liveness or a held lease never wait: they need an operator (N36c).
 */
export class OwnershipBusy extends Error {
  constructor(message, detail = {}) { super(message); this.code = "busy"; this.detail = detail; this.retryable = detail.retryable === true; }
}
/** The gate's exit status for a retryable busy host (EX_TEMPFAIL): another gate is running; the same command may be run again, or with `--wait`. */
export const OWNERSHIP_BUSY_RETRYABLE_EXIT = 75;

const readText = (file) => { try { return fs.readFileSync(file, "utf8"); } catch (e) { if (e.code === "ENOENT") return null; throw e; } };

export function holds(file, handle) {
  try { return JSON.parse(fs.readFileSync(file, "utf8")).handle === handle; } catch { return false; }
}

/**
 * Records, immediately before ONE snapshot-bound (carried) request is sent, that broker work may exist under this handle: sets
 * `broker.submitted` and appends the request's sub-handle (`<handle>#<uuid>`) to `broker.handles`. From then on the record is
 * reclaimed only on the broker's REAPED for the bare handle. Nothing else marks it: installing a carrier map, an uncarried probe
 * (`rustc -vV`, `--version`, doctor) and a request that was never sent leave `submitted` false, because the broker has no lease
 * for a handle it never saw a snapshot-bound request under and could never answer REAPED. Replaces the record atomically, only
 * while it is still this handle's, under a short lock beside the record (carried calls of parallel children, in other
 * processes, mark the same record).
 */
export function markBrokerSubmit(file, handle, callHandle, options = {}) {
  if (typeof callHandle !== "string" || callHandle.length <= handle.length + 1 || !callHandle.startsWith(`${handle}#`) || !/^[A-Za-z0-9._-]+$/.test(callHandle.slice(handle.length + 1))) {
    throw new OwnershipBusy(`build ownership ${file}: a broker submit is recorded only for a carried request's sub-handle of ${handle} (<handle>#<id>), not ${JSON.stringify(callHandle ?? null)}`);
  }
  return withMarkLock(file, () => {
    const text = readText(file);
    let owner = null; try { owner = JSON.parse(text ?? ""); } catch { owner = null; }
    if (owner?.handle !== handle) throw new OwnershipBusy(`build ownership ${file} is no longer held by ${handle}; no broker request may be submitted`);
    const seen = Array.isArray(owner.broker?.handles) ? owner.broker.handles : [];
    if (owner.broker?.submitted && seen.includes(callHandle)) return owner;
    const next = { ...owner, broker: { ...(owner.broker ?? {}), submitted: true, at: owner.broker?.at ?? new Date().toISOString(), handles: [...seen, callHandle] } };
    const tmp = `${file}.${crypto.randomUUID()}.tmp`;
    fs.writeFileSync(tmp, JSON.stringify(next), { flag: "wx", mode: 0o600 });
    try { fs.renameSync(tmp, file); } finally { fs.rmSync(tmp, { force: true }); }
    if (!holds(file, handle)) throw new OwnershipBusy(`build ownership ${file} changed while recording a broker submit`);
    return next;
  }, options);
}

/**
 * Takes back the mark of ONE carried request that was provably never sent (its client never started: the spawn itself failed). Under the same lock
 * as the mark, removes `callHandle` from `broker.handles` and recomputes `submitted` from the handles that remain, so a record whose only mark was
 * this one is again `submitted: false` and reclaimable without a lease the broker never had. A handle that is not listed is left alone (another
 * path may have marked the record), and a handle whose request may have been sent is never passed here. Returns the record.
 */
export function unmarkBrokerSubmit(file, handle, callHandle, options = {}) {
  return withMarkLock(file, () => {
    const text = readText(file);
    let owner = null; try { owner = JSON.parse(text ?? ""); } catch { owner = null; }
    if (owner?.handle !== handle) return owner; // no longer this handle's: nothing of ours to take back
    const seen = Array.isArray(owner.broker?.handles) ? owner.broker.handles : [];
    if (!seen.includes(callHandle)) return owner;
    const remaining = seen.filter((h) => h !== callHandle);
    const broker = { ...(owner.broker ?? {}), handles: remaining, submitted: remaining.length > 0 };
    if (remaining.length === 0) { delete broker.at; delete broker.handles; }
    const next = { ...owner, broker };
    const tmp = `${file}.${crypto.randomUUID()}.tmp`;
    fs.writeFileSync(tmp, JSON.stringify(next), { flag: "wx", mode: 0o600 });
    try { fs.renameSync(tmp, file); } finally { fs.rmSync(tmp, { force: true }); }
    if (!holds(file, handle)) throw new OwnershipBusy(`build ownership ${file} changed while taking back an unsent broker submit`);
    return next;
  }, options);
}

const MARK_LOCK_WAIT_MS = 15_000;
export const MARK_LOCK_SCHEMA = "rightkit.build-ownership-mark-lock/1";
/** The lock beside the record that serializes `broker.handles` updates. Removed with the record (release, reclaim). */
export const markLockFile = (file) => `${file}.mark.lock`;
const stealClaimFile = (file, nonce) => `${markLockFile(file)}.steal-${nonce}`;

const defaultSleep = (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
const parseLock = (text) => { try { const rec = JSON.parse(text); return rec && typeof rec === "object" && rec.schema === MARK_LOCK_SCHEMA && typeof rec.nonce === "string" && /^[0-9a-f-]{36}$/.test(rec.nonce) ? rec : null; } catch { return null; } };
const readLockText = (lock) => { try { return fs.readFileSync(lock, "utf8"); } catch (e) { if (e.code === "ENOENT") return null; throw e; } };

/**
 * Whether the holder recorded in a lock is `"dead"` (proven), `"live"`, or `"unknown"`. Only `"dead"` ever allows a steal: age proves nothing
 * (a suspended live holder can resume), so a lock is taken from its holder only on proof, the same proofs the ownership record uses
 * (build-ownership.mjs judgeOwner): the host rebooted (boot ids differ), the pid is gone, or the pid now runs a process that started at another
 * time. Anything unreadable, another host, or an identity that cannot be read is not proof.
 */
export function judgeLockHolder(rec, { identity, host = os.hostname() }) {
  if (!rec || !Number.isInteger(rec.pid) || rec.pid <= 0) return "unknown";
  if (rec.host !== host) return "unknown";
  let boot = null; try { boot = identity.bootId(); } catch { boot = null; }
  if (typeof rec.bootId === "string" && rec.bootId && typeof boot === "string" && boot && rec.bootId !== boot) return "dead";
  let now;
  try { now = identity.probe(rec.pid); } catch { return "unknown"; }
  if (!now.alive) return "dead";
  if (!Number.isFinite(rec.startedMs) || !Number.isFinite(now.startedMs)) return "unknown";
  return Math.abs(now.startedMs - rec.startedMs) <= LOCK_SAME_START_MS ? "live" : "dead";
}

/** Creates `target` holding `record` atomically and exclusively: the content is complete the moment the name exists (write a temp file, hard-link it). */
function createExclusive(target, record) {
  const tmp = `${target}.${record.nonce}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(record), { flag: "wx", mode: 0o600 });
  try { fs.linkSync(tmp, target); return true; }
  catch (e) { if (e.code === "EEXIST") return false; throw e; }
  finally { fs.rmSync(tmp, { force: true }); }
}

/**
 * Runs `fn` under the lock beside the record. The lock names its holder (pid, start time, boot id) and carries a random nonce:
 *   - a holder that is alive is never displaced, however long it holds (it may be suspended and resume);
 *   - a holder PROVEN dead is displaced by one claimant per observed lock (a claim file named by the dead lock's nonce), which removes
 *     the lock only while it is still the exact one it judged;
 *   - when liveness is uncertain the caller waits, and past the deadline fails closed with OwnershipBusy (no request is sent);
 *   - release removes the lock only while it still carries this holder's nonce, so a holder that somehow lost it never removes a successor's.
 * `options` are seams for tests: `identity` ({ self(), probe(pid), bootId() }), `sleep`, `waitMs`.
 */
export function withMarkLock(file, fn, options = {}) {
  const lock = markLockFile(file);
  const identity = options.identity ?? { self: () => selfLockIdentity(), probe: (pid) => lockProcessIdentity(pid), bootId: () => lockBootId() };
  const sleep = options.sleep ?? defaultSleep;
  const deadline = Date.now() + (options.waitMs ?? MARK_LOCK_WAIT_MS);
  let me;
  try { me = identity.self(); } catch (e) { throw new OwnershipBusy(`build ownership ${file}: this process's identity cannot be read (${e.message}), so it cannot take the submit lock`); }
  const record = { schema: MARK_LOCK_SCHEMA, pid: me.pid, startedMs: me.startedMs, bootId: me.bootId ?? null, host: os.hostname(), nonce: crypto.randomUUID(), at: new Date().toISOString() };
  const take = (target) => {
    try { return createExclusive(target, record); }
    catch (e) { throw new OwnershipBusy(`build ownership ${file}: the submit lock cannot be taken (${e.code ?? e.message})`); }
  };
  let lastVerdict = "unknown";
  for (;;) {
    if (take(lock)) break;
    let text;
    try { text = readLockText(lock); } catch (e) { throw new OwnershipBusy(`build ownership ${file}: the submit lock cannot be read (${e.code ?? e.message})`); }
    if (text === null) continue; // it was released between the create and the read
    const observed = parseLock(text);
    lastVerdict = observed ? judgeLockHolder(observed, { identity }) : "unknown";
    if (lastVerdict === "dead" && stealDeadLock({ file, lock, text, observed, record, identity, take })) continue;
    if (Date.now() > deadline) throw new OwnershipBusy(`build ownership ${file}: the submit lock ${lock} is held${observed ? ` by pid ${observed.pid}` : " by a holder that cannot be read"} and ${lastVerdict === "live" ? "is alive" : "its liveness cannot be proven"}; no broker request is sent`, { holder: observed, verdict: lastVerdict });
    sleep(10);
  }
  try { return fn(); } finally {
    try { const held = parseLock(readLockText(lock) ?? ""); if (held?.nonce === record.nonce) fs.rmSync(lock, { force: true }); } catch { /* the lock stays; a later caller proves its holder dead */ }
  }
}

/** Removes the lock `text`/`observed`, whose holder is proven dead, if this process wins the one claim for that lock. Returns whether the retry should go straight on. */
function stealDeadLock({ file, lock, text, observed, record, identity, take }) {
  const claim = stealClaimFile(file, observed.nonce);
  if (!take(claim)) {
    // Another process is stealing the same dead lock, or died doing it: a claim whose holder is proven dead is dropped, anything else waits.
    const claimText = readLockText(claim);
    if (claimText !== null && judgeLockHolder(parseLock(claimText), { identity }) === "dead") fs.rmSync(claim, { force: true });
    return false;
  }
  try {
    if (readLockText(lock) === text) fs.rmSync(lock, { force: true });
  } finally {
    try { if (parseLock(readLockText(claim) ?? "")?.nonce === record.nonce) fs.rmSync(claim, { force: true }); } catch { /* a dead claim is dropped by the next claimant */ }
  }
  return true;
}

/**
 * Marks the record at `file` as having sent the carried request `callHandle`, for a caller that holds no ownership object (a
 * right-release child of the gate, in another process, which inherits the handle and file in its environment). Throws
 * OwnershipBusy unless `handle` is still the record's owner.
 */
export function markCarriedSend({ file, handle, callHandle }) {
  if (typeof file !== "string" || !file || typeof handle !== "string" || !handle) throw new OwnershipBusy("a carried request is marked only under a build ownership file and handle");
  return markBrokerSubmit(file, handle, callHandle);
}

/** Takes back the mark of a carried request that was provably never sent (see unmarkBrokerSubmit), for a caller that holds no ownership object. */
export function unmarkCarriedSend({ file, handle, callHandle }) {
  if (typeof file !== "string" || !file || typeof handle !== "string" || !handle) throw new OwnershipBusy("an unsent carried request is taken back only under a build ownership file and handle");
  return unmarkBrokerSubmit(file, handle, callHandle);
}
