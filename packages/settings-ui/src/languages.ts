// Whisper language codes and display names for a language picker. Pure; imports nothing.

/**
 * Every fixed-language code the pinned whisper.cpp build accepts, in its language-id order (100 codes, `yue` last). The
 * same table as `rightkit-asr`'s `WHISPER_LANGUAGES`; a host that offers fewer passes `exclude` to `languageOptions`.
 */
export const WHISPER_LANGUAGE_CODES: readonly string[] = [
  "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it", "id", "hi",
  "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur", "hr", "bg", "lt", "la",
  "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn", "et", "mk", "br", "eu", "is", "hy",
  "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si", "km", "sn", "yo", "so", "af", "oc", "ka", "be",
  "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo", "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl",
  "mg", "as", "tt", "haw", "ln", "ha", "ba", "jw", "su", "yue",
];

/** Whisper's code for Javanese is `jw`; `Intl` only knows the ISO `jv`. */
export const DEFAULT_LANGUAGE_NAME_OVERRIDES: Readonly<Record<string, string>> = { jw: "Javanese" };

/**
 * The language's name in `locale` (default `"en"`) from `Intl.DisplayNames`; an `overrides` entry wins, and the code
 * itself comes back when `Intl` does not know it or throws. `overrides` defaults to `DEFAULT_LANGUAGE_NAME_OVERRIDES`.
 */
export function languageDisplayName(
  code: string,
  options: { locale?: string; overrides?: Readonly<Record<string, string>> } = {},
): string {
  const { locale = "en", overrides = DEFAULT_LANGUAGE_NAME_OVERRIDES } = options;
  if (overrides[code]) return overrides[code];
  try {
    return new Intl.DisplayNames([locale], { type: "language" }).of(code) ?? code;
  } catch {
    return code;
  }
}

/**
 * `{ code, name }` per code, sorted by name with `localeCompare`. `codes` defaults to `WHISPER_LANGUAGE_CODES`; `exclude`
 * drops codes the host does not offer. Always a new array.
 */
export function languageOptions(
  options: { codes?: readonly string[]; exclude?: readonly string[]; locale?: string; overrides?: Readonly<Record<string, string>> } = {},
): Array<{ code: string; name: string }> {
  const { codes = WHISPER_LANGUAGE_CODES, exclude = [], locale, overrides } = options;
  return codes
    .filter((code) => !exclude.includes(code))
    .map((code) => ({ code, name: languageDisplayName(code, { locale, overrides }) }))
    .sort((a, b) => a.name.localeCompare(b.name));
}
