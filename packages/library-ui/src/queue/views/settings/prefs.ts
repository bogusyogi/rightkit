import type { AudioQuality, VideoQuality } from "../../contract";
import type { MediaMode } from "../../lib/mediaSelection";

/**
 * Per-device UI preferences that do not need the Rust engine.
 *
 * Paths and engine behaviour live in the Rust `Settings` (via `get_settings` /
 * `set_settings`). These preferences only seed WebView state (intake defaults,
 * transcript reader defaults), so they live in localStorage and apply on next read.
 */
export interface Prefs {
  /** What a pasted link does unless the user picks otherwise. */
  defaultOutput: MediaMode;
  /** Start new transcripts with timestamps visible. */
  showTimestamps: boolean;
  /** Copy keeps timestamps when they are visible. */
  copyTimestamps: boolean;
  defaultVideoQuality: VideoQuality;
  defaultAudioQuality: AudioQuality;
}

export const DEFAULT_PREFS: Prefs = {
  defaultOutput: "transcribe",
  showTimestamps: true,
  copyTimestamps: false,
  defaultVideoQuality: "p1080",
  defaultAudioQuality: "best",
};

export const PREFS_KEY = "scraperight.prefs.v1";

const OUTPUTS: ReadonlyArray<MediaMode> = ["transcribe", "audio", "video", "images"];
const VIDEO: ReadonlyArray<VideoQuality> = ["p720", "p1080", "p2160", "source"];
const AUDIO: ReadonlyArray<AudioQuality> = ["best", "compact"];

export function loadPrefs(): Prefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (!raw) return { ...DEFAULT_PREFS };
    const p = JSON.parse(raw) as Partial<Prefs>;
    return {
      defaultOutput: OUTPUTS.includes(p.defaultOutput as MediaMode)
        ? (p.defaultOutput as MediaMode)
        : DEFAULT_PREFS.defaultOutput,
      showTimestamps:
        typeof p.showTimestamps === "boolean" ? p.showTimestamps : DEFAULT_PREFS.showTimestamps,
      copyTimestamps:
        typeof p.copyTimestamps === "boolean" ? p.copyTimestamps : DEFAULT_PREFS.copyTimestamps,
      defaultVideoQuality: VIDEO.includes(p.defaultVideoQuality as VideoQuality)
        ? (p.defaultVideoQuality as VideoQuality)
        : DEFAULT_PREFS.defaultVideoQuality,
      defaultAudioQuality: AUDIO.includes(p.defaultAudioQuality as AudioQuality)
        ? (p.defaultAudioQuality as AudioQuality)
        : DEFAULT_PREFS.defaultAudioQuality,
    };
  } catch {
    return { ...DEFAULT_PREFS };
  }
}

export function savePrefs(prefs: Prefs): void {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
  } catch {
    /* storage unavailable: preference applies for this session only */
  }
}

/** Speech-model idle unload choices, in minutes (0 never unloads). */
export const MODEL_IDLE_MINUTES_CHOICES: ReadonlyArray<number> = [0, 1, 5, 15, 60];

export const DEFAULT_MODEL_IDLE_MINUTES = 1;

/** The saved value, or the default when it is missing or not a whole number of minutes. */
export function normalizeModelIdleMinutes(value: unknown): number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0
    ? value
    : DEFAULT_MODEL_IDLE_MINUTES;
}

export function modelIdleLabel(minutes: number): string {
  if (minutes === 0) return "Never unload";
  return minutes === 1 ? "After 1 minute" : `After ${minutes} minutes`;
}

/** Select options; a saved custom value stays selectable so it is not silently rewritten. */
export function modelIdleOptions(current: number): Array<{ value: string; label: string }> {
  const minutes = MODEL_IDLE_MINUTES_CHOICES.includes(current)
    ? [...MODEL_IDLE_MINUTES_CHOICES]
    : [...MODEL_IDLE_MINUTES_CHOICES, current].sort((a, b) => a - b);
  return minutes.map((m) => ({ value: String(m), label: modelIdleLabel(m) }));
}
