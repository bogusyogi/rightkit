// Internal: not re-exported from the package root.

const GENERIC_FONT_FAMILY = /^(?:serif|sans-serif|monospace|cursive|fantasy|system-ui|ui-serif|ui-sans-serif|ui-monospace|ui-rounded)$/;

/**
 * True when `value` reads as a CSS font-family list, not a product name: two or more comma-separated entries, one of
 * them a CSS generic family keyword (`sans-serif`, `system-ui`, ...) written in lower case. A single word or a name
 * with commas but no generic family is never matched.
 */
export function isFontFamilyList(value: string): boolean {
  const entries = value.split(",").map((entry) => entry.trim());
  return entries.length > 1 && entries.some((entry) => GENERIC_FONT_FAMILY.test(entry));
}

/** The message for a `wordmark` that holds a font-family list (see `isFontFamilyList`). */
export const WORDMARK_FONT_MESSAGE = "wordmark: expected the product name, got a font-family list; the font belongs in wordmarkFont";
