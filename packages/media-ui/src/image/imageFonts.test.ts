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
      '"Helvetica", system-ui, sans-serif',
    );
  });

  it("matches ViewRight's cssFontFamilyForImageText exactly with the Inter stack", () => {
    const vr = [{ id: "default", name: "Inter", stack: INTER }];
    const o = { fallbackStack: INTER };
    expect(cssFontFamilyForImageText(vr, "", o)).toBe("'Inter', system-ui, sans-serif");
    expect(cssFontFamilyForImageText(vr, null, o)).toBe("'Inter', system-ui, sans-serif");
    expect(cssFontFamilyForImageText([], "", o)).toBe("'Inter', system-ui, sans-serif");
    expect(cssFontFamilyForImageText([], "  ", o)).toBe("'Inter', system-ui, sans-serif");
    expect(cssFontFamilyForImageText(vr, "Helvetica", o)).toBe('"Helvetica", system-ui, sans-serif');
    expect(cssFontFamilyForImageText(vr, 'Bad"Fam\\i\nly', o)).toBe(
      '"BadFami ly", system-ui, sans-serif',
    );
    expect(cssFontFamilyForImageText(vr, "default", o)).toBe(INTER);
  });

  it("systemFallbackStack applies only after a named system font", () => {
    const o = { fallbackStack: INTER, systemFallbackStack: "serif" };
    expect(cssFontFamilyForImageText([], "Helvetica", o)).toBe('"Helvetica", serif');
    expect(cssFontFamilyForImageText([], "", o)).toBe(INTER);
    expect(cssFontFamilyForImageText([], "Helvetica", { fallbackStack: INTER })).toBe(
      '"Helvetica", system-ui, sans-serif',
    );
    expect(fontCssForImageText([], "Helvetica", 20, false, o)).toBe('20px "Helvetica", serif');
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
