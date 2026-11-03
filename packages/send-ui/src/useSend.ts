import { useCallback, useEffect, useRef, useState } from "react";
import type { SendClient } from "./client.js";
import type { SendSnapshot, TrustEntry, TrustState } from "./types.js";

export interface UseSendOptions {
  /**
   * Called to hear that the service changed (the host forwards the crate's `Event` callback to the
   * webview, for example as a Tauri event). Returns the unsubscribe function. Without it the hook
   * polls every `pollMs`.
   */
  subscribe?: (onChange: () => void) => () => void;
  /** Poll period when no `subscribe` is given. Default 2000. */
  pollMs?: number;
}

export interface SendState {
  snapshot: SendSnapshot | null;
  trust: TrustEntry[];
  defaultState: TrustState;
  /** The last command that failed, as the value it threw. */
  error: unknown;
  /** Runs a command, then reloads. Failures land in `error` and are rethrown. */
  run: <T>(action: (client: SendClient) => Promise<T>) => Promise<T>;
  reload: () => Promise<void>;
}

/** Keeps the snapshot and the trust list current and runs commands against them. */
export function useSend(client: SendClient, options: UseSendOptions = {}): SendState {
  const { subscribe, pollMs = 2000 } = options;
  const [snapshot, setSnapshot] = useState<SendSnapshot | null>(null);
  const [trust, setTrust] = useState<TrustEntry[]>([]);
  const [defaultState, setDefaultState] = useState<TrustState>("ask");
  const [error, setError] = useState<unknown>(null);
  const alive = useRef(true);

  const reload = useCallback(async () => {
    try {
      const [next, list, fallback] = await Promise.all([
        client.snapshot(),
        client.trustList(),
        client.trustDefault(),
      ]);
      if (!alive.current) return;
      setSnapshot(next);
      setTrust(list);
      setDefaultState(fallback);
    } catch (failure) {
      if (alive.current) setError(failure);
    }
  }, [client]);

  useEffect(() => {
    alive.current = true;
    void reload();
    let stop: () => void;
    if (subscribe) {
      stop = subscribe(() => void reload());
    } else {
      const timer = setInterval(() => void reload(), pollMs);
      stop = () => clearInterval(timer);
    }
    return () => {
      alive.current = false;
      stop();
    };
  }, [reload, subscribe, pollMs]);

  const run = useCallback(
    async <T,>(action: (client: SendClient) => Promise<T>): Promise<T> => {
      try {
        const result = await action(client);
        if (alive.current) setError(null);
        await reload();
        return result;
      } catch (failure) {
        if (alive.current) setError(failure);
        throw failure;
      }
    },
    [client, reload],
  );

  return { snapshot, trust, defaultState, error, run, reload };
}
