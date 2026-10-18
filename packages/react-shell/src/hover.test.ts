import { afterEach, describe, expect, it, vi } from "vitest";
import { installHoverGuard } from "./hover.js";

/**
 * Fixture contract: stubbed window/document/root with captured listeners; events are plain
 * objects carrying `isTrusted` (and `relatedTarget` for pointerout), dispatched by type.
 */
function fixture() {
  const attrs = new Map<string, string>();
  const root = {
    setAttribute: (n: string, v: string) => void attrs.set(n, v),
    removeAttribute: (n: string) => void attrs.delete(n),
  };
  const winListeners = new Map<string, (e: unknown) => void>();
  const docListeners = new Map<string, (e: unknown) => void>();
  const win = {
    addEventListener: (t: string, l: (e: unknown) => void) => void winListeners.set(t, l),
    removeEventListener: (t: string, l: (e: unknown) => void) => void (winListeners.get(t) === l && winListeners.delete(t)),
  };
  const doc = {
    documentElement: root,
    visibilityState: "visible",
    addEventListener: (t: string, l: (e: unknown) => void) => void docListeners.set(t, l),
    removeEventListener: (t: string, l: (e: unknown) => void) => void (docListeners.get(t) === l && docListeners.delete(t)),
  };
  vi.stubGlobal("window", win);
  vi.stubGlobal("document", doc);
  return {
    attrs,
    doc,
    install: () => installHoverGuard(root as unknown as HTMLElement),
    winEvent: (t: string, e: unknown = {}) => winListeners.get(t)?.(e),
    docEvent: (t: string, e: unknown = {}) => docListeners.get(t)?.(e),
    winListeners,
    docListeners,
  };
}

afterEach(() => vi.unstubAllGlobals());

describe("installHoverGuard trusted movement", () => {
  it("suppresses initially, ignores synthetic movement, enables on trusted movement", () => {
    const f = fixture();
    f.install();
    expect(f.attrs.get("data-rk-pointer")).toBe("off");
    f.winEvent("pointermove", { isTrusted: false });
    expect(f.attrs.get("data-rk-pointer")).toBe("off");
    f.winEvent("pointermove", { isTrusted: true });
    expect(f.attrs.get("data-rk-pointer")).toBe("on");
  });

  it("rearms on blur, leave and hide until next trusted move", () => {
    const f = fixture();
    f.install();
    f.winEvent("pointermove", { isTrusted: true });
    f.winEvent("blur");
    expect(f.attrs.get("data-rk-pointer")).toBe("off");
    f.winEvent("pointermove", { isTrusted: true });
    expect(f.attrs.get("data-rk-pointer")).toBe("on");
    f.docEvent("pointerout", { relatedTarget: {} });
    expect(f.attrs.get("data-rk-pointer")).toBe("on");
    f.docEvent("pointerout", { relatedTarget: null });
    expect(f.attrs.get("data-rk-pointer")).toBe("off");
    f.winEvent("pointermove", { isTrusted: true });
    f.doc.visibilityState = "hidden";
    f.docEvent("visibilitychange");
    expect(f.attrs.get("data-rk-pointer")).toBe("off");
  });

  it("detach removes attribute and listeners", () => {
    const f = fixture();
    const detach = f.install();
    detach();
    expect(f.attrs.has("data-rk-pointer")).toBe(false);
    expect(f.winListeners.size).toBe(0);
    expect(f.docListeners.size).toBe(0);
  });
});
