// Launch-at-login toggle whose value lives in the operating system, not the settings document: read on mount, written on
// change, reverted with an error text when the write fails. The two calls are the host's. Imports only `react`.
import { useCallback, useEffect, useRef, useState } from "react";

export interface UseLaunchAtLoginOptions {
  /** Reads the OS state (the host's command). A rejection reads as off. */
  read(): Promise<boolean>;
  /** Writes the OS state (the host's command). */
  write(enabled: boolean): Promise<unknown>;
  /** False where the setting does not exist: nothing is read. Default true. */
  active?: boolean;
}

export interface LaunchAtLogin {
  /** The state, or null until the first read settles. */
  enabled: boolean | null;
  /** Text of the last failed write; "" when none. */
  error: string;
  setError(message: string): void;
  /** Shows `next` at once, then writes it; a failed write puts the old value back and sets `error` to `String(reason)`. */
  toggle(next: boolean): void;
}

/** See the module comment. A late read after unmount is ignored. */
export function useLaunchAtLogin(options: UseLaunchAtLoginOptions): LaunchAtLogin {
  const { read, write, active = true } = options;
  const [enabled, setEnabled] = useState<boolean | null>(null);
  const [error, setError] = useState("");
  const writeRef = useRef(write);
  writeRef.current = write;

  useEffect(() => {
    if (!active) return;
    let alive = true;
    read()
      .then((value) => alive && setEnabled(value))
      .catch(() => alive && setEnabled(false));
    return () => {
      alive = false;
    };
    // `read` is the host's module-level binding.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);

  const toggle = useCallback(
    (next: boolean) => {
      setEnabled(next);
      setError("");
      void writeRef.current(next).catch((reason) => {
        setEnabled(!next);
        setError(String(reason));
      });
    },
    [],
  );

  return { enabled, error, setError, toggle };
}
