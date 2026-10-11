import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import * as headless from "./headless.js";

const here = dirname(fileURLToPath(import.meta.url));

describe("@rightkit/platform-ui/headless", () => {
  it("exports the headless helpers", () => {
    expect(typeof headless.formatChord).toBe("function");
    expect(typeof headless.floatingPanelStyle).toBe("function");
    expect(typeof headless.createToastBus).toBe("function");
    expect(typeof headless.toast).toBe("function");
    expect(typeof headless.useFocusTrap).toBe("function");
    expect(typeof headless.useModalDismiss).toBe("function");
    expect(typeof headless.useLatest).toBe("function");
    for (const name of ["useDismiss", "useExitTransition", "useListNavigation", "fitToViewport", "rankCommands", "rankItems", "matchScore", "attachFocusTrap", "attachDismiss", "createExitTransition"]) {
      expect(typeof (headless as Record<string, unknown>)[name], name).toBe("function");
    }
  });

  it("formats chords without detecting the platform", () => {
    expect(headless.formatChord("mod+shift+p", "macos")).toEqual(["⌘", "⇧", "P"]);
    expect(headless.formatChord("mod+shift+p")).toEqual(["Mod", "Shift", "P"]);
    expect(headless.formatChord("lalt+ralt+rctrl+lshift", "macos")).toEqual(["Left Option", "Right Option", "Right Control", "Left Shift"]);
    expect(headless.formatChord("lalt+ralt+lctrl+rshift", "windows")).toEqual(["Left Alt", "Right Alt", "Left Ctrl", "Right Shift"]);
    expect(headless.formatChord("lalt", "unknown")).toEqual(["Left Alt"]);
    expect(headless.formatChord("mod+esc", "macos", { labels: { esc: "esc" } })).toEqual(["⌘", "esc"]);
    expect(headless.formatChord("mod+space", "unknown", { labels: { mod: "Ctrl" } })).toEqual(["Ctrl", "Space"]);
  });

  it("clamps a floating panel inside the viewport", () => {
    const style = headless.floatingPanelStyle({
      x: 990,
      y: 700,
      width: 200,
      estimatedHeight: 100,
      viewportWidth: 1000,
      viewportHeight: 768,
    });
    expect(style.left + 200).toBeLessThanOrEqual(1000);
    expect(style.top + 100).toBeLessThanOrEqual(768);
  });

  it("delivers toasts through the bus without React", () => {
    const bus = headless.createToastBus();
    const seen: string[] = [];
    const off = bus.subscribe((event) => {
      if (event.type === "push") seen.push(event.toast.message);
    });
    bus.push("Saved", "success");
    off();
    expect(seen).toEqual(["Saved"]);
  });

  it("never imports @tauri-apps/plugin-os (static or dynamic)", () => {
    const modules = ["headless.ts", "format.ts", "geometry.ts", "toast.ts", "focus.ts", "dismiss.ts", "exit.ts", "list.ts", "rank.ts"];
    for (const name of modules) {
      const source = readFileSync(join(here, name), "utf8");
      expect(source, name).not.toMatch(/from\s+["']@tauri-apps\/plugin-os["']/);
      expect(source, name).not.toMatch(/import\(\s*["']@tauri-apps\/plugin-os["']\s*\)/);
    }
  });

  it("loads without plugin-os being resolvable", async () => {
    vi.resetModules();
    vi.doMock("@tauri-apps/plugin-os", () => {
      throw new Error("headless must not load plugin-os");
    });
    await expect(import("./headless.js")).resolves.toBeDefined();
    vi.doUnmock("@tauri-apps/plugin-os");
  });
});
