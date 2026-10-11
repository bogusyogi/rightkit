import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { attachFocusTrap } from "./focus.js";

type Item = { name: string; hidden: boolean; focus(): void; hasAttribute(a: string): boolean };

function setup(names: string[], hidden: string[] = []) {
  const doc: { activeElement: unknown } = { activeElement: null };
  const items: Item[] = names.map((name) => ({
    name,
    hidden: hidden.includes(name),
    focus() {
      doc.activeElement = this;
    },
    hasAttribute(a) {
      return a === "hidden" && this.hidden;
    },
  }));
  const listeners = new Map<string, (e: unknown) => void>();
  const el = {
    ownerDocument: doc,
    isConnected: true,
    items,
    querySelectorAll: () => items,
    contains: (n: unknown) => items.includes(n as Item) || n === el,
    addEventListener: (t: string, fn: (e: unknown) => void) => void listeners.set(t, fn),
    removeEventListener: (t: string) => void listeners.delete(t),
    focus() {
      doc.activeElement = el;
    },
  };
  const opener = { focus: vi.fn() };
  const config = {
    autoFocus: true,
    trap: true,
    restore: true,
    restoreTiming: "microtask" as "microtask" | "sync" | number,
    fromOutside: "both" as "both" | "backward" | "none",
    skipHidden: true,
    emptyList: "prevent" as "prevent" | "pass",
    focusFallback: "container" as "container" | "none",
    opener: opener as unknown as HTMLElement,
    isActive: () => true,
  };
  const attach = (over: Partial<typeof config> = {}) => attachFocusTrap(el as unknown as HTMLElement, { ...config, ...over });
  const tab = (shift = false) => {
    const e = { key: "Tab", shiftKey: shift, preventDefault: vi.fn() };
    listeners.get("keydown")?.(e);
    return e;
  };
  return { doc, el, items, opener, attach, tab, listeners };
}

describe("attachFocusTrap defaults (0.2.1 behaviour)", () => {
  it("focuses the first control, wraps Tab on the last and Shift+Tab on the first", () => {
    const t = setup(["a", "b", "c"]);
    t.attach();
    expect((t.doc.activeElement as Item).name).toBe("a");
    t.items[2].focus();
    expect(t.tab().preventDefault).toHaveBeenCalled();
    expect((t.doc.activeElement as Item).name).toBe("a");
    expect(t.tab(true).preventDefault).toHaveBeenCalled();
    expect((t.doc.activeElement as Item).name).toBe("c");
    t.items[1].focus();
    expect(t.tab().preventDefault).not.toHaveBeenCalled();
  });

  it("falls back to the container, skips [hidden] controls and swallows Tab on an empty list", () => {
    const t = setup(["h"], ["h"]);
    t.attach();
    expect(t.doc.activeElement).toBe(t.el);
    expect(t.tab().preventDefault).toHaveBeenCalled();
  });

  it("wraps from outside in both directions", () => {
    const t = setup(["a", "b"]);
    t.attach();
    t.doc.activeElement = { outside: true };
    expect(t.tab().preventDefault).toHaveBeenCalled();
    expect((t.doc.activeElement as Item).name).toBe("a");
    t.doc.activeElement = { outside: true };
    expect(t.tab(true).preventDefault).toHaveBeenCalled();
    expect((t.doc.activeElement as Item).name).toBe("b");
  });

  it("restores in a microtask, not synchronously", async () => {
    const t = setup(["a"]);
    const detach = t.attach();
    t.el.isConnected = false;
    detach();
    expect(t.opener.focus).not.toHaveBeenCalled();
    await Promise.resolve();
    expect(t.opener.focus).toHaveBeenCalledTimes(1);
  });

  it("does not restore while the container is still connected and active (StrictMode cleanup)", async () => {
    const t = setup(["a"]);
    const detach = t.attach();
    detach();
    await Promise.resolve();
    expect(t.opener.focus).not.toHaveBeenCalled();
  });

  it("restores when active turns false even if the container stays connected", async () => {
    const t = setup(["a"]);
    const detach = t.attach({ isActive: () => false });
    detach();
    await Promise.resolve();
    expect(t.opener.focus).toHaveBeenCalledTimes(1);
  });
});

describe("attachFocusTrap options", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("autoFocus false leaves focus alone", () => {
    const t = setup(["a"]);
    t.attach({ autoFocus: false });
    expect(t.doc.activeElement).toBe(null);
  });

  it("focusFallback none does not focus the container when there is no control", () => {
    const t = setup([]);
    t.attach({ focusFallback: "none" });
    expect(t.doc.activeElement).toBe(null);
  });

  it("skipHidden false counts [hidden] controls", () => {
    const t = setup(["h", "b"], ["h"]);
    t.attach({ skipHidden: false });
    expect((t.doc.activeElement as Item).name).toBe("h");
  });

  it("emptyList pass leaves Tab native", () => {
    const t = setup([]);
    t.attach({ emptyList: "pass" });
    expect(t.tab().preventDefault).not.toHaveBeenCalled();
  });

  it("fromOutside backward wraps only Shift+Tab; none wraps neither", () => {
    const back = setup(["a", "b"]);
    back.attach({ fromOutside: "backward" });
    back.doc.activeElement = { outside: true };
    expect(back.tab().preventDefault).not.toHaveBeenCalled();
    expect(back.tab(true).preventDefault).toHaveBeenCalled();
    expect((back.doc.activeElement as Item).name).toBe("b");

    const none = setup(["a", "b"]);
    none.attach({ fromOutside: "none" });
    none.doc.activeElement = { outside: true };
    expect(none.tab().preventDefault).not.toHaveBeenCalled();
    expect(none.tab(true).preventDefault).not.toHaveBeenCalled();
    // Inside the container the ends still wrap.
    none.items[1].focus();
    expect(none.tab().preventDefault).toHaveBeenCalled();
  });

  it("forward Tab on the last control wraps even with fromOutside backward", () => {
    const t = setup(["a", "b"]);
    t.attach({ fromOutside: "backward" });
    t.items[1].focus();
    expect(t.tab().preventDefault).toHaveBeenCalled();
    expect((t.doc.activeElement as Item).name).toBe("a");
  });

  it("trap false registers no keydown listener but still focuses and restores", async () => {
    const t = setup(["a"]);
    const detach = t.attach({ trap: false });
    expect(t.listeners.size).toBe(0);
    expect((t.doc.activeElement as Item).name).toBe("a");
    t.el.isConnected = false;
    detach();
    await Promise.resolve();
    expect(t.opener.focus).toHaveBeenCalled();
  });

  it("restore false never refocuses the opener", async () => {
    const t = setup(["a"]);
    t.el.isConnected = false;
    t.attach({ restore: false })();
    await Promise.resolve();
    vi.runAllTimers();
    expect(t.opener.focus).not.toHaveBeenCalled();
  });

  it("restoreTiming sync restores inside the cleanup", () => {
    const t = setup(["a"]);
    t.el.isConnected = false;
    t.attach({ restoreTiming: "sync" })();
    expect(t.opener.focus).toHaveBeenCalledTimes(1);
  });

  it("restoreTiming <ms> restores after the delay", () => {
    const t = setup(["a"]);
    t.el.isConnected = false;
    t.attach({ restoreTiming: 50 })();
    vi.advanceTimersByTime(49);
    expect(t.opener.focus).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(t.opener.focus).toHaveBeenCalledTimes(1);
  });
});
