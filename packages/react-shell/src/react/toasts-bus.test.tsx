import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createToastBus } from "../toast.js";

// Minimal hook harness: the vitest environment has no DOM, so ToastViewport is driven through a tiny fake dispatcher.
type Slot = { value?: unknown; deps?: unknown[]; cleanup?: (() => void) | void };
const h = vi.hoisted(() => ({ slots: [] as Array<Record<string, unknown>>, index: 0, effects: [] as Array<() => void>, dirty: false }));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  const same = (a?: unknown[], b?: unknown[]) => !!a && !!b && a.length === b.length && a.every((x, i) => Object.is(x, b[i]));
  return {
    ...actual,
    useState: (init: unknown) => {
      const i = h.index++;
      const slot = (h.slots[i] ??= { value: typeof init === "function" ? (init as () => unknown)() : init });
      return [slot.value, (next: unknown) => {
        slot.value = typeof next === "function" ? (next as (p: unknown) => unknown)(slot.value) : next;
        h.dirty = true;
      }];
    },
    useRef: (init: unknown) => (h.slots[h.index++] ??= { current: init }),
    useCallback: (fn: unknown, deps: unknown[]) => {
      const slot = (h.slots[h.index++] ??= {}) as Slot;
      if (!same(slot.deps, deps)) { slot.value = fn; slot.deps = deps; }
      return slot.value;
    },
    useEffect: (fn: () => (() => void) | void, deps: unknown[]) => {
      const slot = (h.slots[h.index++] ??= {}) as Slot;
      if (same(slot.deps, deps)) return;
      slot.deps = deps;
      h.effects.push(() => { slot.cleanup?.(); slot.cleanup = fn(); });
    },
  };
});

const { ToastViewport } = await import("./toasts.js");

type Props = Parameters<typeof ToastViewport>[0];
function mount(props: Props) {
  h.slots = [];
  const render = () => {
    let tree: { props: { children: Array<{ props: { toast: { id: number; message: string }; leaving: boolean } }> } };
    do {
      h.dirty = false;
      h.index = 0;
      tree = ToastViewport(props) as never;
      const run = h.effects.splice(0);
      for (const effect of run) effect();
    } while (h.dirty || h.effects.length);
    return tree.props.children.map((c) => ({ id: c.props.toast.id, message: c.props.toast.message, leaving: c.props.leaving }));
  };
  const unmount = () => {
    for (const slot of h.slots) (slot as Slot).cleanup?.();
  };
  return { render, unmount };
}

beforeEach(() => {
  vi.useFakeTimers();
  (globalThis as { window?: unknown }).window = globalThis;
});
afterEach(() => {
  vi.useRealTimers();
  delete (globalThis as { window?: unknown }).window;
});

describe("ToastViewport releases bus bookkeeping on every removal", () => {
  it("timer expiry frees the dedupe slot so the same message shows again", () => {
    const bus = createToastBus({ dedupe: true, ttl: { info: 1000 } });
    const v = mount({ bus, exitMs: 10 });
    v.render();
    const first = bus.push("Saved");
    expect(v.render()).toHaveLength(1);
    vi.advanceTimersByTime(1010);
    expect(v.render()).toHaveLength(0);
    const second = bus.push("Saved");
    expect(second).not.toBe(first);
    expect(v.render()).toHaveLength(1);
  });

  it("does not echo a recursive dismiss event to other subscribers", () => {
    const bus = createToastBus({ ttl: { info: 100 } });
    const events: string[] = [];
    const v = mount({ bus, exitMs: 10 });
    v.render();
    bus.subscribe((e) => events.push(e.type));
    bus.push("x");
    vi.advanceTimersByTime(200);
    v.render();
    expect(events).toEqual(["push", "dismiss"]);
  });

  it("eviction beyond maxVisible, and unmount, release the bus entries", () => {
    const bus = createToastBus({ dedupe: true, ttl: { info: 0 } });
    const v = mount({ bus, maxVisible: 1, exitMs: 10 });
    v.render();
    bus.push("a");
    bus.push("b");
    expect(v.render().map((r) => r.message)).toEqual(["b"]);
    const again = vi.fn();
    bus.subscribe(again);
    bus.push("a"); // "a" was evicted, so it is not live and must show again
    expect(again).toHaveBeenCalledWith(expect.objectContaining({ type: "push" }));
    v.unmount();
    const events: string[] = [];
    bus.subscribe((e) => events.push(e.type));
    bus.push("b");
    expect(events).toEqual(["push"]);
  });

  it("a dedupe replacement does not stay live in the bus", () => {
    const bus = createToastBus({ ttl: { info: 0 } });
    const v = mount({ bus, dedupe: "newest", exitMs: 10 });
    v.render();
    const first = bus.push("same");
    bus.push("same");
    expect(v.render()).toHaveLength(1);
    bus.dismiss(first);
    // Both ids were released: dismissing the replacement again is a no-op with no event.
    const spy = vi.fn();
    bus.subscribe(spy);
    bus.dismiss(first + 1);
    expect(spy).not.toHaveBeenCalled();
  });
});
