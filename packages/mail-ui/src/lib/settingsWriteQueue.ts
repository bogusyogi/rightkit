// Serialises writes per static setting so a fire-and-forget save can neither
// land out of order (rapid toggles) nor be overtaken by a refresh that reads
// the old committed value. After a write settles, the store is reconciled from
// the value the backend confirms, never from the optimistic guess.

const tails = new Map<string, Promise<unknown>>();
const pending = new Map<string, number>();

/** Run `task` after every earlier task for `key` has settled (success or not). */
export function enqueueSettingWrite<T>(key: string, task: () => Promise<T>): Promise<T> {
  const prev = tails.get(key) ?? Promise.resolve();
  pending.set(key, (pending.get(key) ?? 0) + 1);
  const run = prev.then(task, task);
  const tail = run.then(
    () => undefined,
    () => undefined,
  );
  tails.set(key, tail);
  void tail.then(() => {
    const left = (pending.get(key) ?? 1) - 1;
    if (left <= 0) {
      pending.delete(key);
      if (tails.get(key) === tail) tails.delete(key);
    } else {
      pending.set(key, left);
    }
  });
  return run;
}

/** Resolve once every write queued so far (including ones queued meanwhile) is done. */
export async function settleSettingWrites(): Promise<void> {
  while (tails.size > 0) {
    await Promise.all([...tails.values()]);
  }
}

export type SettingSave<T> = {
  key: string;
  /** Show the user's intent immediately. */
  optimistic?: () => void;
  /** Commit to the backend. */
  write: () => Promise<unknown>;
  /** Read the committed value back from the backend. */
  read: () => Promise<T>;
  /** Put a confirmed value into the store. */
  apply: (confirmed: T) => void;
  onError?: (e: unknown) => void;
};

/** Optimistic save: queue the write, then reconcile from the confirmed value.
 *  Only the newest queued write for a key reconciles, so a burst of toggles
 *  settles on the last one instead of flickering through older values. On
 *  failure the confirmed value replaces the optimistic one and `onError` fires. */
export function saveSetting<T>(s: SettingSave<T>): Promise<void> {
  s.optimistic?.();
  const op = enqueueSettingWrite(s.key, async () => {
    let failure: unknown;
    let failed = false;
    try {
      await s.write();
    } catch (e) {
      failed = true;
      failure = e;
    }
    // Still the newest write for this key? (count includes this one.)
    if ((pending.get(s.key) ?? 1) <= 1) {
      try {
        s.apply(await s.read());
      } catch {
        /* keep current state if the backend cannot be read */
      }
    }
    if (failed) throw failure;
  });
  return op.catch((e) => s.onError?.(e));
}
