import { describe, expect, it } from "vitest";
import { createThemeStore, resolveTheme, type SystemThemeMedia, type ThemeStorage } from "./theme.js";

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
