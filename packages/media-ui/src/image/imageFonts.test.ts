import { describe, expect, it } from "vitest";
import {
  DEFAULT_IMAGE_FONT_ID,
  cssFontFamilyForImageText,
  fontCssForImageText,
} from "./imageFonts.js";

const fonts = [
  { id: "default", name: "Default", stack: "'Base', serif" },
  { id: "brand", name: "Brand", stack: "'Brand', sans-serif" },
];
const INTER = "'Inter', system-ui, sans-serif";

describe("image font resolution", () => {
  it("empty family resolves through defaultFontId, then the package default", () => {
    expect(cssFontFamilyForImageText(fonts, "", { defaultFontId: "brand" })).toBe(fonts[1]!.stack);
    expect(cssFontFamilyForImageText(fonts, null)).toBe(fonts[0]!.stack);
    expect(DEFAULT_IMAGE_FONT_ID).toBe("default");
  });

  it("an explicit family wins over defaultFontId", () => {
    expect(cssFontFamilyForImageText(fonts, "default", { defaultFontId: "brand" })).toBe(
      fonts[0]!.stack,
    );
  });

  it("falls back to system-ui by default and to fallbackStack when given", () => {
    expect(cssFontFamilyForImageText([], "")).toBe("system-ui, sans-serif");
    expect(cssFontFamilyForImageText([], "Helvetica")).toBe('"Helvetica", system-ui, sans-serif');
    expect(cssFontFamilyForImageText([], "", { fallbackStack: INTER })).toBe(INTER);
    expect(cssFontFamilyForImageText([], "Helvetica", { fallbackStack: INTER })).toBe(
      `"Helvetica", ${INTER}`,
    );
  });

  it("fontCssForImageText threads the options (export/print share the same stack)", () => {
    expect(fontCssForImageText([], "", 20, true, { fallbackStack: INTER })).toBe(
      `700 20px ${INTER}`,
    );
    expect(fontCssForImageText(fonts, "", 12, false, { defaultFontId: "brand" })).toBe(
      "12px 'Brand', sans-serif",
    );
    expect(fontCssForImageText([], "", 3, false)).toBe("8px system-ui, sans-serif");
  });
});
