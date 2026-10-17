import { describe, expect, it } from "vitest";
import { applyStoredThemeSync, createThemeStore, resolveTheme, themeBootScript, type SystemThemeMedia, type ThemeStorage } from "./theme.js";

function memoryStorage(initial: Record<string, string> = {}): ThemeStorage & { data: Map<string, string> } {
  const data = new Map(Object.entries(initial));
  return {
    data,
    getItem: (k) => data.get(k) ?? null,
    setItem: (k, v) => void data.set(k, v),
    removeItem: (k) => void data.delete(k),
  };
}

function fakeRoot() {
  const attrs = new Map<string, string>();
  return {
    attrs,
    setAttribute: (n: string, v: string) => void attrs.set(n, v),
    removeAttribute: (n: string) => void attrs.delete(n),
  };
}

function fakeMedia(matches: boolean) {
  const listeners = new Set<() => void>();
  const media: SystemThemeMedia & { set(next: boolean): void; listeners: Set<() => void> } = {
    matches,
    addEventListener: (_type, l) => void listeners.add(l),
    removeEventListener: (_type, l) => void listeners.delete(l),
    set(next) {
      (media as { matches: boolean }).matches = next;
      for (const l of [...listeners]) l();
    },
    listeners,
  };
  return media;
}

describe("resolveTheme", () => {
  it("resolves system against the OS preference and passes explicit choices through", () => {
    expect(resolveTheme("system", true)).toBe("dark");
    expect(resolveTheme("system", false)).toBe("light");
    expect(resolveTheme("light", true)).toBe("light");
  });
});

function countingRoot() {
  const attrs = new Map<string, string>();
  const writes: string[] = [];
  return {
    attrs,
    writes,
    setAttribute: (n: string, v: string) => {
      writes.push(v);
      attrs.set(n, v);
    },
    removeAttribute: (n: string) => void attrs.delete(n),
    getAttribute: (n: string) => attrs.get(n) ?? null,
  };
}

describe("applyStoredThemeSync", () => {
  it("applies a stored dark theme before any store or render exists, and the store then does not flip", () => {
    const root = countingRoot();
    const storage = memoryStorage({ t: "dark" });
    expect(applyStoredThemeSync({ storageKey: "t", storage, root, systemMedia: null })).toBe("dark");
    expect(root.attrs.get("data-theme")).toBe("dark");
    expect(root.writes).toEqual(["dark"]);

    const store = createThemeStore({ storageKey: "t", storage, root, systemMedia: null });
    store.apply(store.read());
    expect(root.writes).toEqual(["dark"]);
    store.dispose();
  });

  it("resolves system through matchMedia and leaves no listener behind", () => {
    const root = countingRoot();
    const media = fakeMedia(true);
    expect(applyStoredThemeSync({ storageKey: "t", storage: memoryStorage({ t: "system" }), root, systemMedia: media })).toBe("dark");
    expect(root.attrs.get("data-theme")).toBe("dark");
    expect(media.listeners.size).toBe(0);
  });

  it("falls back to the default choice when storage throws, and never throws", () => {
    const root = countingRoot();
    const storage: ThemeStorage = {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {},
      removeItem: () => {},
    };
    expect(applyStoredThemeSync({ storageKey: "t", storage, root, systemMedia: null, defaultChoice: "dark" })).toBe("dark");
    expect(root.attrs.get("data-theme")).toBe("dark");
    const badRoot = {
      setAttribute: () => {
        throw new Error("boom");
      },
      removeAttribute: () => {},
    };
    expect(applyStoredThemeSync({ storageKey: "t", storage, root: badRoot, systemMedia: null })).toBeNull();
  });

  it("returns null for system with alwaysPaintDataTheme: false and removes data-theme", () => {
    const root = countingRoot();
    root.attrs.set("data-theme", "dark");
    expect(applyStoredThemeSync({ storageKey: "t", storage: memoryStorage(), root, systemMedia: fakeMedia(true), alwaysPaintDataTheme: false })).toBeNull();
    expect(root.attrs.has("data-theme")).toBe(false);
  });
});

describe("themeBootScript", () => {
  function run(script: string, stored: string | null | "throw", dark: boolean) {
    const root = countingRoot();
    const localStorage = {
      getItem: () => {
        if (stored === "throw") throw new Error("blocked");
        return stored;
      },
    };
    new Function("localStorage", "document", "matchMedia", script)(localStorage, { documentElement: root }, () => ({ matches: dark }));
    return root.attrs.get("data-theme");
  }

  it("applies the stored choice, resolves system, and falls back on throwing storage", () => {
    const script = themeBootScript({ storageKey: "t" });
    expect(run(script, "dark", false)).toBe("dark");
    expect(run(script, "system", true)).toBe("dark");
    expect(run(script, null, false)).toBe("light");
    expect(run(script, "throw", true)).toBe("dark");
    expect(run(themeBootScript({ storageKey: "t", defaultChoice: "dark" }), "throw", false)).toBe("dark");
    expect(run(themeBootScript({ storageKey: "t", qaOverride: "light" }), "dark", true)).toBe("light");
  });

  it("matches applyStoredThemeSync and is safe to inline", () => {
    const script = themeBootScript({ storageKey: 'a</script>"' });
    expect(script).not.toContain("</script>");
    expect(() => run(script, "light", false)).not.toThrow();
  });
});

