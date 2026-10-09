// Bounded detach for hdiutil mounts. Spotlight, Finder, and quick-look helpers can hold a freshly
// written volume for a moment, so a first `hdiutil detach` regularly fails with "Resource busy".
// Retry a fixed number of times with exponential backoff, then fall back to `-force` once.

export const DETACH_TRIES = 5;
export const DETACH_BACKOFF_MS = Object.freeze([500, 1000, 2000, 4000]);

const blockingSleep = (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
const describe = (result) => String(result?.error?.message ?? result?.stderr ?? result?.status ?? "failed").trim();

/**
 * Synchronous. `detach(args)` runs `hdiutil <args>` and returns `{ status, stderr?, error? }`.
 * Returns `{ ok, attempts, forced }`; throws only through the caller when `ok` is false.
 */
export function detachWithRetrySync({ detach, mount, sleep = blockingSleep, log = () => {}, tries = DETACH_TRIES, backoffMs = DETACH_BACKOFF_MS }) {
  let last;
  for (let attempt = 1; attempt <= tries; attempt += 1) {
    last = detach(["detach", mount]);
    if (!last?.error && last?.status === 0) return { ok: true, attempts: attempt, forced: false, result: last };
    const wait = backoffMs[Math.min(attempt - 1, backoffMs.length - 1)];
    log({ event: "hdiutil-detach-retry", mount, attempt, of: tries, retryInMs: attempt < tries ? wait : 0, message: describe(last) });
    if (attempt < tries) sleep(wait);
  }
  log({ event: "hdiutil-detach-force", mount, message: describe(last) });
  const forced = detach(["detach", "-force", mount]);
  return { ok: !forced?.error && forced?.status === 0, attempts: tries, forced: true, result: forced };
}

/** Async twin for promise-based runners: `detach(args)` resolves on success and rejects on failure. */
export async function detachWithRetry({ detach, mount, sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)), log = () => {}, tries = DETACH_TRIES, backoffMs = DETACH_BACKOFF_MS }) {
  let last;
  for (let attempt = 1; attempt <= tries; attempt += 1) {
    try { await detach(["detach", mount]); return { ok: true, attempts: attempt, forced: false }; }
    catch (error) { last = error; }
    const wait = backoffMs[Math.min(attempt - 1, backoffMs.length - 1)];
    log({ event: "hdiutil-detach-retry", mount, attempt, of: tries, retryInMs: attempt < tries ? wait : 0, message: String(last?.message ?? last) });
    if (attempt < tries) await sleep(wait);
  }
  log({ event: "hdiutil-detach-force", mount, message: String(last?.message ?? last) });
  try { await detach(["detach", "-force", mount]); return { ok: true, attempts: tries, forced: true }; }
  catch (error) { return { ok: false, attempts: tries, forced: true, error }; }
}
