// useTauriEvent - single safe pattern for Tauri event subscriptions.
//
// Subscribes once per event name; always invokes the latest handler (via a
// ref, so a changing handler closure does NOT re-subscribe); cleanup awaits
// the `listen` promise so an unmount before it resolves still unsubscribes;
// failures are swallowed so browser-preview (no Tauri IPC) doesn't throw.
//
// Use where it removes duplicate listener wiring (App, Home dashboard), not
// where a component needs bespoke per-event logic.

import { useEffect, useRef } from "react";
import { listen } from "../lib/tauri.js";

export function useTauriEvent<T>(
  event: string,
  handler: (payload: T) => void,
): void {
  const handlerRef = useRef(handler);
  handlerRef.current = handler;

  useEffect(() => {
    const unlistenPromise = listen<T>(event, (e) => handlerRef.current(e.payload));
    return () => {
      void unlistenPromise.then((unlisten) => unlisten()).catch(() => undefined);
    };
  }, [event]);
}
