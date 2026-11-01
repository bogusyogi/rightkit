import { describe, expect, it } from "vitest";
import { bytesToBase64 } from "./base64.js";
import { rightDrawerWidth, sourceWidthPercent } from "./splitResizeMath.js";

describe("document-core helpers", () => {
  it("encodes bytes across chunk boundaries", () => {
    const bytes = new TextEncoder().encode("hello world");
    expect(bytesToBase64(bytes, 3)).toBe(btoa("hello world"));
  });
  it("clamps split widths", () => {
    const bounds = { left: 0, right: 1200, width: 1200 };
    expect(sourceWidthPercent(bounds, 1100)).toBe(25);
    expect(rightDrawerWidth(bounds, 1190)).toBe(280);
  });
});
