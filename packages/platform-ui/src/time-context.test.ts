import { describe, expect, it, vi } from "vitest";
import { blockNativeContextMenu } from "./context-menu.js";
import { relativeTimeFrom } from "./time.js";

const NOW = 1_000_000_000_000;
const ago = (seconds: number) => NOW - seconds * 1000;

describe("relativeTimeFrom", () => {
  it("uses the ViewRight thresholds and singular/plural forms", () => {
    expect(relativeTimeFrom(ago(0), NOW)).toBe("just now");
    expect(relativeTimeFrom(ago(59), NOW)).toBe("just now");
    expect(relativeTimeFrom(ago(60), NOW)).toBe("1 minute ago");
    expect(relativeTimeFrom(ago(150), NOW)).toBe("3 minutes ago");
    expect(relativeTimeFrom(ago(3600), NOW)).toBe("1 hour ago");
    expect(relativeTimeFrom(ago(7200), NOW)).toBe("2 hours ago");
    expect(relativeTimeFrom(ago(86400), NOW)).toBe("1 day ago");
    expect(relativeTimeFrom(ago(3 * 86400), NOW)).toBe("3 days ago");
  });

  it("reads a future timestamp as just now", () => {
    expect(relativeTimeFrom(NOW + 5000, NOW)).toBe("just now");
  });

  it("defaults now to Date.now()", () => {
    vi.spyOn(Date, "now").mockReturnValue(NOW);
    expect(relativeTimeFrom(ago(120))).toBe("2 minutes ago");
    vi.restoreAllMocks();
  });
});

describe("blockNativeContextMenu", () => {
  function fakeDoc() {
    const handlers: Array<{ type: string; fn: (e: Event) => void; capture: unknown }> = [];
    return {
      handlers,
      addEventListener: (type: string, fn: (e: Event) => void, capture: unknown) => void handlers.push({ type, fn, capture }),
      removeEventListener: (type: string, fn: (e: Event) => void) => {
        const i = handlers.findIndex((h) => h.type === type && h.fn === fn);
        if (i >= 0) handlers.splice(i, 1);
      },
    };
  }

  it("prevents the native menu in the capture phase and removes the listener on uninstall", () => {
    const doc = fakeDoc();
    const uninstall = blockNativeContextMenu(doc);
    expect(doc.handlers).toHaveLength(1);
    expect(doc.handlers[0].capture).toBe(true);
    const event = { preventDefault: vi.fn() } as unknown as Event;
    doc.handlers[0].fn(event);
    expect(event.preventDefault).toHaveBeenCalled();
    uninstall();
    expect(doc.handlers).toHaveLength(0);
  });

  it("is idempotent per document and a no-op without one", () => {
    const doc = fakeDoc();
    const first = blockNativeContextMenu(doc);
    const second = blockNativeContextMenu(doc);
    expect(second).toBe(first);
    expect(doc.handlers).toHaveLength(1);
    first();
    expect(() => blockNativeContextMenu(undefined)()).not.toThrow();
  });
});
