// The build-ownership record's broker-submit mark and the constants every process under an ownership shares. A LEAF module: it imports
// only node builtins, so snapshot-broker.mjs (carried sends, in the gate and in its children) and build-ownership.mjs can both use it
// without an import cycle.
import crypto from "node:crypto";
import fs from "node:fs";

export const OWNERSHIP_ENV = "RIGHTKIT_BUILD_OWNERSHIP";
/** The owner record's path, exported beside the handle so every process under the ownership records detached children there. */
export const OWNERSHIP_FILE_ENV = "RIGHTKIT_BUILD_OWNERSHIP_FILE";

export class OwnershipBusy extends Error {
  constructor(message, detail = {}) { super(message); this.code = "busy"; this.detail = detail; }
}

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
export function markBrokerSubmit(file, handle, callHandle) {
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
  });
}

const MARK_LOCK_STALE_MS = 10_000;
const MARK_LOCK_WAIT_MS = 15_000;
/** The lock beside the record that serializes `broker.handles` updates. Removed with the record (release, reclaim). */
export const markLockFile = (file) => `${file}.mark.lock`;
function withMarkLock(file, fn) {
  const lock = markLockFile(file);
  const sleep = (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
  const deadline = Date.now() + MARK_LOCK_WAIT_MS;
  for (;;) {
    try { fs.writeFileSync(lock, String(process.pid), { flag: "wx", mode: 0o600 }); break; }
    catch (e) {
      if (e.code !== "EEXIST") throw new OwnershipBusy(`build ownership ${file}: the submit lock cannot be taken (${e.code ?? e.message})`);
    }
    // A marker holds the lock for milliseconds; one older than the stale bound died inside it (a crash leaves no work to keep).
    try { if (Date.now() - fs.statSync(lock).mtimeMs > MARK_LOCK_STALE_MS) { fs.rmSync(lock, { force: true }); continue; } } catch { continue; }
    if (Date.now() > deadline) throw new OwnershipBusy(`build ownership ${file}: the submit lock ${lock} stayed held; no broker request is sent`);
    sleep(10);
  }
  try { return fn(); } finally { fs.rmSync(lock, { force: true }); }
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
