import type { LocalAsrPhase, LocalBackend } from "../client.js";

// The languages the local Parakeet model covers, as the model card lists them.
export const TDT_LANGUAGES = [
  "English",
  "Spanish",
  "French",
  "German",
  "Italian",
  "Portuguese",
  "Dutch",
  "Russian",
  "Ukrainian",
  "Polish",
  "Czech",
  "Slovak",
  "Slovenian",
  "Croatian",
  "Bulgarian",
  "Romanian",
  "Hungarian",
  "Greek",
  "Finnish",
  "Swedish",
  "Danish",
  "Estonian",
  "Latvian",
  "Lithuanian",
  "Maltese",
];

// Whisper's 99 languages, in the engine's order (the engine's
// WHISPER_LANGUAGES, minus "yue", which is Cantonese and not part
// of the standard 99). The lock passes the code straight through to Whisper.
const WHISPER_CODES = [
  "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl",
  "ar", "sv", "it", "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro",
  "da", "hu", "ta", "no", "th", "ur", "hr", "bg", "lt", "la", "mi", "ml", "cy",
  "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn", "et", "mk", "br", "eu",
  "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si", "km",
  "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo",
  "uz", "fo", "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg",
  "as", "tt", "haw", "ln", "ha", "ba", "jw", "su",
] as const;

// Whisper uses "jw" for Javanese; Intl only knows the ISO code "jv".
const NAME_OVERRIDES: Record<string, string> = { jw: "Javanese" };

function languageName(code: string): string {
  if (NAME_OVERRIDES[code]) return NAME_OVERRIDES[code];
  try {
    return new Intl.DisplayNames(["en"], { type: "language" }).of(code) ?? code;
  } catch {
    return code;
  }
}

export const WHISPER_LANGUAGE_COUNT = WHISPER_CODES.length;

export const PICK_LANGUAGES: Array<{ code: string; name: string }> = WHISPER_CODES.map(
  (code) => ({ code, name: languageName(code) }),
).sort((a, b) => a.name.localeCompare(b.name));

export type DlState = { active: boolean; phase: LocalAsrPhase; pct: number; error: string };
export type WhisperDlState = { active: boolean; phase: string; pct: number; error: string };

export const BOX_BASE =
  "relative flex w-full flex-col overflow-hidden rounded-[var(--radius-md)] border px-[var(--space-3)] py-[var(--space-2)] text-left transition-[background-color,border-color] duration-150 ease-[var(--ease-out-strong)]";

export const MODELS: Array<{
  backend: LocalBackend;
  title: string;
  summary: string;
  detail: string;
  languages: string[];
}> = [
  // Unified is retired — TDT beats it on the canonical
  // productivity-dictation corpus (6.85% vs 7.02% WER) at 2.5x the speed and
  // 100 MB less disk, so the separate English-only lane earns nothing.
  // Settings offers exactly two lanes: TDT (free) and Whisper (Pro).
  {
    backend: "parakeet-tdt",
    title: "25 languages",
    summary: "Powered by Parakeet Ultra",
    detail:
      "Fast, accurate dictation in English and 24 more languages. On-device, licensed CC BY 4.0.",
    languages: TDT_LANGUAGES,
  },
];
