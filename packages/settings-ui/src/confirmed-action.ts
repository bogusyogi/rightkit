// Settings action that asks first: confirm, then run, then report the outcome as one status line. The confirmation, the action and
// the wording are the host's. Imports only `react`.
import { useCallback, useRef, useState } from "react";

export interface ConfirmedActionMessages<T = unknown> {
  /** Status text when `confirm` itself throws (the confirmation could not be shown). Nothing runs. */
  confirmFailed(reason: unknown): string;
  /** Status text after `run` resolves. A string, or a function of what `run` resolved with. */
  succeeded: string | ((value: T) => string);
  /** Status text when `run` throws. */
  failed(reason: unknown): string;
}

export interface UseConfirmedActionOptions<T = unknown> {
  /** Asks the user. Resolves false to cancel (nothing changes, the previous status stays); a throw reports `messages.confirmFailed`. */
  confirm(): Promise<boolean>;
  /** The action (the host's command). */
  run(): Promise<T>;
  messages: ConfirmedActionMessages<T>;
}

export interface ConfirmedActionResult {
  ok: boolean;
  text: string;
}

export interface ConfirmedAction {
  /** `run` is in flight. */
  busy: boolean;
  /** The last outcome (cleared when a new run starts); null before the first and after a start that has not settled. */
  result: ConfirmedActionResult | null;
  /** Confirm, then run. Never rejects. */
  start(): Promise<void>;
}

/** See the module comment. A cancelled confirmation leaves `result` as it was. */
export function useConfirmedAction<T = unknown>(options: UseConfirmedActionOptions<T>): ConfirmedAction {
  const latest = useRef(options);
  latest.current = options;
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<ConfirmedActionResult | null>(null);

  const start = useCallback(async () => {
    const { confirm, run, messages } = latest.current;
    try {
      if (!(await confirm())) return;
    } catch (reason) {
      setResult({ ok: false, text: messages.confirmFailed(reason) });
      return;
    }
    setBusy(true);
    setResult(null);
    try {
      const value = await run();
      setResult({ ok: true, text: typeof messages.succeeded === "function" ? messages.succeeded(value) : messages.succeeded });
    } catch (reason) {
      setResult({ ok: false, text: messages.failed(reason) });
    } finally {
      setBusy(false);
    }
  }, []);

  return { busy, result, start };
}
