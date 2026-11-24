// Launch-splash behaviour: the wait clock, the staged status line, stall detection with recovery actions,
// a clock that ticks. The window markup, copy and the commands behind the actions belong to the host.
import { useCallback, useEffect, useState } from "react";

/** A status line shown once the wait reaches `atSecs`. */
export interface SplashStage {
  atSecs: number;
  message: string;
}

/**
 * The message of the latest stage reached by `waitedSecs`; the first stage's message before any is reached.
 * `stages` is in ascending `atSecs` order and not empty.
 */
export function splashMessageForWait(waitedSecs: number, stages: readonly SplashStage[]): string {
  let message = stages[0]?.message ?? "";
  for (const stage of stages) {
    if (waitedSecs >= stage.atSecs) message = stage.message;
  }
  return message;
}

/** Seconds waited since mount (ticks every second) and a loader step that cycles `0..stepCount-1` every `stepMs`. */
export function useSplashClock(options: { stepCount: number; stepMs: number }): {
  waitedSecs: number;
  loaderStep: number;
} {
  const { stepCount, stepMs } = options;
  const [waitedSecs, setWaitedSecs] = useState(0);
  const [loaderStep, setLoaderStep] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => setWaitedSecs((s) => s + 1), 1000);
    return () => window.clearInterval(id);
  }, []);
  useEffect(() => {
    const id = window.setInterval(() => setLoaderStep((s) => (s + 1) % stepCount), stepMs);
    return () => window.clearInterval(id);
  }, [stepCount, stepMs]);
  return { waitedSecs, loaderStep };
}

/** What the host reports about startup: whether preparation failed and how long since it last progressed. */
export interface SplashStatus {
  failed: boolean;
  sinceProgressMs: number | null;
}

/** Default seconds without progress before the splash offers recovery. */
export const DEFAULT_SPLASH_STALL_SECS = 90;

export interface UseSplashStallOptions {
  /** Reads the host's startup status. */
  readStatus: () => Promise<SplashStatus>;
  waitedSecs: number;
  /** Default 90. */
  stallSecs?: number;
  /** Status poll interval. Default 2000. */
  pollMs?: number;
  /** Text of the note after an action failed. Default: `Failed: <error>`. */
  failedNote?: (error: unknown) => string;
}

/**
 * Stall detection: the splash is stalled once preparation is recorded failed, or once both the wait and the
 * time since progress reach `stallSecs`. `runAction` runs a recovery action (copy diagnostics, reveal a
 * folder) and leaves its outcome in `note`.
 */
export function useSplashStall(options: UseSplashStallOptions) {
  const { readStatus, waitedSecs, stallSecs = DEFAULT_SPLASH_STALL_SECS, pollMs = 2000, failedNote } = options;
  const [status, setStatus] = useState<SplashStatus>({ failed: false, sinceProgressMs: null });
  const [note, setNote] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    const poll = () =>
      void readStatus()
        .then((next) => live && setStatus(next))
        .catch(() => undefined);
    poll();
    const id = window.setInterval(poll, pollMs);
    return () => {
      live = false;
      window.clearInterval(id);
    };
  }, [readStatus, pollMs]);
  const idleSecs = status.sinceProgressMs == null ? waitedSecs : Math.floor(status.sinceProgressMs / 1000);
  const stalled = status.failed || (waitedSecs >= stallSecs && idleSecs >= stallSecs);
  const runAction = useCallback(
    (action: () => Promise<unknown>, doneNote: string) => () => {
      setNote(null);
      action().then(
        () => setNote(doneNote),
        (err) => setNote(failedNote ? failedNote(err) : `Failed: ${String(err)}`),
      );
    },
    [failedNote],
  );
  return { stalled, note, setNote, runAction };
}

/** A value read once on mount (the desktop wallpaper); null while loading or when the read fails. */
export function useSplashWallpaper(read: () => Promise<string | null>): string | null {
  const [value, setValue] = useState<string | null>(null);
  useEffect(() => {
    void read()
      .then((data) => setValue(data ?? null))
      .catch(() => undefined);
  }, [read]);
  return value;
}

/** A value that is re-read every `tickMs` (a clock face). */
export function useTickingValue<T>(read: () => T, tickMs = 1000): T {
  const [value, setValue] = useState<T>(read);
  useEffect(() => {
    const id = window.setInterval(() => setValue(read()), tickMs);
    return () => window.clearInterval(id);
  }, [read, tickMs]);
  return value;
}

/** Options of the clock formatters: the locale and, for the time part, the 12 or 24 hour clock. */
export interface ClockFormatOptions {
  /** BCP 47 locale. Default `en-US`. */
  locale?: string;
  /** 12 hour clock. Default true. */
  hour12?: boolean;
}

/**
 * Menu-bar clock text such as `Wed Jul 1  12:03 AM`: short weekday, short month and day, two spaces, then the time.
 * The date and the time are formatted separately and joined because one `toLocaleString` call inserts a locale
 * separator (" at ") a real menu bar never shows, and the commas of the weekday form are dropped.
 */
export function formatMenuBarClock(date: Date, options: ClockFormatOptions = {}): string {
  const { locale = "en-US", hour12 = true } = options;
  const day = date.toLocaleDateString(locale, { weekday: "short", month: "short", day: "numeric" }).replace(/,/g, "");
  const time = date.toLocaleTimeString(locale, { hour: "numeric", minute: "2-digit", hour12 });
  return `${day}  ${time}`;
}

/** Taskbar clock parts such as `{ time: "12:03 AM", date: "7/1/2026" }`, for a clock drawn on two lines. */
export function formatTaskbarClock(date: Date, options: ClockFormatOptions = {}): { time: string; date: string } {
  const { locale = "en-US", hour12 = true } = options;
  return {
    time: date.toLocaleTimeString(locale, { hour: "numeric", minute: "2-digit", hour12 }),
    date: date.toLocaleDateString(locale, { month: "numeric", day: "numeric", year: "numeric" }),
  };
}
