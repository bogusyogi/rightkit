import { afterEach, describe, expect, it, vi } from "vitest";

const osMock = vi.hoisted(() => ({ platform: vi.fn<() => string>() }));
vi.mock("@tauri-apps/plugin-os", () => osMock);

describe("native platform loading", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.resetModules();
    osMock.platform.mockReset();
  });

  it("resolves the native platform from plugin-os on demand", async () => {
    vi.stubGlobal("navigator", { platform: "Win32", userAgent: "Mozilla/5.0 (Windows NT 10.0)" });
    osMock.platform.mockReturnValue("macos");
    const { loadPlatform, getPlatform } = await import("./index.js");

    // Synchronous read before the native load: the browser report.
    expect(getPlatform()).toBe("windows");
    await expect(loadPlatform()).resolves.toBe("macos");
    expect(getPlatform()).toBe("macos");
    expect(osMock.platform).toHaveBeenCalledOnce();
  });

  it("falls back to the browser report when plugin-os throws (outside Tauri)", async () => {
    vi.stubGlobal("navigator", { platform: "MacIntel", userAgent: "Mozilla/5.0 (Macintosh)" });
    osMock.platform.mockImplementation(() => {
      throw new Error("not in Tauri");
    });
    const { loadPlatform } = await import("./index.js");
    await expect(loadPlatform()).resolves.toBe("macos");
    expect(osMock.platform).toHaveBeenCalledOnce();
  });

  it("normalises an unsupported native name to unknown", async () => {
    osMock.platform.mockReturnValue("android");
    const { loadPlatform } = await import("./index.js");
    await expect(loadPlatform()).resolves.toBe("unknown");
  });
});
