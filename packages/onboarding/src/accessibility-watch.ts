// Hub-level accessibility watch: a granted flag that is re-checked on mount, on focus and on visibility change, polled
// only while access is absent, with a once-per-grant shortcut activation. No host names: the port is the host's.
import { useEffect, useRef, useState } from "react";

/** The native side of the watch. */
export interface AccessibilityWatchPort {
  /** Resolves whether accessibility access is granted, without prompting. */
  status(): Promise<boolean>;
  /** Called once each time access turns granted (again after a revoke). A failure re-arms the next check. */
  activateShortcuts?(): Promise<unknown>;
}

export interface UseAccessibilityWatchOptions {
  /** Watch only when true (e.g. on macOS); otherwise `granted` stays true and nothing is checked. */
  enabled: boolean;
  /** Poll interval while access is absent. Default 2000. */
  pollMs?: number;
}

/**
 * Returns `{ granted }`. Assumes granted until the first check resolves, so a banner never flashes. While access is
 * absent it polls every `pollMs`; a healthy app makes no repeated native calls. The host draws the banner and its
 * button from `granted`.
 */
export function useAccessibilityWatch(port: AccessibilityWatchPort, options: UseAccessibilityWatchOptions): { granted: boolean } {
  const { enabled, pollMs = 2000 } = options;
  const [granted, setGranted] = useState(true);
  const activated = useRef(false);
  const portRef = useRef(port);
  portRef.current = port;

  useEffect(() => {
    if (!enabled) return;
    let alive = true;
    const check = () =>
      portRef.current
        .status()
        .then((ok) => {
          if (!alive) return;
          setGranted(ok);
          if (ok && !activated.current) {
            activated.current = true;
            const activate = portRef.current.activateShortcuts;
            if (activate) {
              void activate.call(portRef.current).catch(() => {
                activated.current = false;
              });
            }
          } else if (!ok) {
            activated.current = false;
          }
        })
        .catch(() => undefined);
    void check();
    const id = granted ? undefined : window.setInterval(check, pollMs);
    const onFocus = () => void check();
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onFocus);
    return () => {
      alive = false;
      if (id !== undefined) window.clearInterval(id);
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onFocus);
    };
  }, [granted, enabled, pollMs]);

  return { granted: enabled ? granted : true };
}
