import { describe, expect, it, vi } from "vitest";
import { attachDismiss, type DismissConfig } from "./dismiss.js";

function fakeTarget() {
  const handlers: Array<{ type: string; fn: (e: any) => void; capture: boolean }> = [];
  return {
    addEventListener: (type: string, fn: (e: any) => void, capture = false) => void handlers.push({ type, fn, capture: !!capture }),
    removeEventListener: (type: string, fn: (e: any) => void, capture = false) => {
      const i = handlers.findIndex((h) => h.type === type && h.fn === fn && h.capture === !!capture);
      if (i >= 0) handlers.splice(i, 1);
    },
    emit: (type: string, ev: Record<string, unknown> = {}) => {
      const e = { preventDefault: vi.fn(), stopPropagation: vi.fn(), ...ev };
      for (const h of [...handlers]) if (h.type === type) h.fn(e);
      return e as { preventDefault: ReturnType<typeof vi.fn>; stopPropagation: ReturnType<typeof vi.fn> };
    },
    count: () => handlers.length,
    handlers,
  };
}

const IN = { in: true };
const OUT = { out: true };
const BACKDROP = { backdrop: true };

function setup(over: Partial<DismissConfig> = {}) {
  const win = fakeTarget();
  const doc = fakeTarget();
  const onDismiss = vi.fn();
  const config: DismissConfig = {
    onDismiss,
    escape: true,
    escapeTarget: null,
    preventDefault: true,
    stopPropagation: true,
    imeGuard: true,
    outside: false,
    outsideContextMenu: false,
    isOutside: (t) => t === OUT || t === BACKDROP,
    onWindowBlur: false,
    onResize: false,
    onScroll: false,
    ...over,
  };
  const detach = attachDismiss(config, { win, doc });
  return { win, doc, onDismiss, detach };
}

describe("attachDismiss Escape", () => {
  it("dismisses on a window capture-phase Escape, calling preventDefault and stopPropagation by default", () => {
    const t = setup();
    expect(t.win.handlers[0]).toMatchObject({ type: "keydown", capture: true });
    const e = t.win.emit("keydown", { key: "Escape" });
    expect(t.onDismiss).toHaveBeenCalledWith("escape", expect.anything());
    expect(e.preventDefault).toHaveBeenCalled();
    expect(e.stopPropagation).toHaveBeenCalled();
  });

  it("can leave the event untouched", () => {
    const t = setup({ preventDefault: false, stopPropagation: false });
    const e = t.win.emit("keydown", { key: "Escape" });
    expect(t.onDismiss).toHaveBeenCalled();
    expect(e.preventDefault).not.toHaveBeenCalled();
    expect(e.stopPropagation).not.toHaveBeenCalled();
  });

  it("stopPropagation only", () => {
    const t = setup({ preventDefault: false });
    const e = t.win.emit("keydown", { key: "Escape" });
    expect(e.stopPropagation).toHaveBeenCalled();
    expect(e.preventDefault).not.toHaveBeenCalled();
  });

  it("ignores other keys and, with the IME guard, composing Escape (isComposing and keyCode 229)", () => {
    const t = setup();
    t.win.emit("keydown", { key: "Enter" });
    t.win.emit("keydown", { key: "Escape", isComposing: true });
    t.win.emit("keydown", { key: "Escape", keyCode: 229 });
    expect(t.onDismiss).not.toHaveBeenCalled();
    const off = setup({ imeGuard: false });
    off.win.emit("keydown", { key: "Escape", isComposing: true });
    expect(off.onDismiss).toHaveBeenCalledTimes(1);
  });

  it("listens on an element in the bubble phase when given a target, not on the window", () => {
    const el = fakeTarget();
    const t = setup({ escapeTarget: el });
    expect(t.win.count()).toBe(0);
    expect(el.handlers[0]).toMatchObject({ type: "keydown", capture: false });
    el.emit("keydown", { key: "Escape" });
    expect(t.onDismiss).toHaveBeenCalledWith("escape", expect.anything());
  });

  it("escape false attaches nothing", () => {
    const t = setup({ escape: false });
    expect(t.win.count() + t.doc.count()).toBe(0);
  });
});

