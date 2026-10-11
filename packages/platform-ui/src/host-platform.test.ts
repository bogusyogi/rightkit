import { afterEach, describe, expect, it, vi } from "vitest";

const osMock = vi.hoisted(() => ({ platform: vi.fn<() => string>() }));
vi.mock("@tauri-apps/plugin-os", () => osMock);

describe("host-injected platform, QA options and fallback", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.resetModules();
    vi.restoreAllMocks();
    osMock.platform.mockReset();
    delete globalThis.__RIGHTKIT_PLATFORM__;
    delete globalThis.__RIGHTKIT_QA_PLATFORM__;
  });

  it("reads the host-injected platform synchronously before the native load", async () => {
    vi.stubGlobal("navigator", { platform: "MacIntel", userAgent: "Mozilla/5.0 (Macintosh)" });
    globalThis.__RIGHTKIT_PLATFORM__ = "win";
    osMock.platform.mockImplementation(() => {
      throw new Error("not in Tauri");
    });
    const { getPlatform, loadPlatform } = await import("./index.js");

    expect(getPlatform()).toBe("windows");
    await expect(loadPlatform()).resolves.toBe("windows");
    expect(getPlatform()).toBe("windows");
  });

  it("ignores an injected value that is not a string", async () => {
    vi.stubGlobal("navigator", { platform: "MacIntel", userAgent: "Mozilla/5.0 (Macintosh)" });
    (globalThis as { __RIGHTKIT_PLATFORM__?: unknown }).__RIGHTKIT_PLATFORM__ = 3;
    osMock.platform.mockImplementation(() => {
      throw new Error("not in Tauri");
    });
    const { getPlatform } = await import("./index.js");
    expect(getPlatform()).toBe("macos");
  });

  it("lets the async native value win over a disagreeing injected value, with a warning", async () => {
    vi.stubGlobal("navigator", { platform: "Win32", userAgent: "Mozilla/5.0 (Windows NT 10.0)" });
    globalThis.__RIGHTKIT_PLATFORM__ = "windows";
    osMock.platform.mockReturnValue("macos");
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const { getPlatform, loadPlatform } = await import("./index.js");

    expect(getPlatform()).toBe("windows");
    await expect(loadPlatform()).resolves.toBe("macos");
    expect(getPlatform()).toBe("macos");
    expect(warn).toHaveBeenCalledOnce();
  });

  it("does not warn when the injected and native values agree", async () => {
    globalThis.__RIGHTKIT_PLATFORM__ = "linux";
    osMock.platform.mockReturnValue("linux");
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const { loadPlatform } = await import("./index.js");
    await expect(loadPlatform()).resolves.toBe("linux");
    expect(warn).not.toHaveBeenCalled();
  });

  it("uses the injected value when plugin-os is absent", async () => {
    globalThis.__RIGHTKIT_PLATFORM__ = "linux";
    osMock.platform.mockImplementation(() => {
      throw new Error("not in Tauri");
    });
    const { loadPlatform } = await import("./index.js");
    await expect(loadPlatform()).resolves.toBe("linux");
  });

  it("builds the initialization script from the normalised platform only", async () => {
    const { platformInitializationScript } = await import("./index.js");
    expect(platformInitializationScript("macos")).toBe('window.__RIGHTKIT_PLATFORM__ = "macos";');
    expect(platformInitializationScript("mac")).toBe('window.__RIGHTKIT_PLATFORM__ = "macos";');
    expect(platformInitializationScript("Win32")).toBe('window.__RIGHTKIT_PLATFORM__ = "windows";');
    expect(platformInitializationScript('x"; alert(1); "')).toBe('window.__RIGHTKIT_PLATFORM__ = "unknown";');
  });

  it("accepts the mac, darwin and win aliases", async () => {
    const { normalizePlatform } = await import("./index.js");
    expect(normalizePlatform("mac")).toBe("macos");
    expect(normalizePlatform("darwin")).toBe("macos");
    expect(normalizePlatform("win")).toBe("windows");
    expect(normalizePlatform("win32")).toBe("windows");
  });

  it("accepts a custom QA query parameter without qa=1 when configured", async () => {
    const { configurePlatform, resolveQaPlatform } = await import("./index.js");
    expect(resolveQaPlatform({ query: "?qaPlatform=macos" })).toBeUndefined();
    configurePlatform({ qaQueryParam: "qaPlatform", qaRequireQaFlag: false });
    expect(resolveQaPlatform({ query: "?qaPlatform=mac" })).toBe("macos");
    expect(resolveQaPlatform({ query: "?qa=1&qaPlatform=win" })).toBe("windows");
    expect(resolveQaPlatform({ query: "?other=win" })).toBeUndefined();
  });

  it("uses a custom QA query parameter name with the qa=1 gate", async () => {
    const { configurePlatform, resolveQaPlatform } = await import("./index.js");
    configurePlatform({ qaQueryParam: "platformOverride" });
    expect(resolveQaPlatform({ query: "?qa=1&platformOverride=linux" })).toBe("linux");
    expect(resolveQaPlatform({ query: "?qa=1&qaPlatform=linux" })).toBeUndefined();
    expect(resolveQaPlatform({ query: "?platformOverride=linux" })).toBeUndefined();
  });

  it("lets native win over the QA override inside a native host when configured", async () => {
    vi.stubGlobal("navigator", { platform: "MacIntel", userAgent: "Mozilla/5.0 (Macintosh)" });
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    globalThis.__RIGHTKIT_QA_PLATFORM__ = "windows";
    osMock.platform.mockReturnValue("macos");
    const { configurePlatform, getPlatform, loadPlatform } = await import("./index.js");

    configurePlatform({ nativeWinsOverQa: true });
    expect(getPlatform()).toBe("macos");
    await loadPlatform();
    expect(getPlatform()).toBe("macos");
  });

  it("keeps the QA override inside a native host by default", async () => {
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    globalThis.__RIGHTKIT_QA_PLATFORM__ = "windows";
    osMock.platform.mockReturnValue("macos");
    const { getPlatform, loadPlatform } = await import("./index.js");
    await loadPlatform();
    expect(getPlatform()).toBe("windows");
  });

  it("uses the QA override with native-wins configured outside a native host", async () => {
    vi.stubGlobal("navigator", { platform: "MacIntel", userAgent: "Mozilla/5.0 (Macintosh)" });
    globalThis.__RIGHTKIT_QA_PLATFORM__ = "windows";
    osMock.platform.mockImplementation(() => {
      throw new Error("not in Tauri");
    });
    const { configurePlatform, getPlatform } = await import("./index.js");
    configurePlatform({ nativeWinsOverQa: true });
    expect(getPlatform()).toBe("windows");
  });

  it("uses the fallback only when the result would be unknown", async () => {
    vi.stubGlobal("navigator", { platform: "MacIntel", userAgent: "Mozilla/5.0 (Macintosh)" });
    osMock.platform.mockReturnValue("android");
    const { configurePlatform, getPlatform, loadPlatform } = await import("./index.js");

    configurePlatform({ fallbackPlatform: "linux" });
    await expect(loadPlatform()).resolves.toBe("linux");
    expect(getPlatform()).toBe("linux");
  });

  it("leaves a known platform alone when a fallback is configured", async () => {
    osMock.platform.mockReturnValue("windows");
    const { configurePlatform, loadPlatform } = await import("./index.js");
    configurePlatform({ fallbackPlatform: "linux" });
    await expect(loadPlatform()).resolves.toBe("windows");
  });

  it("keeps unknown when no fallback is configured", async () => {
    osMock.platform.mockReturnValue("android");
    const { loadPlatform } = await import("./index.js");
    await expect(loadPlatform()).resolves.toBe("unknown");
  });

  it("applies the fallback to a browser report that is unknown", async () => {
    vi.stubGlobal("navigator", { platform: "", userAgent: "Mozilla/5.0 (X11)" });
    osMock.platform.mockImplementation(() => {
      throw new Error("not in Tauri");
    });
    const { configurePlatform, getPlatform } = await import("./index.js");
    configurePlatform({ fallbackPlatform: "windows" });
    expect(getPlatform()).toBe("windows");
  });
});

describe("ctrlormeta chord token", () => {
  it("renders Ctrl or Meta per platform", async () => {
    const { formatChord } = await import("./index.js");
    expect(formatChord("ctrlormeta+k", "macos")).toEqual(["⌘/⌃", "K"]);
    expect(formatChord("ctrlormeta+k", "windows")).toEqual(["Ctrl/Win", "K"]);
    expect(formatChord("ctrlormeta+k", "linux")).toEqual(["Ctrl/Win", "K"]);
    expect(formatChord("ctrlormeta+k", "ios")).toEqual(["Touch", "K"]);
    expect(formatChord("ctrlormeta+k", "unknown")).toEqual(["Ctrl/Meta", "K"]);
  });
});
