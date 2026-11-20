// The home-page state over a shared recent-rows store: the rows, this week's word count and time saved, the
// load error, a refresh, and a one-shot "ready" notification the host uses to know the surface has loaded
// (success or failure). The store, the figure reader and the ready call are inputs. Imports only `react`.
// Moved from HeardRight `hooks/useHomeDashboard.ts`.
import { useCallback, useEffect, useMemo, useState } from "react";
import { formatSeconds, timeSavedSeconds, wordsInWindow } from "./metrics.js";
import type { SharedRecent } from "./recent.js";

export interface UseRecentDashboardOptions<T> {
  /** Reads the text and delivery time of a row for the figures. */
  read(item: T): { text: string; atMs: number };
  /** Called once per `readyKey` after the first load settles, whether it succeeded or failed. Failures are ignored. */
  onReady?(readyKey: string): Promise<unknown>;
  /** Which surface is ready (HeardRight: `home` or `titlebar`); a change reloads and notifies again. */
  readyKey: string;
}

export interface RecentDashboard<T> {
  history: T[];
  error: string;
  wordsThisWeek: number;
  timeSaved: string;
  refreshHistory(): Promise<void>;
  clearError(): void;
}

export function useRecentDashboard<T>(store: SharedRecent<T>, options: UseRecentDashboardOptions<T>): RecentDashboard<T> {
  const { read, onReady, readyKey } = options;
  const history = store.use();
  const [error, setError] = useState("");

  const refresh = useCallback(async () => {
    try {
      // The fetch publishes into the shared store, which updates `history` for every mounted consumer.
      await store.fetch();
      setError("");
    } catch (e) {
      setError(String(e));
    }
  }, [store]);

  const refreshHistory = useCallback(async () => {
    await store.fetch();
  }, [store]);

  useEffect(() => {
    let cancelled = false;
    void refresh().finally(() => {
      if (!cancelled) {
        void Promise.resolve(onReady?.(readyKey)).catch(() => undefined);
      }
    });
    return () => {
      cancelled = true;
    };
    // biome-ignore lint/correctness/useExhaustiveDependencies: onReady is read at call time; only the surface and store reload
  }, [readyKey, refresh]);

  const wordsThisWeek = useMemo(() => wordsInWindow(history, read), [history, read]);
  const timeSaved = useMemo(() => formatSeconds(timeSavedSeconds(wordsThisWeek)), [wordsThisWeek]);
  const clearError = useCallback(() => setError(""), []);

  return { history, error, wordsThisWeek, timeSaved, refreshHistory, clearError };
}
