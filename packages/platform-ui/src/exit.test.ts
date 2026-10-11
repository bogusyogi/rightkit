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

  it("drop (default): an action passed while closing or pending is not run", async () => {
    const a = make();
    void a.core.close();
    const fn = vi.fn();
    await a.core.close(fn);
    expect(fn).not.toHaveBeenCalled();

    const b = make({ closeAfter: "settled" });
    let resolve!: () => void;
    const first = b.core.close(() => new Promise<void>((r) => (resolve = r)));
    await b.core.close(fn);
    expect(fn).not.toHaveBeenCalled();
    resolve();
    await first;
  });

  it("run: the action runs while closing or pending, and the close is not repeated", async () => {
    const a = make({ onActionWhileClosing: "run" });
    void a.core.close(() => undefined);
    const fn = vi.fn();
    await a.core.close(fn);
    expect(fn).toHaveBeenCalledTimes(1);
    expect(a.onExitStart).toHaveBeenCalledTimes(1);

    const b = make({ closeAfter: "settled", onActionWhileClosing: "run" });
    let resolve!: () => void;
    const first = b.core.close(() => new Promise<void>((r) => (resolve = r)));
    await b.core.close(fn);
    expect(fn).toHaveBeenCalledTimes(2);
    resolve();
    await first;
    expect(b.onExitStart).toHaveBeenCalledTimes(1);
  });

  it("run: an action after the exit finished (closed) is still dropped", async () => {
    const a = make({ onActionWhileClosing: "run", exitMs: 0 });
    void a.core.close();
    const fn = vi.fn();
    await a.core.close(fn);
    expect(fn).not.toHaveBeenCalled();
  });

  describe("onSyncThrow", () => {
    const boom = () => {
      throw new Error("sync");
    };
    for (const closeAfter of ["immediately", "settled"] as const) {
      it(`close (default), closeAfter ${closeAfter}: a synchronous throw becomes a rejected promise and the exit still starts`, async () => {
        const { core, onExitStart } = make({ closeAfter });
        let p!: Promise<void>;
        expect(() => (p = core.close(boom))).not.toThrow();
        await expect(p).rejects.toThrow("sync");
        expect(core.getState()).toEqual({ phase: "closing", pending: false });
        expect(onExitStart).toHaveBeenCalledTimes(1);
      });

      it(`propagate, closeAfter ${closeAfter}: the throw reaches the caller of close(), nothing starts and state is unchanged`, () => {
        const { core, onExitStart } = make({ closeAfter, onSyncThrow: "propagate" });
        const seen: string[] = [];
        core.subscribe(() => seen.push(core.getState().phase));
        expect(() => core.close(boom)).toThrow("sync");
        expect(core.getState()).toEqual({ phase: "open", pending: false });
        expect(onExitStart).not.toHaveBeenCalled();
        expect(seen).toEqual([]);
        expect(vi.getTimerCount()).toBe(0);
        // still usable afterwards
        void core.close(() => undefined);
        expect(core.getState().phase === "closing" || core.getState().pending).toBe(true);
      });

      it(`propagate, closeAfter ${closeAfter}: an asynchronous rejection behaves as in close mode`, async () => {
        const { core } = make({ closeAfter, onSyncThrow: "propagate" });
        await expect(core.close(() => Promise.reject(new Error("async")))).rejects.toThrow("async");
        expect(core.getState().phase).toBe("closing");
      });

      it(`propagate, closeAfter ${closeAfter}: a normal action still closes and resolves`, async () => {
        const { core, onExitStart } = make({ closeAfter, onSyncThrow: "propagate" });
        await core.close(() => undefined);
        expect(core.getState().phase).toBe("closing");
        expect(onExitStart).toHaveBeenCalledTimes(1);
      });
    }

    it("propagate: with the action-while-closing run option, a synchronous throw also reaches the caller", () => {
      const { core } = make({ onSyncThrow: "propagate", onActionWhileClosing: "run" });
      void core.close();
      expect(() => core.close(boom)).toThrow("sync");
    });
  });
});
