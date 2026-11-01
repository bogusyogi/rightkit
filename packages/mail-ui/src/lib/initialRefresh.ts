// Signal that the first unlocked full refresh has settled. The store has no
// field for this: accounts and activeAccount look identical before the first
// refresh and after a refresh that found zero accounts. refreshAction marks it;
// the thread-list gate subscribes through useSyncExternalStore.

let settled = false;
const listeners = new Set<() => void>();

export function markInitialRefreshSettled(): void {
  if (settled) return;
  settled = true;
  for (const listener of listeners) listener();
}

export function isInitialRefreshSettled(): boolean {
  return settled;
}

export function subscribeInitialRefreshSettled(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
