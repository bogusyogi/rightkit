import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import type { TauriSubscribe } from './events.js';

export { useRecoveryNotice } from './recovery-notice.js';
export type { RecoveryNotice, RecoveryNoticeStatus, UseRecoveryNoticeOptions } from './recovery-notice.js';

/**
 * One native event subscription per mount, for a `subscribe` made with `defineTauriEvents`. The handler
 * is always the latest committed one (kept in a ref, so a changing closure does not re-subscribe).
 * Cleanup waits for the subscription, so an unmount before it resolves still unsubscribes. A rejected
 * subscription (a preview host with no events) is swallowed.
 *
 * `onReady` runs once after the listener is registered, so a caller can read the state the event would
 * have announced without a gap between its read and the registration. It is skipped when the
 * subscription was cleaned up or replaced first, and its throw, rejection or non-settlement never
 * affects the unlisten. The handler and `onReady` are published at commit, and a listener only reaches
 * them while its own `subscribe` and `event` are the committed ones, so a listener that is being
 * replaced cannot deliver an event or a readiness signal to its successor's callbacks.
 */
export function useTauriEvent<Spec, K extends keyof Spec & string>(
  subscribe: TauriSubscribe<Spec>,
  event: K,
  handler: (payload: Spec[K]) => void,
  onReady?: () => void | Promise<void>,
): void {
  const committed = useRef({ subscribe, event, handler, onReady });
  useLayoutEffect(() => {
    committed.current = { subscribe, event, handler, onReady };
  });

  useEffect(() => {
    let live = true;
    const owns = () =>
      live && committed.current.subscribe === subscribe && committed.current.event === event;
    let pending: Promise<() => void>;
    try {
      pending = subscribe(event, (payload) => {
        if (owns()) committed.current.handler(payload);
      });
    } catch {
      return;
    }
    void pending.then(
      () => {
        if (!owns()) return;
        try {
          void Promise.resolve(committed.current.onReady?.()).catch(() => undefined);
        } catch {
          // A readiness callback that throws must not leave the subscription unmanaged.
        }
      },
      () => undefined,
    );
    return () => {
      live = false;
      void pending.then((unsubscribe) => unsubscribe()).catch(() => undefined);
    };
  }, [subscribe, event]);
}

export interface UseHotkeyConflictOptions<Spec, K extends keyof Spec & string> {
  /** Reads the current conflict (`null` = none). Called once per mount and again after a failed retry. */
  read: () => Promise<Spec[K]>;
  /** Asks the native side to register the shortcuts again. A rejection means the conflict persists. */
  retry: () => Promise<unknown>;
  /** The app's `subscribe` made with `defineTauriEvents`. */
  subscribe: TauriSubscribe<Spec>;
  /** The event that announces a registration status; its payload replaces the conflict. */
  event: K;
}

export interface HotkeyConflictState<C> {
  /** The conflict last read or announced; `null` before the first read and when there is none. */
  conflict: C | null;
  /** `retry` is in flight. */
  retrying: boolean;
  /** Retries the registration. A failure re-reads the conflict (the shown one stays if that read fails too). Never rejects. */
  retry: () => Promise<void>;
}

/**
 * State and action of a "keyboard shortcuts could not be registered" notice: the conflict read once per mount, kept current by
 * the registration-status event (through `useTauriEvent`), and the retry. A successful retry leaves the conflict to the event.
 * The wording and markup stay in the app.
 */
export function useHotkeyConflict<Spec, K extends keyof Spec & string>(
  options: UseHotkeyConflictOptions<Spec, K>,
): HotkeyConflictState<Spec[K]> {
  const [conflict, setConflict] = useState<Spec[K] | null>(null);
  const [retrying, setRetrying] = useState(false);
  const latest = useRef({ read: options.read, retry: options.retry, conflict });
  latest.current = { read: options.read, retry: options.retry, conflict };

  useEffect(() => {
    void latest.current
      .read()
      .then(setConflict)
      .catch(() => undefined);
  }, []);

  useTauriEvent(options.subscribe, options.event, (payload) => setConflict(payload));

  const retry = useCallback(async () => {
    setRetrying(true);
    try {
      await latest.current.retry();
    } catch {
      const shown = latest.current.conflict;
      setConflict(await latest.current.read().catch(() => shown));
    } finally {
      setRetrying(false);
    }
  }, []);

  return { conflict, retrying, retry };
}

export interface UseRefreshedValueOptions<T> {
  /** Reads the value. Should not reject: a rejection is swallowed and leaves the value as it was unless `fallback` is given. */
  read: () => Promise<T>;
  /** The value before the first read settles. */
  initial: T;
  /** The value a failed read puts in place of the last one. Omit to keep the last value on failure. */
  fallback?: { value: T };
  /** Names of events dispatched on `window` (`window.dispatchEvent(new Event(name))`) that trigger a re-read. */
  events?: readonly string[];
  /** Also re-read when the window gains focus. Default true. */
  onFocus?: boolean;
}

export interface RefreshedValue<T> {
  value: T;
  /** Reads now, as an event would. */
  refresh: () => void;
}

/**
 * A value read on mount and read again whenever the window gains focus or one of the named window events fires. A read that
 * settles after unmount is ignored. The latest `read` is used, so a changing closure does not re-subscribe.
 */
export function useRefreshedValue<T>(options: UseRefreshedValueOptions<T>): RefreshedValue<T> {
  const { events = [], onFocus = true } = options;
  const [value, setValue] = useState<T>(options.initial);
  const latest = useRef(options);
  latest.current = options;
  const alive = useRef(true);
  const eventKey = events.join('\n');

  const refresh = useCallback(() => {
    latest.current
      .read()
      .then((next) => {
        if (alive.current) setValue(next);
      })
      .catch(() => {
        const fallback = latest.current.fallback;
        if (alive.current && fallback) setValue(fallback.value);
      });
  }, []);

  useEffect(() => {
    alive.current = true;
    refresh();
    const names = eventKey === '' ? [] : eventKey.split('\n');
    for (const name of names) window.addEventListener(name, refresh);
    if (onFocus) window.addEventListener('focus', refresh);
    return () => {
      alive.current = false;
      for (const name of names) window.removeEventListener(name, refresh);
      if (onFocus) window.removeEventListener('focus', refresh);
    };
  }, [eventKey, onFocus, refresh]);

  return { value, refresh };
}
