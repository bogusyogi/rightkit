import { describe, expect, it, vi } from "vitest";
import { attachFocusTrap } from "./focus.js";

type Item = { name: string; rects: number; focus(): void; hasAttribute(a: string): boolean; getClientRects(): unknown[] };

function setup(items: Array<{ name: string; rects: number }>) {
  const doc: { activeElement: unknown; listeners: Map<string, { fn: (e: unknown) => void; capture: boolean }>; addEventListener: (...a: unknown[]) => void; removeEventListener: (...a: unknown[]) => void } = {
    activeElement: null,
    listeners: new Map(),
    addEventListener(type: unknown, fn: unknown, capture: unknown) {
      this.listeners.set(String(type), { fn: fn as (e: unknown) => void, capture: capture === true });
    },
    removeEventListener(type: unknown) {
      this.listeners.delete(String(type));
    },
  };
  const nodes: Item[] = items.map((i) => ({
    name: i.name,
    rects: i.rects,
    focus() {
      doc.activeElement = this;
    },
    hasAttribute: () => false,
    getClientRects() {
      return Array.from({ length: this.rects }, () => ({}));
    },
  }));
  const elListeners = new Map<string, unknown>();
  const el = {
    ownerDocument: doc,
    isConnected: true,
    querySelectorAll: () => nodes,
    contains: (n: unknown) => nodes.includes(n as Item) || n === el,
    addEventListener: (t: string, fn: unknown) => void elListeners.set(t, fn),
    removeEventListener: (t: string) => void elListeners.delete(t),
    focus() {
      doc.activeElement = el;
    },
  };
  const opener = { focus: vi.fn() };
  const attach = (over: Record<string, unknown> = {}) =>
    attachFocusTrap(el as unknown as HTMLElement, {
      autoFocus: true,
      trap: true,
      restore: true,
      restoreTiming: "sync",
      restoreWhen: "inactive",
      captureOpener: "render",
      fromOutside: "both",
      skipHidden: true,
      emptyList: "prevent",
      focusFallback: "container",
      opener: { current: opener as unknown as HTMLElement },
      isActive: () => true,
      ...over,
    } as never);
  const tab = (shift = false) => {
    const e = { key: "Tab", shiftKey: shift, preventDefault: vi.fn() };
    (doc.listeners.get("keydown")?.fn ?? elListeners.get("keydown") as ((e: unknown) => void))(e);
    return e;
  };
  return { doc, el, nodes, attach, tab, elListeners, opener };
}

describe("attachFocusTrap scope and skipUnrendered", () => {
  it("default scope keeps the Tab listener on the container, not the document", () => {
    const t = setup([{ name: "a", rects: 1 }]);
    t.attach();
    expect(t.elListeners.has("keydown")).toBe(true);
    expect(t.doc.listeners.has("keydown")).toBe(false);
  });

  it("scope document listens on the document in capture phase and removes it on detach", () => {
    const t = setup([{ name: "a", rects: 1 }, { name: "b", rects: 1 }]);
    const detach = t.attach({ scope: "document" });
    expect(t.elListeners.has("keydown")).toBe(false);
    expect(t.doc.listeners.get("keydown")?.capture).toBe(true);
    detach();
    expect(t.doc.listeners.has("keydown")).toBe(false);
  });

  it("with scope document, Tab wraps while focus sits on a backdrop outside the container", () => {
    const t = setup([{ name: "a", rects: 1 }, { name: "b", rects: 1 }]);
    t.attach({ scope: "document" });
    t.doc.activeElement = { backdrop: true };
    const e = t.tab();
    expect(e.preventDefault).toHaveBeenCalled();
    expect((t.doc.activeElement as Item).name).toBe("a");
  });

  it("skipUnrendered drops controls with no client rects from the wrap targets", () => {
    const t = setup([{ name: "gone", rects: 0 }, { name: "a", rects: 1 }, { name: "b", rects: 1 }, { name: "hidden", rects: 0 }]);
    t.attach({ skipUnrendered: true, autoFocus: true });
    expect((t.doc.activeElement as Item).name).toBe("a");
    t.nodes[2].focus();
    t.tab();
    expect((t.doc.activeElement as Item).name).toBe("a");
  });

  it("without skipUnrendered, controls with no rects still count (0.2.8 behaviour)", () => {
    const t = setup([{ name: "gone", rects: 0 }, { name: "a", rects: 1 }]);
    t.attach();
    expect((t.doc.activeElement as Item).name).toBe("gone");
  });
});

describe("stacked document-scoped traps", () => {
  it("only the topmost document trap handles Tab, and the lower one resumes after it detaches", () => {
    const lower = setup([{ name: "l1", rects: 1 }, { name: "l2", rects: 1 }]);
    const upper = setup([{ name: "u1", rects: 1 }, { name: "u2", rects: 1 }]);
    // Both traps share one document so they compete exactly as portaled dialogs do.
    (upper.el as { ownerDocument: unknown }).ownerDocument = lower.doc;
    const keydowns: Array<(e: unknown) => void> = [];
    lower.doc.addEventListener = (type: unknown, fn: unknown) => void (type === "keydown" && keydowns.push(fn as (e: unknown) => void));
    lower.doc.removeEventListener = (type: unknown, fn: unknown) => {
      const i = keydowns.indexOf(fn as (e: unknown) => void);
      if (type === "keydown" && i >= 0) keydowns.splice(i, 1);
    };
    lower.attach({ scope: "document" });
    const detachUpper = upper.attach({ scope: "document" });
    expect(keydowns).toHaveLength(2);
    // Focus sits in the upper dialog's middle: the lower trap would treat it as outside and redirect.
    (lower.doc as { activeElement: unknown }).activeElement = upper.nodes[0];
    const tab = () => {
      const e = { key: "Tab", shiftKey: false, preventDefault: vi.fn() };
      for (const fn of [...keydowns]) fn(e);
      return e;
    };
    tab();
    expect(lower.nodes.includes((lower.doc as { activeElement: never }).activeElement)).toBe(false);
    detachUpper();
    (lower.doc as { activeElement: unknown }).activeElement = lower.nodes[1];
    const e = tab();
    expect(e.preventDefault).toHaveBeenCalled();
    expect((lower.doc as { activeElement: unknown }).activeElement).toBe(lower.nodes[0]);
  });
});
