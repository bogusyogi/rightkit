// Dashboard figures over recent rows: words in a window, the time they saved against typing, and a short
// duration text. Pure; the item shape comes in through `read`.

export const WEEK_MS = 7 * 24 * 60 * 60 * 1000;

/** Whitespace-separated words in `text`. */
export function countWords(text: string): number {
  return text.trim().split(/\s+/).filter(Boolean).length;
}

/**
 * Words in the items whose time falls within the last `windowMs` (default 7 days). Stale history does not
 * count: a transcript from 13 days ago is not "this week".
 */
export function wordsInWindow<T>(
  items: readonly T[],
  read: (item: T) => { text: string; atMs: number },
  windowMs: number = WEEK_MS,
  now: number = Date.now(),
): number {
  const since = now - windowMs;
  let words = 0;
  for (const item of items) {
    const { text, atMs } = read(item);
    if (atMs >= since) words += countWords(text);
  }
  return words;
}

/** Seconds saved against typing `words` at `typingWpm` (default 40 words per minute). */
export function timeSavedSeconds(words: number, typingWpm = 40): number {
  return Math.round((words / typingWpm) * 60);
}

/** "42s", "17 min" or "2h 5m". */
export function formatSeconds(sec: number): string {
  if (sec < 60) return `${sec}s`;
  if (sec < 3600) return `${Math.round(sec / 60)} min`;
  const h = Math.floor(sec / 3600);
  const m = Math.round((sec - h * 3600) / 60);
  return `${h}h ${m}m`;
}
