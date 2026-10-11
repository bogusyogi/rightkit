import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createExitTransition, type ExitTransitionOptions } from "./exit.js";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

const make = (over: ExitTransitionOptions = {}) => {
  const onClosed = vi.fn();
  const onExitStart = vi.fn();
  const core = createExitTransition(() => ({ onClosed, onExitStart, ...over }));
  return { core, onClosed, onExitStart };
};

describe("createExitTransition", () => {
  it("open -> closing -> closed after exitMs (default 80)", async () => {
    const { core, onClosed, onExitStart } = make();
    expect(core.getState()).toEqual({ phase: "open", pending: false });
    void core.close();
    expect(core.getState().phase).toBe("closing");
    expect(onExitStart).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(79);
    expect(onClosed).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(core.getState().phase).toBe("closed");
    expect(onClosed).toHaveBeenCalledTimes(1);
  });

  it("exitMs <= 0 finishes at once", () => {
    const { core, onClosed } = make({ exitMs: 0 });
    void core.close();
    expect(core.getState().phase).toBe("closed");
    expect(onClosed).toHaveBeenCalledTimes(1);
  });

  it("initialOpen false starts closed", () => {
    expect(make({ initialOpen: false }).core.getState().phase).toBe("closed");
  });

  it("a second close while closing or closed does nothing", () => {
    const { core, onExitStart } = make();
    void core.close();
    void core.close();
    expect(onExitStart).toHaveBeenCalledTimes(1);
  });

  it("open() during the exit starts a fresh instance: the old timer cannot close it", () => {
    const { core, onClosed } = make({ exitMs: 100 });
    void core.close();
    vi.advanceTimersByTime(60);
    core.open();
    expect(core.getState().phase).toBe("open");
    vi.advanceTimersByTime(500);
    expect(core.getState().phase).toBe("open");
    expect(onClosed).not.toHaveBeenCalled();
    void core.close();
    vi.advanceTimersByTime(100);
    expect(onClosed).toHaveBeenCalledTimes(1);
  });

  it("immediately: the exit starts, then the action runs synchronously", () => {
    const order: string[] = [];
    const { core } = make({ onExitStart: () => order.push("exit") });
    void core.close(() => order.push("action"));
    expect(order).toEqual(["exit", "action"]);
  });

  it("immediately: an action's rejection surfaces on the returned promise; the exit still ran", async () => {
    const { core } = make();
    await expect(core.close(() => Promise.reject(new Error("boom")))).rejects.toThrow("boom");
    expect(core.getState().phase).toBe("closing");
  });

  it("settled: stays open and pending until the promise settles, then exits", async () => {
    const { core, onExitStart } = make({ closeAfter: "settled" });
    let resolve!: () => void;
    const done = core.close(() => new Promise<void>((r) => (resolve = r)));
    expect(core.getState()).toEqual({ phase: "open", pending: true });
    expect(onExitStart).not.toHaveBeenCalled();
    void core.close();
    resolve();
    await done;
    expect(core.getState()).toEqual({ phase: "closing", pending: false });
    vi.advanceTimersByTime(80);
    expect(core.getState().phase).toBe("closed");
  });

  it("settled: a rejected action still closes, and the rejection reaches the caller", async () => {
    const { core } = make({ closeAfter: "settled" });
    await expect(core.close(() => Promise.reject(new Error("no")))).rejects.toThrow("no");
    expect(core.getState().phase).toBe("closing");
  });

  it("settled: a synchronous action closes after a microtask", async () => {
    const { core } = make({ closeAfter: "settled" });
    const done = core.close(() => undefined);
    await done;
    expect(core.getState().phase).toBe("closing");
  });

  it("settled: a stale settle after open() does not close the new instance", async () => {
    const { core, onExitStart } = make({ closeAfter: "settled" });
    let resolve!: () => void;
    const done = core.close(() => new Promise<void>((r) => (resolve = r)));
    core.open();
    resolve();
    await done;
    expect(core.getState()).toEqual({ phase: "open", pending: false });
    expect(onExitStart).not.toHaveBeenCalled();
  });

  it("settled without an action closes like immediately", () => {
    const { core } = make({ closeAfter: "settled" });
    void core.close();
    expect(core.getState().phase).toBe("closing");
  });

  it("dispose clears the timer and orphans an in-flight settle", async () => {
    const a = make({ exitMs: 100 });
    void a.core.close();
    a.core.dispose();
    vi.advanceTimersByTime(1000);
    expect(a.onClosed).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);

    const b = make({ closeAfter: "settled" });
    let resolve!: () => void;
    const done = b.core.close(() => new Promise<void>((r) => (resolve = r)));
    b.core.dispose();
    resolve();
    await done;
    expect(b.onExitStart).not.toHaveBeenCalled();
  });

  it("notifies subscribers on change only, and unsubscribes", () => {
    const { core } = make();
    const seen: string[] = [];
    const off = core.subscribe(() => seen.push(core.getState().phase));
    core.open();
    void core.close();
    vi.advanceTimersByTime(80);
    off();
    core.open();
    expect(seen).toEqual(["closing", "closed"]);
  });

  it("reads options at call time, so exitMs can follow props", () => {
    let ms = 50;
    const core = createExitTransition(() => ({ exitMs: ms }));
    ms = 200;
    void core.close();
    vi.advanceTimersByTime(199);
    expect(core.getState().phase).toBe("closing");
    vi.advanceTimersByTime(1);
    expect(core.getState().phase).toBe("closed");
  });
});
