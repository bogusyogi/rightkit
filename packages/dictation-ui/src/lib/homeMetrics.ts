// Home dashboard types + pure helpers. Extracted from HomePage so the hook,
// the page, and the stat/transcript components share one source.

export type AppState =
  | { state: "armed" }
  | { state: "recording"; session_id: string }
  | { state: "transcribing"; session_id: string }
  | { state: "pasting"; session_id: string; transcript: string }
  | { state: "pasted"; delivery_id: string; transcript: string }
  | {
      state: "paste_sent_unconfirmed";
      delivery_id: string;
      transcript: string;
      reason: string;
    }
  | {
      state: "copied_fallback";
      delivery_id: string;
      transcript: string;
      reason: string;
    }
  | { state: "error"; message: string; last_transcript?: string };

export interface HistoryItem {
  delivery_id: string;
  transcript: string;
  outcome: { kind: string; reason?: string };
  target?: { process_name?: string; window_title?: string; exe_path?: string };
  delivered_at_ms: number;
  /** Pre-AI-polish text, when the record has one (lets the UI toggle raw <-> polished). */
  raw_transcript?: string | null;
  /** Recorded audio length in ms - the clip duration. */
  recording_ms?: number | null;
  /** Origin of the record. Absent/null = dictation; file_transcription = a
   *  "Transcribe a file" result (labeled by file name in the UI).
   *  `device` carries cross-device sync provenance ("iphone" | "watch" | "mac"),
   *  absent for records that predate sync. The LOCAL device never sets it on
   *  its own records — only a synced-in record from another device does, so
   *  the History UI can badge it (Mac shows no badge for its own recordings). */
  source?: { kind: string; file_name?: string; device?: string | null } | null;
}

function wordsCount(items: HistoryItem[]): number {
  return items.reduce((n, it) => n + it.transcript.trim().split(/\s+/).filter(Boolean).length, 0);
}

const WEEK_MS = 7 * 24 * 60 * 60 * 1000;

/// Words from transcripts delivered within the last 7 days. The "Words this
/// week" stat must NOT count stale all-time history (a transcript from 13 days
/// ago is not "this week"). Filters on `delivered_at_ms` before summing.
export function wordsThisWeek(items: HistoryItem[], now: number = Date.now()): number {
  const since = now - WEEK_MS;
  return wordsCount(items.filter((it) => it.delivered_at_ms >= since));
}

export function timeSavedSeconds(words: number): number {
  // 40 wpm typing baseline → seconds saved vs typing.
  return Math.round((words / 40) * 60);
}

export function fmtSeconds(sec: number): string {
  if (sec < 60) return `${sec}s`;
  if (sec < 3600) return `${Math.round(sec / 60)} min`;
  const h = Math.floor(sec / 3600);
  const m = Math.round((sec - h * 3600) / 60);
  return `${h}h ${m}m`;
}
