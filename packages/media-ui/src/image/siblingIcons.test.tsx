import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { resolveIcon } from "../icons.js";

describe("image viewer sibling caret overrides", () => {
  it("uses the host's caret glyphs when given and the built-in otherwise", () => {
    const icons = { caretLeft: <i data-host="left" />, caretRight: <i data-host="right" /> };
    expect(renderToStaticMarkup(<>{resolveIcon("caretLeft", icons)}</>)).toContain('data-host="left"');
    expect(renderToStaticMarkup(<>{resolveIcon("caretRight", icons)}</>)).toContain('data-host="right"');
    expect(renderToStaticMarkup(<>{resolveIcon("caretLeft", {})}</>)).toContain("<svg");
    expect(resolveIcon("caretRight", { caretRight: null })).toBeNull();
  });
});
