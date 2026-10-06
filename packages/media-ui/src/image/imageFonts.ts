import type { MediaFontOption } from "../adapters.js";

export const DEFAULT_IMAGE_FONT_ID = "default";

const FALLBACK_STACK = "system-ui, sans-serif";

export function isBundledImageFontId(
  fonts: MediaFontOption[],
  fontFamily: string | null | undefined,
): boolean {
  return fonts.some((font) => font.id === fontFamily);
}

export function cssFontFamilyForImageText(
  fonts: MediaFontOption[],
  fontFamily: string | null | undefined,
): string {
  const bundled = fonts.find((font) => font.id === (fontFamily || DEFAULT_IMAGE_FONT_ID));
  if (bundled) return bundled.stack;
  const system = sanitizeFontFamily(fontFamily || "");
  return system ? `${JSON.stringify(system)}, ${FALLBACK_STACK}` : FALLBACK_STACK;
}

export function fontCssForImageText(
  fonts: MediaFontOption[],
  fontFamily: string | null | undefined,
  size: number,
  bold: boolean,
): string {
  const px = Math.max(8, Math.round(size || 8));
  return `${bold ? "700 " : ""}${px}px ${cssFontFamilyForImageText(fonts, fontFamily)}`;
}

function sanitizeFontFamily(fontFamily: string): string {
  return fontFamily
    .replace(/[\r\n]/g, " ")
    .replace(/["\\]/g, "")
    .trim();
}
