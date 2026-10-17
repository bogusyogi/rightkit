import { describe, expect, it, vi } from "vitest";
import { createToastBus, type ToastEvent } from "./toast.js";

function collect(bus: ReturnType<typeof createToastBus>) {
  const events: ToastEvent[] = [];
  bus.subscribe((event) => events.push(event));
  return events;
}

describe("createToastBus replace-by-key", () => {
  it("dismisses the live toast with the same id, then pushes the new one", () => {
    const bus = createToastBus();
    const events = collect(bus);
    const first = bus.push("Saving", { id: "save" });
    const second = bus.push("Saved", { id: "save", kind: "success" });
    expect(events.map((e) => e.type)).toEqual(["push", "dismiss", "push"]);
    expect(events[1]).toEqual({ type: "dismiss", id: first });
    expect(events[2]).toMatchObject({ type: "push", toast: { id: second, message: "Saved", key: "save" } });
  });

  it("does not replace a toast with a different key or none", () => {
    const bus = createToastBus();
    const events = collect(bus);
    bus.push("A", { id: "a" });
    bus.push("B", { id: "b" });
    bus.push("C");
    expect(events.filter((e) => e.type === "dismiss")).toHaveLength(0);
  });

  it("forgets a key once dismissed, so the next push with it does not dismiss anything", () => {
    const bus = createToastBus();
    const events = collect(bus);
    const id = bus.push("A", { id: "a" });
    bus.dismiss(id);
    bus.push("A again", { id: "a" });
    expect(events.map((e) => e.type)).toEqual(["push", "dismiss", "push"]);
  });
});

describe("createToastBus dedupe", () => {
  it("returns the live id for an identical plain toast and emits nothing", () => {
    const bus = createToastBus({ dedupe: true });
    const events = collect(bus);
    const id = bus.push("Copied", "success");
    expect(bus.push("Copied", "success")).toBe(id);
    expect(events.filter((e) => e.type === "push")).toHaveLength(1);
  });

  it("keeps different kinds and messages apart", () => {
    const bus = createToastBus({ dedupe: true });
    const events = collect(bus);
    bus.push("Copied", "success");
    bus.push("Copied", "error");
    bus.push("Other", "success");
    expect(events.filter((e) => e.type === "push")).toHaveLength(3);
  });

  it("never dedupes a toast with an action, or a keyed toast", () => {
    const bus = createToastBus({ dedupe: true });
    const events = collect(bus);
    const action = { label: "Undo", run: vi.fn() };
    bus.push("Deleted", { action });
    bus.push("Deleted", { action });
    bus.push("Keyed", { id: "k" });
    bus.push("Keyed", { id: "k" });
    expect(events.filter((e) => e.type === "push")).toHaveLength(4);
  });

  it("dedupes again once the earlier toast is dismissed", () => {
    const bus = createToastBus({ dedupe: true });
    const events = collect(bus);
    const id = bus.push("Copied");
    bus.dismiss(id);
    bus.push("Copied");
    expect(events.filter((e) => e.type === "push")).toHaveLength(2);
  });

  it("is off by default", () => {
    const bus = createToastBus();
    const events = collect(bus);
    bus.push("Copied");
    bus.push("Copied");
    expect(events.filter((e) => e.type === "push")).toHaveLength(2);
  });
});

describe("createToastBus max", () => {
  it("dismisses the oldest live toast when a push exceeds max", () => {
    const bus = createToastBus({ max: 2 });
    const events = collect(bus);
    const a = bus.push("a");
    const b = bus.push("b");
    const c = bus.push("c");
    expect(events.slice(2)).toEqual([
      { type: "dismiss", id: a },
      expect.objectContaining({ type: "push", toast: expect.objectContaining({ id: c }) }),
    ]);
    expect(b).toBeLessThan(c);
  });

  it("counts only live toasts", () => {
    const bus = createToastBus({ max: 2 });
    const events = collect(bus);
    const a = bus.push("a");
    bus.dismiss(a);
    bus.push("b");
    bus.push("c");
    expect(events.filter((e) => e.type === "dismiss")).toEqual([{ type: "dismiss", id: a }]);
  });

  it("is unlimited by default", () => {
    const bus = createToastBus();
    const events = collect(bus);
    for (let i = 0; i < 10; i++) bus.push(`t${i}`);
    expect(events.filter((e) => e.type === "dismiss")).toHaveLength(0);
  });
});

describe("createToastBus dismiss bookkeeping", () => {
  it("dismiss frees the dedupe slot and is idempotent", () => {
    const bus = createToastBus({ dedupe: true });
    const events = collect(bus);
    const a = bus.push("Same");
    expect(bus.push("Same")).toBe(a);
    bus.dismiss(a);
    bus.dismiss(a);
    expect(events.map((e) => e.type)).toEqual(["push", "dismiss"]);
    expect(bus.push("Same")).not.toBe(a);
  });

  it("dismiss drops the key so a later keyed push does not emit a stale dismiss", () => {
    const bus = createToastBus();
    const events = collect(bus);
    bus.push("A", { id: "k" });
    bus.dismiss(1);
    bus.push("B", { id: "k" });
    expect(events.map((e) => e.type)).toEqual(["push", "dismiss", "push"]);
  });
});