describe("attachDismiss outside press", () => {
  it("mousedown: dismisses when the press lands outside, not inside", () => {
    const t = setup({ outside: "mousedown" });
    t.doc.emit("mousedown", { target: IN });
    expect(t.onDismiss).not.toHaveBeenCalled();
    t.doc.emit("mousedown", { target: OUT });
    expect(t.onDismiss).toHaveBeenCalledWith("outside", expect.anything());
    t.doc.emit("click", { target: OUT });
    expect(t.onDismiss).toHaveBeenCalledTimes(1);
  });

  it("click: needs press and release outside; a drag from inside or to inside does not dismiss", () => {
    const t = setup({ outside: "click" });
    t.doc.emit("mousedown", { target: OUT });
    t.doc.emit("mouseup", { target: OUT });
    t.doc.emit("click", { target: OUT });
    expect(t.onDismiss).toHaveBeenCalledTimes(1);
    t.doc.emit("mousedown", { target: IN });
    t.doc.emit("mouseup", { target: OUT });
    t.doc.emit("click", { target: OUT });
    t.doc.emit("mousedown", { target: OUT });
    t.doc.emit("mouseup", { target: IN });
    t.doc.emit("click", { target: OUT });
    expect(t.onDismiss).toHaveBeenCalledTimes(1);
    // A click with no recorded press (the click that opened the layer) never dismisses.
    t.doc.emit("click", { target: OUT });
    expect(t.onDismiss).toHaveBeenCalledTimes(1);
  });

  it("click-target: any click whose target is outside dismisses, even after a drag from inside", () => {
    const t = setup({ outside: "click-target" });
    t.doc.emit("mousedown", { target: IN });
    t.doc.emit("mouseup", { target: OUT });
    t.doc.emit("click", { target: BACKDROP });
    expect(t.onDismiss).toHaveBeenCalledTimes(1);
    t.doc.emit("click", { target: IN });
    expect(t.onDismiss).toHaveBeenCalledTimes(1);
    expect(t.doc.handlers.some((h) => h.type === "mousedown")).toBe(false);
  });

  it("outside context menu is suppressed and dismisses; inside is left alone", () => {
    const t = setup({ outsideContextMenu: true });
    const inside = t.doc.emit("contextmenu", { target: IN });
    expect(inside.preventDefault).not.toHaveBeenCalled();
    const e = t.doc.emit("contextmenu", { target: BACKDROP });
    expect(e.preventDefault).toHaveBeenCalled();
    expect(t.onDismiss).toHaveBeenCalledTimes(1);
  });
});

describe("attachDismiss window flags and cleanup", () => {
  it("blur, resize and scroll are off by default", () => {
    const t = setup();
    t.win.emit("blur");
    t.win.emit("resize");
    t.win.emit("scroll", { target: OUT });
    expect(t.onDismiss).not.toHaveBeenCalled();
  });

  it("each flag dismisses with its own reason; scroll inside the panel is ignored", () => {
    const t = setup({ onWindowBlur: true, onResize: true, onScroll: true, isInsideScroll: (x) => x === IN });
    t.win.emit("blur");
    t.win.emit("resize");
    t.win.emit("scroll", { target: IN });
    t.win.emit("scroll", { target: OUT });
    expect(t.onDismiss.mock.calls.map((c) => c[0])).toEqual(["blur", "resize", "scroll"]);
    expect(t.win.handlers.find((h) => h.type === "scroll")?.capture).toBe(true);
  });

  it("removes every listener on detach, and a second detach is harmless", () => {
    const t = setup({ outside: "click", outsideContextMenu: true, onWindowBlur: true, onResize: true, onScroll: true });
    expect(t.win.count() + t.doc.count()).toBeGreaterThan(4);
    t.detach();
    expect(t.win.count() + t.doc.count()).toBe(0);
    t.detach();
    t.win.emit("keydown", { key: "Escape" });
    expect(t.onDismiss).not.toHaveBeenCalled();
  });
});
