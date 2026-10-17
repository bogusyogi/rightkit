import { describe, expect, it, vi } from "vitest";
import { attachDismiss, type DismissConfig } from "./dismiss.js";

function fakeTarget() {
  const handlers: Array<{ type: string; fn: (e: any) => void; capture: boolean }> = [];
  return {
    addEventListener: (type: string, fn: (e: any) => void, capture = false) => void handlers.push({ type, fn, capture: !!capture }),
    removeEventListener: (type: string, fn: (e: any) => void) => {
      const i = handlers.findIndex((h) => h.type === type && h.fn === fn);
      if (i >= 0) handlers.splice(i, 1);
    },
    emit: (type: string, ev: Record<string, unknown> = {}) => {
      const e = { preventDefault: vi.fn(), stopPropagation: vi.fn(), ...ev };
      for (const h of [...handlers]) if (h.type === type) h.fn(e);
      return e as { preventDefault: ReturnType<typeof vi.fn> };
    },
    handlers,
  };
}

const MENU_A = { menu: "a" };
const MENU_B = { menu: "b" };
const OUT = { out: true };

function setup(over: Partial<DismissConfig> = {}) {
  const win = fakeTarget();
  const doc = fakeTarget();
  const onDismiss = vi.fn();
  const menus = new Set<unknown>([MENU_A, MENU_B]);
  const config: DismissConfig = {
    onDismiss,
    escape: false,
    escapeTarget: null,
    preventDefault: true,
    stopPropagation: true,
    imeGuard: true,
    outside: "mousedown",
    outsideContextMenu: false,
    isOutside: (t) => !menus.has(t) && t === OUT,
    onWindowBlur: false,
    onResize: false,
    onScroll: false,
    ...over,
  };
  const detach = attachDismiss(config, { win, doc });
  return { doc, onDismiss, detach };
}

describe("multi-zone outside detection", () => {
  it("a press inside any of several zones is not outside", () => {
    const t = setup();
    t.doc.emit("mousedown", { target: MENU_B });
    expect(t.onDismiss).not.toHaveBeenCalled();
    t.doc.emit("mousedown", { target: OUT });
    expect(t.onDismiss).toHaveBeenCalledWith("outside", expect.anything());
  });
});

describe("focus-out dismiss", () => {
  it("dismisses with reason focus when focus moves to a target outside", () => {
    const t = setup({ onFocusOut: true });
    t.doc.emit("focusout", { relatedTarget: OUT });
    expect(t.onDismiss).toHaveBeenCalledWith("focus", expect.anything());
  });

  it("does not dismiss when focus moves within the panel, or to nothing", () => {
    const t = setup({ onFocusOut: true });
    t.doc.emit("focusout", { relatedTarget: MENU_A });
    t.doc.emit("focusout", { relatedTarget: null });
    expect(t.onDismiss).not.toHaveBeenCalled();
  });

  it("a Tab keydown dismisses once focus has settled outside, and never prevents the Tab", async () => {
    vi.useFakeTimers();
    try {
      const t = setup({ onFocusOut: true });
      const e = t.doc.emit("keydown", { key: "Tab" });
      expect(e.preventDefault).not.toHaveBeenCalled();
      expect(t.onDismiss).not.toHaveBeenCalled();
      (t.doc as unknown as { activeElement: unknown }).activeElement = OUT;
      vi.runAllTimers();
      expect(t.onDismiss).toHaveBeenCalledWith("focus", expect.anything());
    } finally {
      vi.useRealTimers();
    }
  });

  it("attaches no focus listeners by default", () => {
    const t = setup();
    expect(t.doc.handlers.some((h) => h.type === "focusout" || h.type === "keydown")).toBe(false);
  });
});

describe("focus-out dismiss hardening", () => {
  it("ignores a focusout that started outside the panel", () => {
    const t = setup({ onFocusOut: true });
    t.doc.emit("focusout", { target: OUT, relatedTarget: OUT });
    expect(t.onDismiss).not.toHaveBeenCalled();
  });

  it("a Tab then focusout dismisses once, not again when the timer fires", () => {
    vi.useFakeTimers();
    try {
      const t = setup({ onFocusOut: true });
      t.doc.emit("keydown", { key: "Tab", target: MENU_A });
      t.doc.emit("focusout", { target: MENU_A, relatedTarget: OUT });
      (t.doc as unknown as { activeElement: unknown }).activeElement = OUT;
      vi.runAllTimers();
      expect(t.onDismiss).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("detach cancels a pending Tab dismissal", () => {
    vi.useFakeTimers();
    try {
      const t = setup({ onFocusOut: true });
      t.doc.emit("keydown", { key: "Tab", target: MENU_A });
      t.detach();
      (t.doc as unknown as { activeElement: unknown }).activeElement = OUT;
      vi.runAllTimers();
      expect(t.onDismiss).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("a Tab pressed outside the panel never dismisses", () => {
    vi.useFakeTimers();
    try {
      const t = setup({ onFocusOut: true });
      t.doc.emit("keydown", { key: "Tab", target: OUT });
      (t.doc as unknown as { activeElement: unknown }).activeElement = OUT;
      vi.runAllTimers();
      expect(t.onDismiss).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });
});
