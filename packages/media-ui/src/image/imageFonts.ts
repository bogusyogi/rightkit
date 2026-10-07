import type { MediaFontOption } from "../adapters.js";

export const DEFAULT_IMAGE_FONT_ID = "default";

const FALLBACK_STACK = "system-ui, sans-serif";

export function isBundledImageFontId(
  fonts: MediaFontOption[],
  fontFamily: string | null | undefined,
): boolean {
  return fonts.some((font) => font.id === fontFamily);
}

export interface ImageFontOptions {
  /** Bundled font id used when a layer has no font family. Default "default". */
  defaultFontId?: string;
  /** Final CSS fallback stack. Default "system-ui, sans-serif". */
  fallbackStack?: string;
}

export function cssFontFamilyForImageText(
  fonts: MediaFontOption[],
  fontFamily: string | null | undefined,
  options: ImageFontOptions = {},
): string {
  const fallback = options.fallbackStack ?? FALLBACK_STACK;
  const bundled = fonts.find(
    (font) => font.id === (fontFamily || options.defaultFontId || DEFAULT_IMAGE_FONT_ID),
  );
  if (bundled) return bundled.stack;
  const system = sanitizeFontFamily(fontFamily || "");
  return system ? `${JSON.stringify(system)}, ${fallback}` : fallback;
}

export function fontCssForImageText(
  fonts: MediaFontOption[],
  fontFamily: string | null | undefined,
  size: number,
  bold: boolean,
  options: ImageFontOptions = {},
): string {
  const px = Math.max(8, Math.round(size || 8));
  return `${bold ? "700 " : ""}${px}px ${cssFontFamilyForImageText(fonts, fontFamily, options)}`;
}

function sanitizeFontFamily(fontFamily: string): string {
  return fontFamily
    .replace(/[\r\n]/g, " ")
    .replace(/["\\]/g, "")
    .trim();
}
