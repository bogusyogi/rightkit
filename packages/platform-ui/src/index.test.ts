import { describe, expect, it, vi } from "vitest";
import {
  WINDOW_CONTROL_LABELS,
  createWindowControls,
  formatChord,
  getPlatform,
  normalizePlatform,
  platformCapabilities,
} from "./index.js";

describe("platform primitives", () => {
  it("normalizes only supported Tauri platform names", () => {
    expect(normalizePlatform("macos")).toBe("macos");
    expect(normalizePlatform("ios")).toBe("ios");
    expect(normalizePlatform("windows")).toBe("windows");
    expect(normalizePlatform("linux")).toBe("linux");
    expect(normalizePlatform("android")).toBe("linux");
    expect(normalizePlatform("made-up")).toBe("linux");
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
  it("delegates each action and owns stable accessible names", async () => {
    const adapter = {
      minimize: vi.fn(async () => undefined),
      toggleMaximize: vi.fn(async () => undefined),
      close: vi.fn(async () => undefined),
      setFullscreen: vi.fn(async (_value: boolean) => undefined),
    };
    const controls = createWindowControls(adapter);

    await controls.minimize();
    await controls.toggleMaximize();
    await controls.close();
    await controls.enterFullscreen();
    await controls.exitFullscreen();

    expect(adapter.minimize).toHaveBeenCalledOnce();
    expect(adapter.toggleMaximize).toHaveBeenCalledOnce();
    expect(adapter.close).toHaveBeenCalledOnce();
    expect(adapter.setFullscreen).toHaveBeenNthCalledWith(1, true);
    expect(adapter.setFullscreen).toHaveBeenNthCalledWith(2, false);
    expect(WINDOW_CONTROL_LABELS).toEqual({
      minimize: "Minimize window",
      maximize: "Maximize or restore window",
      close: "Close window",
      enterFullscreen: "Enter fullscreen",
      exitFullscreen: "Exit fullscreen",
    });
  });
});
