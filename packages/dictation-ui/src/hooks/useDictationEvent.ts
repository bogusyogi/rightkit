// useDictationEvent: one host event subscription per mount. Always calls the latest handler (via a
// ref, so a changing closure does not re-subscribe); cleanup waits for the subscription so an
// unmount before it resolves still unsubscribes; failures are swallowed (a preview host may have no
// events).

import { useEffect, useRef } from "react";
import { dictationClient, type DictationEventMap } from "../host.js";

export function useDictationEvent<K extends keyof DictationEventMap>(
  event: K,
  handler: (payload: DictationEventMap[K]) => void,
): void {
  const handlerRef = useRef(handler);
  handlerRef.current = handler;

  useEffect(() => {
    let pending: Promise<() => void>;
    try {
      pending = dictationClient().on(event, (payload) => handlerRef.current(payload));
    } catch {
      return;
    }
    return () => {
      void pending.then((unsubscribe) => unsubscribe()).catch(() => undefined);
    };
  }, [event]);
}
