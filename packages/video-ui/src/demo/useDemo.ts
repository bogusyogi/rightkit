import { useCallback, useEffect, useState } from "react";
import {
  type DemoConsent,
  type DemoState,
  type DemoStatus,
  demoStatus,
  onDemoConsent,
  onDemoState,
} from "./bindings";

const IDLE: DemoState = { phase: "idle", session: null, bundle: null, message: null, hotkey: "" };

/** Live demo state: the saved on/off flag, the session phase and any consent waiting for an answer. */
export function useDemo() {
  const [status, setStatus] = useState<DemoStatus | null>(null);
  const [state, setState] = useState<DemoState>(IDLE);
  const [consent, setConsent] = useState<DemoConsent | null>(null);

  const refresh = useCallback(async () => {
    try {
      const s = await demoStatus();
      setStatus(s);
      setState(s.state);
      setConsent(s.pending_consent);
    } catch {
      // A build without the demo feature has no such command: demo mode simply is not there.
      setStatus(null);
    }
  }, []);

  useEffect(() => {
    void refresh();
    const unlisten = Promise.all([
      onDemoState((s) => {
        setState(s);
        setStatus((prev) => (prev ? { ...prev, state: s, hotkey: s.hotkey } : prev));
        if (s.phase !== "awaiting_consent") setConsent(null);
      }),
      onDemoConsent((c) => setConsent(c)),
    ]);
    return () => {
      void unlisten.then((fns) => fns.forEach((f) => f()));
    };
  }, [refresh]);

  return { status, state, consent, refresh };
}
