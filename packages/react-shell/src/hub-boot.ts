// Boot glue for a hub window: a one-time settings read, a settings-changed window event, the setup gate
// (first-run and acceptance state) and a persisted theme choice. Imports only `react`. What is read, applied and
// shown belongs to the host.
import { useCallback, useEffect, useRef, useState } from "react";

/** Options of {@link useStartupRead}. */
export interface UseStartupReadOptions<T> {
  /** Reads the value (a settings command). Called once, on mount. */
  read: () => Promise<T>;
  /** Called with the value when the read succeeds. Apply it to state, theme and fonts here. */
  onLoaded: (value: T) => void;
  /** Called when the read throws. Put the app in its safe state here (for example fail into onboarding). */
  onFailed: (error: unknown) => void;
}

/**
 * Reads once on mount and hands the result to `onLoaded`, or the error to `onFailed`. Callbacks may change
 * between renders; the latest are used. Not cancelled on unmount (like a plain mount effect, a late result only
 * reaches state setters), and under StrictMode the read runs twice.
 */
export function useStartupRead<T>(options: UseStartupReadOptions<T>): void {
  const ref = useRef(options);
  ref.current = options;
  useEffect(() => {
    void (async () => {
      try {
        const value = await ref.current.read();
        ref.current.onLoaded(value);
      } catch (error) {
        ref.current.onFailed(error);
      }
    })();
  }, []);
}

/**
 * Listens for a `CustomEvent` on `window` and calls `handler` with its `detail`. The listener is added once; the
 * latest `handler` runs. The host dispatches the event (for example after saving settings in another view).
 */
export function useWindowCustomEvent<D>(eventName: string, handler: (detail: D) => void): void {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => {
    const listener = (event: Event) => ref.current((event as CustomEvent<D>).detail);
    window.addEventListener(eventName, listener);
    return () => window.removeEventListener(eventName, listener);
  }, [eventName]);
}

/** Options of {@link useHubSetupGate}. */
export interface UseHubSetupGateOptions {
  /** Onboarding done; `undefined` until known. */
  onboarded: boolean | undefined;
  /** Legal acceptance current; `undefined` until known. */
  legalAccepted: boolean | undefined;
  /**
   * Runs once the state is known and setup is NOT complete (and again if the pair changes while incomplete).
   * Show and focus the window here; it may be async. Errors are swallowed: showing the window is best effort.
   */
  onSetupIncomplete?: () => void | Promise<void>;
  /**
   * Runs when setup is complete (on every change of the pair that leaves it complete); the returned function
   * runs on cleanup. Start background work here (update checks, subscriptions).
   */
  onSetupComplete?: () => void | (() => void);
  /**
   * `document.documentElement.dataset` key set to `"1"` as soon as both values are known, as a ready flag for
   * end-to-end tests. Default: none.
   */
  readyDatasetKey?: string;
}

/**
 * The gate between boot and the running hub. While either value is `undefined` nothing runs. Once both are
 * known: the ready flag is set, then `onSetupIncomplete` runs when onboarding or acceptance is pending, or
 * `onSetupComplete` when both hold. Returns `{ known, complete }` for rendering. Callbacks are read at effect
 * time, so they need no `useCallback`.
 */
export function useHubSetupGate(options: UseHubSetupGateOptions): { known: boolean; complete: boolean } {
  const { onboarded, legalAccepted, readyDatasetKey } = options;
  const ref = useRef(options);
  ref.current = options;
  const known = onboarded !== undefined && legalAccepted !== undefined;
  const complete = !!onboarded && !!legalAccepted;

  useEffect(() => {
    if (onboarded === undefined || legalAccepted === undefined) return;
    if (onboarded && legalAccepted) return;
    void (async () => {
      try {
        await ref.current.onSetupIncomplete?.();
      } catch {
        // Best effort.
      }
    })();
  }, [onboarded, legalAccepted]);

  useEffect(() => {
    if (known && readyDatasetKey) document.documentElement.dataset[readyDatasetKey] = "1";
  }, [known, readyDatasetKey]);

  useEffect(() => {
    if (!complete) return;
    return ref.current.onSetupComplete?.() ?? undefined;
  }, [complete]);

  return { known, complete };
}

/** Options of {@link useThemeChoiceSave}. */
export interface UseThemeChoiceSaveOptions<C extends string> {
  /** Persists the choice (a settings patch) and resolves with the value the backend stored. */
  persist: (choice: C) => Promise<unknown>;
  /** Applies the choice to the shell (a theme store's `save`). Gets the stored value when valid, else `choice`. */
  apply: (choice: C) => void;
  /** True when a stored value is one of the valid choices. */
  isChoice: (value: unknown) => value is C;
  /** Error text when persisting fails. */
  errorLabel: string;
}

/**
 * A theme picker's change handler: clears the error, persists, then applies what the backend stored (falling
 * back to the picked choice when the stored value is not a valid choice). On failure `error` holds
 * `errorLabel` and nothing is applied. `onChange` keeps one identity across renders.
 */
export function useThemeChoiceSave<C extends string>(options: UseThemeChoiceSaveOptions<C>) {
  const ref = useRef(options);
  ref.current = options;
  const [error, setError] = useState<string | null>(null);
  const onChange = useCallback(async (choice: C) => {
    setError(null);
    try {
      const stored = await ref.current.persist(choice);
      ref.current.apply(ref.current.isChoice(stored) ? stored : choice);
    } catch {
      setError(ref.current.errorLabel);
    }
  }, []);
  const clearError = useCallback(() => setError(null), []);
  return { error, onChange, clearError };
}
