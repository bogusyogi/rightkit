import { afterEach, describe, expect, it, vi } from "vitest";
import { applyThemeAttribute, onPrefersDarkChange, prefersDark, themeAttributeValue, themeBootScript } from "./runtime.js";

function fakeEl() {
  const attrs = new Map<string, string>();
  return {
    attrs,
    setAttribute: (n: string, v: string) => void attrs.set(n, v),
    removeAttribute: (n: string) => void attrs.delete(n),
  };
}

function runBoot(script: string, stored: string | null | "throw", el: ReturnType<typeof fakeEl>, dark = false) {
  const localStorage = {
    getItem: () => {
      if (stored === "throw") throw new Error("blocked");
      return stored;
    },
  };
  new Function("localStorage", "document", "matchMedia", script)(localStorage, { documentElement: el }, () => ({ matches: dark }));
}

describe("themeAttributeValue / applyThemeAttribute", () => {
  it("default: dark writes data-theme=dark", () => {
    expect(themeAttributeValue("dark")).toBe("dark");
    const el = fakeEl();
    applyThemeAttribute("dark", el);
    expect(el.attrs.get("data-theme")).toBe("dark");
  });

  it("darkRemovesAttribute: dark removes it, light writes it", () => {
    const el = fakeEl();
    el.attrs.set("data-theme", "light");
    applyThemeAttribute("dark", el, { darkRemovesAttribute: true });
    expect(el.attrs.has("data-theme")).toBe(false);
    applyThemeAttribute("light", el, { darkRemovesAttribute: true });
    expect(el.attrs.get("data-theme")).toBe("light");
  });

  it("is a no-op without a root", () => {
    expect(() => applyThemeAttribute("dark", null)).not.toThrow();
  });
});

describe("themeBootScript (consolidated first paint)", () => {
  it("applies stored choice, resolves system, falls back on throwing storage", () => {
    const s = themeBootScript("k");
    const dark = fakeEl();
    runBoot(s, "dark", dark, false);
    expect(dark.attrs.get("data-theme")).toBe("dark");
    const sysDark = fakeEl();
    runBoot(s, "system", sysDark, true);
    expect(sysDark.attrs.get("data-theme")).toBe("dark");
    const none = fakeEl();
    runBoot(s, null, none, false);
    expect(none.attrs.get("data-theme")).toBe("light");
    const thrown = fakeEl();
    runBoot(themeBootScript("k", { defaultChoice: "dark" }), "throw", thrown, false);
    expect(thrown.attrs.get("data-theme")).toBe("dark");
  });

  it("darkRemovesAttribute: resolved dark removes, light writes", () => {
    const s = themeBootScript("k", { darkRemovesAttribute: true });
    const dark = fakeEl();
    dark.attrs.set("data-theme", "stale");
    runBoot(s, "dark", dark);
    expect(dark.attrs.has("data-theme")).toBe(false);
    const sysDark = fakeEl();
    sysDark.attrs.set("data-theme", "stale");
    runBoot(s, "system", sysDark, true);
    expect(sysDark.attrs.has("data-theme")).toBe(false);
    const light = fakeEl();
    runBoot(s, "light", light);
    expect(light.attrs.get("data-theme")).toBe("light");
  });

  it("alwaysPaintDataTheme:false removes attribute for system; script is inline-safe", () => {
    const el = fakeEl();
    el.attrs.set("data-theme", "dark");
    runBoot(themeBootScript("k", { alwaysPaintDataTheme: false }), "system", el, true);
    expect(el.attrs.has("data-theme")).toBe(false);
    expect(themeBootScript('a</script>"')).not.toContain("</script>");
  });
});

function fakeMedia(initial: boolean) {
  const handlers = new Set<(e: { matches: boolean }) => void>();
  const queries: string[] = [];
  const list = {
    matches: initial,
    addEventListener: (type: string, h: (e: { matches: boolean }) => void) => {
      if (type === "change") handlers.add(h);
    },
    removeEventListener: (type: string, h: (e: { matches: boolean }) => void) => {
      if (type === "change") handlers.delete(h);
    },
  };
  vi.stubGlobal("window", {
    matchMedia: (q: string) => {
      queries.push(q);
      return list;
    },
  });
  return {
    handlers,
    queries,
    set(matches: boolean) {
      list.matches = matches;
      for (const h of [...handlers]) h({ matches });
    },
  };
}

describe("prefersDark / onPrefersDarkChange", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("is false and subscribes nothing when there is no window (SSR)", () => {
    expect(typeof window).toBe("undefined");
    expect(prefersDark()).toBe(false);
    const cb = vi.fn();
    const off = onPrefersDarkChange(cb);
    expect(() => off()).not.toThrow();
    expect(cb).not.toHaveBeenCalled();
  });

  it("is false when window has no matchMedia", () => {
    vi.stubGlobal("window", {});
    expect(prefersDark()).toBe(false);
    expect(() => onPrefersDarkChange(() => {})()).not.toThrow();
  });

  it("reads the dark query", () => {
    const m = fakeMedia(true);
    expect(prefersDark()).toBe(true);
    expect(m.queries).toEqual(["(prefers-color-scheme: dark)"]);
    m.set(false);
    expect(prefersDark()).toBe(false);
  });

  it("delivers changes and stops after unsubscribe", () => {
    const m = fakeMedia(false);
    const seen: boolean[] = [];
    const off = onPrefersDarkChange((d) => seen.push(d));
    expect(seen).toEqual([]);
    m.set(true);
    m.set(false);
    expect(seen).toEqual([true, false]);
    off();
    off();
    expect(m.handlers.size).toBe(0);
    m.set(true);
    expect(seen).toEqual([true, false]);
  });

  it("falls back to addListener/removeListener on old Safari", () => {
    const handlers = new Set<(e: { matches: boolean }) => void>();
    vi.stubGlobal("window", {
      matchMedia: () => ({
        matches: false,
        addListener: (h: (e: { matches: boolean }) => void) => handlers.add(h),
        removeListener: (h: (e: { matches: boolean }) => void) => handlers.delete(h),
      }),
    });
    const seen: boolean[] = [];
    const off = onPrefersDarkChange((d) => seen.push(d));
    for (const h of handlers) h({ matches: true });
    expect(seen).toEqual([true]);
    off();
    expect(handlers.size).toBe(0);
  });
});
