import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/plugin-os", () => ({ platform: () => { throw new Error("not in Tauri"); } }));

describe("getPlatform outside Tauri", () => {
  afterEach(() => { vi.unstubAllGlobals(); vi.resetModules(); });

  it("falls back to the browser's platform so chords render natively in dev", async () => {
    vi.stubGlobal("navigator", { platform: "MacIntel", userAgent: "Mozilla/5.0 (Macintosh)" });
    const { getPlatform } = await import("./index.js");
    expect(getPlatform()).toBe("macos");
  });

  it("detects Windows browsers", async () => {
    vi.stubGlobal("navigator", { platform: "Win32", userAgent: "Mozilla/5.0 (Windows NT 10.0)" });
    const { getPlatform } = await import("./index.js");
    expect(getPlatform()).toBe("windows");
  });
});
