import { describe, expect, it } from "vitest";
import { chatKindName, CHAT_KIND_ORDER } from "./chatKind.js";
import { qrMatrix, qrSvgPath } from "./qr.js";

describe("system-ui pure helpers", () => {
  it("builds a square QR matrix and an SVG path for it", () => {
    const m = qrMatrix("https://example.com/pair?code=1234");
    expect(m.length).toBeGreaterThanOrEqual(21);
    for (const row of m) expect(row.length).toBe(m.length);
    const { path, size } = qrSvgPath(m);
    expect(size).toBeGreaterThanOrEqual(m.length);
    expect(path.length).toBeGreaterThan(0);
  });

  it("names chat kinds", () => {
    expect(CHAT_KIND_ORDER[0]).toBe("claude");
    expect(chatKindName("codex")).toBe("Codex");
  });
});
