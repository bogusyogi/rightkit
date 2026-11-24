// A recent-rows store shared by every screen that shows them (a home page and a title bar). The change
// subscription and the refetch it triggers live once at module scope and fan out to every mounted consumer,
// instead of each mount subscribing and fetching independently and doubling the IPC per change. Concurrent
// fetches share one in-flight call. Imports only `react`.
import { useEffect, useState } from "react";

export interface SharedRecentOptions<T> {
  fetch(): Promise<T[]>;
  /** Calls `onChange` when stored rows change; resolves the unsubscribe. A rejection (a preview host with no events) allows a retry on the next mount. */
  subscribe(onChange: () => void): Promise<() => void>;
}

export interface SharedRecent<T> {
  /** Fetches the rows once (concurrent callers share the call) and publishes them to every consumer. */
  fetch(): Promise<T[]>;
  /** The shared rows; starts the change subscription on first mount. */
  use(): T[];
}

export function createSharedRecent<T>(options: SharedRecentOptions<T>): SharedRecent<T> {
  const subscribers = new Set<(items: T[]) => void>();
  let shared: T[] = [];
  let listenerStarted = false;
  let inFlight: Promise<T[]> | null = null;

  function publish(items: T[]): void {
    shared = items;
    for (const subscriber of subscribers) subscriber(items);
  }

  function fetchRecent(): Promise<T[]> {
    inFlight ??= options
      .fetch()
      .then((items) => {
        publish(items);
        return items;
      })
      .finally(() => {
        inFlight = null;
      });
    return inFlight;
  }

  function ensureListener(): void {
    if (listenerStarted) return;
    listenerStarted = true;
    options
      .subscribe(() => {
        void fetchRecent().catch(() => undefined);
      })
      .catch(() => {
        listenerStarted = false;
      });
  }

  function use(): T[] {
    const [items, setItems] = useState<T[]>(() => shared);
    useEffect(() => {
      ensureListener();
      const subscriber = (next: T[]) => setItems(next);
      subscribers.add(subscriber);
      return () => {
        subscribers.delete(subscriber);
      };
    }, []);
    return items;
  }

  return { fetch: fetchRecent, use };
}
