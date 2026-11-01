// @vitest-environment happy-dom
import { beforeEach, describe, expect, it, vi } from "vitest";

// B7 proof: the palette switch applies RightKit token overrides on the root and persists cr.palette.v1.
// The store reads localStorage at import time, so each test loads a fresh module.
async function loadStore(stored?: string) {
  vi.resetModules();
  localStorage.clear();
  if (stored !== undefined) localStorage.setItem("cr.palette.v1", stored);
  return import("./themeStore");
}

describe("palette switch (themeStore)", () => {
  beforeEach(() => {
    document.documentElement.removeAttribute("style");
    document.documentElement.removeAttribute("data-theme");
  });

  it("applies token overrides on the root for a sibling palette and removes them for coderight", async () => {
    const { useThemeStore } = await loadStore();
    const base = document.documentElement.style.cssText;

    useThemeStore.getState().setPalette("mailright");
    const applied = document.documentElement.style.cssText;
    expect(applied).not.toBe(base);
    expect(applied).toContain("--");

    useThemeStore.getState().setPalette("coderight");
    expect(document.documentElement.style.cssText).toBe(base);
  });

  it("persists the choice under cr.palette.v1", async () => {
    const { useThemeStore } = await loadStore();
    useThemeStore.getState().setPalette("viewright");
    expect(localStorage.getItem("cr.palette.v1")).toBe("viewright");
    useThemeStore.getState().setPalette("coderight");
    expect(localStorage.getItem("cr.palette.v1")).toBe("coderight");
  });

  it("restores a stored sibling palette on load and ignores unknown stored values", async () => {
    const restored = await loadStore("scraperight");
    expect(restored.useThemeStore.getState().palette).toBe("scraperight");
    expect(document.documentElement.style.cssText).not.toBe("");

    const fallback = await loadStore("not-a-palette");
    expect(fallback.useThemeStore.getState().palette).toBe("coderight");
  });

  describe("System theme", () => {
    function stubMedia(dark: boolean) {
      const listeners = new Set<() => void>();
      const media = {
        matches: dark,
        addEventListener: (_: string, l: () => void) => listeners.add(l),
        removeEventListener: (_: string, l: () => void) => listeners.delete(l),
      };
      vi.stubGlobal("matchMedia", () => media);
      Object.defineProperty(window, "matchMedia", { configurable: true, value: () => media });
      return {
        flip(next: boolean) {
          media.matches = next;
          for (const l of listeners) l();
        },
      };
    }

    it("resolves System to the dark palette tokens and no data-theme attribute when the OS is dark", async () => {
      stubMedia(true);
      vi.resetModules();
      localStorage.clear();
      localStorage.setItem("cr.theme.v1", "system");
      localStorage.setItem("cr.palette.v1", "mailright");
      const mod = await import("./themeStore");
      expect(document.documentElement.getAttribute("data-theme")).toBeNull();
      expect(mod.shellThemeStore.resolved()).toBe("dark");
      expect(document.documentElement.style.cssText).toContain("--");
    });

    it("resolves System to light data-theme and light palette tokens when the OS is light, and follows OS changes", async () => {
      const media = stubMedia(false);
      vi.resetModules();
      localStorage.clear();
      localStorage.setItem("cr.theme.v1", "system");
      localStorage.setItem("cr.palette.v1", "mailright");
      const mod = await import("./themeStore");
      const lightStyle = document.documentElement.style.cssText;
      expect(document.documentElement.getAttribute("data-theme")).toBe("light");
      expect(mod.shellThemeStore.resolved()).toBe("light");
      media.flip(true);
      expect(document.documentElement.getAttribute("data-theme")).toBeNull();
      expect(mod.shellThemeStore.resolved()).toBe("dark");
      expect(document.documentElement.style.cssText).not.toBe(lightStyle);
    });
  });
});