describe("createThemeStore", () => {
  it("paints the resolved value for system by default, so data-theme is never missing", () => {
    const root = fakeRoot();
    const store = createThemeStore({ storageKey: "t", storage: memoryStorage(), root, systemMedia: fakeMedia(true) });
    store.apply(store.read());
    expect(root.attrs.get("data-theme")).toBe("dark");
    store.dispose();
  });

  it("restores the earlier behaviour with alwaysPaintDataTheme: false", () => {
    const root = fakeRoot();
    const store = createThemeStore({ storageKey: "t", storage: memoryStorage(), root, systemMedia: fakeMedia(true), alwaysPaintDataTheme: false });
    store.apply(store.read());
    expect(root.attrs.has("data-theme")).toBe(false);
    store.dispose();
  });

  it("uses defaultChoice when storage is unset", () => {
    const store = createThemeStore({ storageKey: "t", storage: memoryStorage(), root: fakeRoot(), systemMedia: null, defaultChoice: "dark" });
    expect(store.read()).toBe("dark");
    store.dispose();
  });

  it("stores a non-default choice and clears the key when returning to the default", () => {
    const storage = memoryStorage();
    const store = createThemeStore({ storageKey: "t", storage, root: fakeRoot(), systemMedia: null, defaultChoice: "dark" });
    store.save("light");
    expect(storage.data.get("t")).toBe("light");
    store.save("dark");
    expect(storage.data.has("t")).toBe(false);
    store.dispose();
  });

  it("re-applies on OS changes while the choice is system, without any React", () => {
    const root = fakeRoot();
    const media = fakeMedia(false);
    const store = createThemeStore({ storageKey: "t", storage: memoryStorage(), root, systemMedia: media });
    const seen: string[] = [];
    store.subscribe((choice) => seen.push(choice));
    store.apply(store.read());
    expect(root.attrs.get("data-theme")).toBe("light");

    media.set(true);
    expect(root.attrs.get("data-theme")).toBe("dark");
    expect(store.resolved()).toBe("dark");
    expect(seen).toEqual(["system"]);
    store.dispose();
  });

  it("ignores OS changes when the user chose an explicit theme", () => {
    const root = fakeRoot();
    const media = fakeMedia(false);
    const store = createThemeStore({ storageKey: "t", storage: memoryStorage(), root, systemMedia: media });
    store.save("light");
    media.set(true);
    expect(root.attrs.get("data-theme")).toBe("light");
    store.dispose();
  });

  it("detaches the OS listener on dispose", () => {
    const media = fakeMedia(false);
    const store = createThemeStore({ storageKey: "t", storage: memoryStorage(), root: fakeRoot(), systemMedia: media });
    expect(media.listeners.size).toBe(1);
    store.dispose();
    expect(media.listeners.size).toBe(0);
  });
});

describe("createThemeStore darkRemovesAttribute (0.6.9)", () => {
  function attributeRoot() {
    const attrs = new Map<string, string>();
    return {
      attrs,
      root: {
        setAttribute: (n: string, v: string) => void attrs.set(n, v),
        removeAttribute: (n: string) => void attrs.delete(n),
        getAttribute: (n: string) => attrs.get(n) ?? null,
      },
    };
  }

  it("removes data-theme for dark and writes it only for light", () => {
    const { attrs, root } = attributeRoot();
    const store = createThemeStore({ storageKey: "t", storage: memoryStorage({ t: "dark" }), root, systemMedia: null, darkRemovesAttribute: true });
    store.apply(store.read());
    expect(attrs.has("data-theme")).toBe(false);
    store.save("light");
    expect(attrs.get("data-theme")).toBe("light");
    store.save("dark");
    expect(attrs.has("data-theme")).toBe(false);
    store.dispose();
  });

  it("keeps writing data-theme=dark by default", () => {
    const { attrs, root } = attributeRoot();
    const store = createThemeStore({ storageKey: "t", storage: memoryStorage({ t: "dark" }), root, systemMedia: null });
    store.apply(store.read());
    expect(attrs.get("data-theme")).toBe("dark");
    store.dispose();
  });
});
