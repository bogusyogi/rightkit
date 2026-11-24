import { useEffect, useLayoutEffect, useRef } from 'react';
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
