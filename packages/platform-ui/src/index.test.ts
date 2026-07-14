import { describe, expect, it, vi } from "vitest";
import {
  DEFAULT_WINDOW_CONTROL_LABELS,
  bootstrapQaPlatform,
  createFullscreenControls,
  createWindowControlLabels,
  WINDOW_CONTROL_LABELS,
  createWindowControls,
  formatChord,
  getPlatform,
  normalizePlatform,
  platformCapabilities,
  resolveQaPlatform,
} from "./index.js";

describe("platform primitives", () => {
  it("normalizes supported Tauri names and the explicit Win32 QA alias", () => {
    expect(normalizePlatform("macos")).toBe("macos");
    expect(normalizePlatform("ios")).toBe("ios");
    expect(normalizePlatform("windows")).toBe("windows");
    expect(normalizePlatform("linux")).toBe("linux");
    expect(normalizePlatform("Win32")).toBe("windows");
    expect(normalizePlatform("android")).toBe("unknown");
    expect(normalizePlatform("made-up")).toBe("unknown");
  });

  it("only resolves a query override behind the canonical QA gate", () => {
    expect(resolveQaPlatform({ query: "?qa=1&qaPlatform=Win32" })).toBe("windows");
    expect(resolveQaPlatform({ query: "?qa=0&qaPlatform=macos" })).toBeUndefined();
    expect(resolveQaPlatform({ query: "?qaPlatform=macos" })).toBeUndefined();
  });

  it("only resolves canonical, explicitly gated QA environment input", () => {
    expect(
      resolveQaPlatform({
        env: { RIGHTKIT_QA: "1", RIGHTKIT_QA_PLATFORM: "macos" },
      }),
    ).toBe("macos");
    expect(
      resolveQaPlatform({
        env: { RIGHTKIT_QA: "0", RIGHTKIT_QA_PLATFORM: "windows" },
      }),
    ).toBeUndefined();
  });

  it("bootstraps the global QA override only from explicit canonical input", () => {
    delete globalThis.__RIGHTKIT_QA_PLATFORM__;
    expect(bootstrapQaPlatform({ query: "?qa=1&qaPlatform=android" })).toBe("unknown");
    expect(globalThis.__RIGHTKIT_QA_PLATFORM__).toBe("unknown");
    delete globalThis.__RIGHTKIT_QA_PLATFORM__;
  });

  it("clears a stale QA override when the canonical gate is absent", () => {
    globalThis.__RIGHTKIT_QA_PLATFORM__ = "windows";
    expect(bootstrapQaPlatform({ query: "?qa=0&qaPlatform=macos" })).toBeUndefined();
    expect(globalThis.__RIGHTKIT_QA_PLATFORM__).toBeUndefined();
  });

  it("formats one canonical chord for each OS without browser sniffing", () => {
    expect(formatChord("mod+alt+shift+space", "macos")).toEqual(["⌘", "⌥", "⇧", "Space"]);
    expect(formatChord("mod+alt+shift+space", "windows")).toEqual([
      "Ctrl",
      "Alt",
      "Shift",
      "Space",
    ]);
    expect(formatChord("record", "ios")).toEqual(["Record"]);
  });

  it("formats a stored shortcut for display without rewriting the stored value", () => {
    const storedShortcut = "rctrl+alt+k";
    expect(formatChord(storedShortcut, "windows")).toEqual(["Right Ctrl", "Alt", "K"]);
    expect(storedShortcut).toBe("rctrl+alt+k");
  });

  it("lets an explicit QA platform override the cached native result", () => {
    getPlatform();
    globalThis.__RIGHTKIT_QA_PLATFORM__ = "windows";
    expect(getPlatform()).toBe("windows");
    globalThis.__RIGHTKIT_QA_PLATFORM__ = "macos";
    expect(getPlatform()).toBe("macos");
    delete globalThis.__RIGHTKIT_QA_PLATFORM__;
  });

  it("exposes explicit capability flags instead of product assumptions", () => {
    expect(platformCapabilities("macos")).toMatchObject({ nativeTrafficLights: true });
    expect(platformCapabilities("windows")).toMatchObject({ customCaptionButtons: true });
    expect(platformCapabilities("ios")).toMatchObject({ desktopWindowControls: false });
  });
});

describe("headless native window controls", () => {
  it("supports apps that only implement minimize, maximize, and close", async () => {
    const adapter = {
      minimize: vi.fn(async () => undefined),
      toggleMaximize: vi.fn(async () => undefined),
      close: vi.fn(async () => undefined),
    };
    const controls = createWindowControls(adapter);

    await controls.minimize();
    await controls.toggleMaximize();
    await controls.close();

    expect(adapter.minimize).toHaveBeenCalledOnce();
    expect(adapter.toggleMaximize).toHaveBeenCalledOnce();
    expect(adapter.close).toHaveBeenCalledOnce();
  });

  it("exposes fullscreen as a separate optional adapter", async () => {
    const adapter = {
      setFullscreen: vi.fn(async (_value: boolean) => undefined),
    };
    const controls = createFullscreenControls(adapter);

    await controls.enterFullscreen();
    await controls.exitFullscreen();

    expect(adapter.setFullscreen).toHaveBeenNthCalledWith(1, true);
    expect(adapter.setFullscreen).toHaveBeenNthCalledWith(2, false);
  });

  it("provides brand-neutral default labels that apps can override", () => {
    expect(DEFAULT_WINDOW_CONTROL_LABELS).toEqual({
      minimize: "Minimize window",
      maximize: "Maximize or restore window",
      close: "Close window",
      enterFullscreen: "Enter fullscreen",
      exitFullscreen: "Exit fullscreen",
    });
    expect(WINDOW_CONTROL_LABELS).toBe(DEFAULT_WINDOW_CONTROL_LABELS);
    expect(createWindowControlLabels({ close: "Close inspector" })).toEqual({
      ...DEFAULT_WINDOW_CONTROL_LABELS,
      close: "Close inspector",
    });
  });
});
