// useHomeDashboard - owns all Home state and side effects: app state, recent
// history, wake-listen toggle, the transient wake pulse, and error. Uses
// useTauriEvent for the per-mount listeners (consistent safe cleanup), a
// module-level shared store for recent history (see below), and a ref'd
// timer for the wake pulse so it's cleared before re-scheduling and on unmount.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke, listen } from "../lib/tauri";
import { useTauriEvent } from "./useTauriEvent";
import {
  fmtSeconds,
  timeSavedSeconds,
  wordsThisWeek as wordsThisWeekCalc,
  type AppState,
  type HistoryItem,
} from "../lib/homeMetrics";

type CommandName = "start_dictation" | "stop_dictation" | "cancel_dictation";

// Shared recent-history store. Home and TitleBar each mount this hook, so the
// `delivery-recorded` subscription and the `get_recent_history` refetch it
// triggers live once at module scope and fan out to every mounted consumer,
// instead of each mount subscribing and invoking independently and doubling
// the IPC per delivery. `fetchRecentHistory` also dedupes concurrent callers
// (the two mounts' initial refresh, or a delivery event racing a refresh)
// behind one in-flight invoke.
const historySubscribers = new Set<(items: HistoryItem[]) => void>();
let sharedHistory: HistoryItem[] = [];
let historyListenerStarted = false;
let inFlightHistory: Promise<HistoryItem[]> | null = null;

function publishHistory(items: HistoryItem[]): void {
  sharedHistory = items;
  for (const subscriber of historySubscribers) subscriber(items);
}

function fetchRecentHistory(): Promise<HistoryItem[]> {
  inFlightHistory ??= invoke<HistoryItem[]>("get_recent_history")
    .then((items) => {
      publishHistory(items);
      return items;
    })
    .finally(() => {
      inFlightHistory = null;
    });
  return inFlightHistory;
}

function ensureHistoryListener(): void {
  if (historyListenerStarted) return;
  historyListenerStarted = true;
  void listen<unknown>("delivery-recorded", () => {
    void fetchRecentHistory().catch(() => undefined);
  }).catch(() => {
    // Browser-preview mode has no Tauri IPC; allow a retry on the next mount.
    historyListenerStarted = false;
  });
}

export interface UseHomeDashboard {
  state: AppState;
  history: HistoryItem[];
  wakePulse: boolean;
  error: string;
  wordsThisWeek: number;
  timeSaved: string;
  transcribingSeconds: number;
  runCommand: (name: CommandName) => Promise<void>;
  refreshHistory: () => Promise<void>;
  clearError: () => void;
}

export function useHomeDashboard(readySurface: "home" | "titlebar"): UseHomeDashboard {
  const [state, setState] = useState<AppState>({ state: "armed" });
  const [history, setHistory] = useState<HistoryItem[]>(() => sharedHistory);
  const [wakePulse, setWakePulse] = useState(false);
  const [error, setError] = useState("");
  const [transcribingSeconds, setTranscribingSeconds] = useState(0);
  const wakePulseTimer = useRef<number | undefined>(undefined);
  const transcribingTimer = useRef<number | undefined>(undefined);
  const transcribingStart = useRef<number | undefined>(undefined);

  const refresh = useCallback(async () => {
    const [nextState, recent] = await Promise.allSettled([
      invoke<AppState>("get_app_state"),
      fetchRecentHistory(),
    ] as const);

    if (nextState.status === "fulfilled") {
      setState(nextState.value);
    }
    // A fulfilled `recent` already published into the shared history store,
    // which updates this hook's `history` via the subscriber below.

    if (nextState.status === "fulfilled" || recent.status === "fulfilled") {
      setError("");
      return;
    }

    setError(String(nextState.reason ?? recent.reason));
  }, []);

  const refreshHistory = useCallback(async () => {
    await fetchRecentHistory();
  }, []);

  // Subscribe to the shared history store + start its single
  // `delivery-recorded` listener on first mount.
  useEffect(() => {
    ensureHistoryListener();
    const subscriber = (items: HistoryItem[]) => setHistory(items);
    historySubscribers.add(subscriber);
    return () => {
      historySubscribers.delete(subscriber);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    void refresh().finally(() => {
      if (!cancelled) {
        void invoke("hub_frontend_ready", { surface: readySurface }).catch(() => undefined);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [readySurface, refresh]);

  useTauriEvent<AppState>("app-state-changed", (payload) => {
    setState(payload);

    // Track transcribing duration for slow-transcribe feedback.
    if (payload.state === "transcribing") {
      if (!transcribingStart.current) {
        transcribingStart.current = Date.now();
        transcribingTimer.current = window.setInterval(() => {
          const elapsed = Math.floor(
            (Date.now() - (transcribingStart.current ?? Date.now())) / 1000,
          );
          setTranscribingSeconds(elapsed);
        }, 1000);
      }
    } else {
      if (transcribingTimer.current !== undefined) {
        window.clearInterval(transcribingTimer.current);
        transcribingTimer.current = undefined;
      }
      transcribingStart.current = undefined;
      setTranscribingSeconds(0);
    }
  });

  // NOTE: the backend also emits a 10Hz "recording-level" event for the live
  // mic meter. Home/TitleBar never read a level from this hook (the pill's
  // own PillOverlay listens to pill://event directly for its rim animation),
  // so this hook deliberately does NOT subscribe to it — doing so would
  // re-render every useHomeDashboard() consumer (Home.tsx AND TitleBar.tsx,
  // which each mount their own instance) 10x/sec during every dictation for
  // state nothing here displays. Add a subscription here only alongside an
  // actual consumer of the level.

  useTauriEvent("wake-fired", () => {
    if (wakePulseTimer.current !== undefined) {
      window.clearTimeout(wakePulseTimer.current);
    }
    setWakePulse(true);
    wakePulseTimer.current = window.setTimeout(() => {
      wakePulseTimer.current = undefined;
      setWakePulse(false);
    }, 600);
  });

  // Clear any pending wake-pulse timer on unmount.
  useEffect(
    () => () => {
      if (wakePulseTimer.current !== undefined) {
        window.clearTimeout(wakePulseTimer.current);
      }
      if (transcribingTimer.current !== undefined) {
        window.clearInterval(transcribingTimer.current);
      }
    },
    [],
  );

  const wordsThisWeek = useMemo(() => wordsThisWeekCalc(history), [history]);
  const timeSaved = useMemo(() => fmtSeconds(timeSavedSeconds(wordsThisWeek)), [wordsThisWeek]);

  const runCommand = useCallback(
    async (name: CommandName) => {
      setError("");
      try {
        await invoke(name);
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [refresh],
  );

  const clearError = useCallback(() => setError(""), []);

  return {
    state,
    history,
    wakePulse,
    error,
    wordsThisWeek,
    timeSaved,
    transcribingSeconds,
    runCommand,
    refreshHistory,
    clearError,
  };
}
