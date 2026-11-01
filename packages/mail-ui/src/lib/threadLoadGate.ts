// Gate for the thread list's load triggers. Cold start used to call
// list_threads from the mount effect before the app lock, accounts and labels
// were known, then again as each one arrived. The FIRST load now waits for the
// initial refresh and the startup labels fetch. Every later trigger loads
// exactly as before, subject only to the unlock, category and query guards.

export type ThreadLoadInputs = {
  /** False while the app lock is engaged; the backend refuses reads then. */
  appUnlocked: boolean;
  /** The Inbox is waiting for a Gmail category to be picked. */
  categoryPending: boolean;
  /** A search query is active; the thread list is not the source. */
  hasQuery: boolean;
  /** The first unlocked full refresh has settled (lock status and accounts known). */
  refreshSettled: boolean;
  /** The startup labels fetch has resolved (labels decide categoryPending). */
  labelsSettled: boolean;
};

export type ThreadLoadGate = { startupLoaded: boolean };

export function shouldLoadThreads(
  s: ThreadLoadInputs & { startupLoaded: boolean },
): boolean {
  if (!s.appUnlocked || s.categoryPending || s.hasQuery) return false;
  if (s.startupLoaded) return true;
  return s.refreshSettled && s.labelsSettled;
}

/** Decide whether this effect run issues a load. The first issued load is
 *  recorded in `gate`, which closes the startup window for good. */
export function claimThreadLoad(gate: ThreadLoadGate, inputs: ThreadLoadInputs): boolean {
  if (!shouldLoadThreads({ ...inputs, startupLoaded: gate.startupLoaded })) return false;
  gate.startupLoaded = true;
  return true;
}
