import { useEffect, useRef } from 'react';
import type { TauriSubscribe } from './events.js';

export { useRecoveryNotice } from './recovery-notice.js';
export type { RecoveryNotice, RecoveryNoticeStatus, UseRecoveryNoticeOptions } from './recovery-notice.js';

/**
 * One native event subscription per mount, for a `subscribe` made with `defineTauriEvents`. The handler
 * is always the latest one (kept in a ref, so a changing closure does not re-subscribe). Cleanup waits
 * for the subscription, so an unmount before it resolves still unsubscribes. A rejected subscription
 * (a preview host with no events) is swallowed.
 */
export function useTauriEvent<Spec, K extends keyof Spec & string>(
  subscribe: TauriSubscribe<Spec>,
  event: K,
  handler: (payload: Spec[K]) => void,
): void {
  const handlerRef = useRef(handler);
  handlerRef.current = handler;

  useEffect(() => {
    let live = true;
    let pending: Promise<() => void>;
    try {
      pending = subscribe(event, (payload) => {
        if (live) handlerRef.current(payload);
      });
    } catch {
      return;
    }
    void pending.catch(() => undefined);
    return () => {
      live = false;
      void pending.then((unsubscribe) => unsubscribe()).catch(() => undefined);
    };
  }, [subscribe, event]);
}
