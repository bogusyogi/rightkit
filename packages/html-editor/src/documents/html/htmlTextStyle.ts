export type HtmlTextStyleProperty = "fontFamily" | "fontSize" | "color" | "backgroundColor";

export type HtmlTextStyleAttrs = Record<HtmlTextStyleProperty, string | null>;

export const HTML_RICH_FONTS = [
  "Arial",
  "Helvetica",
  "Times New Roman",
  "Georgia",
  "Garamond",
  "Verdana",
  "Tahoma",
  "Trebuchet MS",
  "Courier New",
  "Consolas",
] as const;

export const HTML_RICH_SIZES = [8, 9, 10, 11, 12, 14, 16, 18, 20, 24, 28, 36, 48, 72] as const;

export const HTML_TEXT_COLORS = [
  { name: "Black", hex: "000000" },
  { name: "Dk Gray", hex: "434343" },
  { name: "Gray", hex: "666666" },
  { name: "Lt Gray", hex: "999999" },
  { name: "White", hex: "FFFFFF" },
  { name: "Red", hex: "E5484D" },
  { name: "Orange", hex: "E8833A" },
  { name: "Yellow", hex: "E5A300" },
  { name: "Green", hex: "2E9C68" },
  { name: "Teal", hex: "1F9C9C" },
  { name: "Blue", hex: "2F6FEB" },
  { name: "Indigo", hex: "5B53D6" },
  { name: "Purple", hex: "8B5CF6" },
  { name: "Magenta", hex: "C2398A" },
  { name: "Copper", hex: "C46A3D" },
  { name: "Sienna", hex: "A5552B" },
] as const;

export const HTML_HIGHLIGHT_COLORS = [
  { name: "Yellow", hex: "FFFF00" },
  { name: "Green", hex: "00FF00" },
  { name: "Cyan", hex: "00FFFF" },
  { name: "Magenta", hex: "FF00FF" },
  { name: "Blue", hex: "00B0F0" },
  { name: "Red", hex: "FF0000" },
  { name: "Gray", hex: "BFBFBF" },
  { name: "Lime", hex: "C4F000" },
] as const;

const CSS_TO_PROPERTY: Record<string, HtmlTextStyleProperty> = {
  "font-family": "fontFamily",
  "font-size": "fontSize",
  color: "color",
  "background-color": "backgroundColor",
};

const PROPERTY_TO_CSS: Record<HtmlTextStyleProperty, string> = {
  fontFamily: "font-family",
  fontSize: "font-size",
  color: "color",
  backgroundColor: "background-color",
};

export const EMPTY_HTML_TEXT_STYLE: HtmlTextStyleAttrs = {
  fontFamily: null,
  fontSize: null,
  color: null,
  backgroundColor: null,
};

export function safeHtmlTextStyleValue(
  property: HtmlTextStyleProperty,
  value: string | null | undefined,
): string | null {
  if (!value) return null;
  const trimmed = value.trim();
  if (!trimmed || trimmed.length > 120 || /[;{}]|url\s*\(|expression\s*\(/i.test(trimmed))
    return null;
  if (property === "fontSize")
    return /^(?:\d+(?:\.\d+)?)(?:px|pt|em|rem|%)$/i.test(trimmed) ? trimmed : null;
  if (property === "color" || property === "backgroundColor")
    return /^(?:#[0-9a-f]{3,8}|rgba?\([\d\s.,%]+\)|hsla?\([\d\s.,%]+\)|[a-z]+)$/i.test(trimmed)
      ? trimmed
      : null;
  return /^[\w\s,'"-]+$/.test(trimmed) ? trimmed : null;
}

export function parseHtmlTextStyle(style: string | null | undefined): HtmlTextStyleAttrs {
  const result = { ...EMPTY_HTML_TEXT_STYLE };
  for (const declaration of (style ?? "").split(";")) {
    const colon = declaration.indexOf(":");
    if (colon < 0) continue;
    const property = CSS_TO_PROPERTY[declaration.slice(0, colon).trim().toLowerCase()];
    if (!property) continue;
    result[property] = safeHtmlTextStyleValue(property, declaration.slice(colon + 1));
  }
  return result;
}

export function htmlTextStyleCss(attrs: Partial<HtmlTextStyleAttrs>): string {
  return (Object.keys(PROPERTY_TO_CSS) as HtmlTextStyleProperty[])
    .flatMap((property) => {
      const value = safeHtmlTextStyleValue(property, attrs[property]);
      return value ? [`${PROPERTY_TO_CSS[property]}:${value}`] : [];
    })
    .join(";");
}

export function hasHtmlTextStyle(attrs: Partial<HtmlTextStyleAttrs>): boolean {
  return (Object.keys(PROPERTY_TO_CSS) as HtmlTextStyleProperty[]).some(
    (property) => !!attrs[property],
  );
}
