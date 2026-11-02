import { afterEach, describe, expect, it, vi } from "vitest";

import { STREAM_DELTA_FLUSH_MS, StreamDeltaBatcher } from "./streamDeltaBatcher.js";

const flushNow = async () => {
  await vi.advanceTimersByTimeAsync(STREAM_DELTA_FLUSH_MS);
};

describe("StreamDeltaBatcher", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("coalesces adjacent deltas per session without changing their order", async () => {
    vi.useFakeTimers();
    const batches: Array<{ sessionId: string; texts: string[]; types: string[] }> = [];
    const batcher = new StreamDeltaBatcher({
      onFlush: (sessionId, deltas) =>
        batches.push({
          sessionId,
          texts: deltas.map((delta) => delta.text),
          types: deltas.map((delta) => delta.type),
        }),
    });

    batcher.enqueue("s1", "assistant", "one", 1);
    batcher.enqueue("s1", "assistant", " two", 2);
    batcher.enqueue("s1", "reasoning", "think", 3);
    batcher.enqueue("s2", "assistant", "other", 4);
    await flushNow();

    expect(batches).toEqual([
      { sessionId: "s1", texts: ["one two", "think"], types: ["assistant", "reasoning"] },
      { sessionId: "s2", texts: ["other"], types: ["assistant"] },
    ]);
  });

  it("delivers on a bounded timer even when animation frames are parked", async () => {
    vi.useFakeTimers();
    vi.spyOn(window, "requestAnimationFrame").mockImplementation(() => 1);
    const received: string[] = [];
    const batcher = new StreamDeltaBatcher({
      onFlush: (_sessionId, deltas) => received.push(deltas.map((delta) => delta.text).join("")),
    });

    batcher.enqueue("s1", "assistant", "still visible");
    expect(received).toEqual([]);
    await flushNow();

    expect(received).toEqual(["still visible"]);
    expect(window.requestAnimationFrame).toHaveBeenCalled();
  });

  it("flushes a terminal session immediately and does not duplicate the timer delivery", async () => {
    vi.useFakeTimers();
    const received: string[] = [];
    const batcher = new StreamDeltaBatcher({
      onFlush: (_sessionId, deltas) => received.push(deltas.map((delta) => delta.text).join("")),
    });

    batcher.enqueue("s1", "assistant", "final chunk");
    batcher.flush("s1");
    await vi.advanceTimersByTimeAsync(STREAM_DELTA_FLUSH_MS * 2);

    expect(received).toEqual(["final chunk"]);
    expect(batcher.pending).toBe(false);
  });

  it("flushes queued text when visibility or focus returns", () => {
    vi.useFakeTimers();
    const received: string[] = [];
    const batcher = new StreamDeltaBatcher({
      onFlush: (_sessionId, deltas) => received.push(deltas.map((delta) => delta.text).join("")),
    });
    const detach = batcher.attachLifecycleListeners(window);

    batcher.enqueue("s1", "assistant", "caught up");
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    document.dispatchEvent(new Event("visibilitychange"));

    expect(received).toEqual(["caught up"]);
    detach();
  });

  it("adapts the next gap to an expensive render frame", async () => {
    vi.useFakeTimers();
    let now = 1000;
    const frameCallbacks: FrameRequestCallback[] = [];
    vi.spyOn(performance, "now").mockImplementation(() => now);
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      frameCallbacks.push(callback);
      return frameCallbacks.length;
    });
    const received: string[] = [];
    const batcher = new StreamDeltaBatcher({
      onFlush: (_sessionId, deltas) => received.push(deltas.map((delta) => delta.text).join("")),
    });

    batcher.enqueue("s1", "assistant", "first");
    await vi.advanceTimersByTimeAsync(0);
    now = 1100;
    frameCallbacks.shift()?.(1040);

    batcher.enqueue("s1", "assistant", "second");
    await vi.advanceTimersByTimeAsync(79);
    expect(received).toEqual(["first"]);
    await vi.advanceTimersByTimeAsync(1);

    expect(received).toEqual(["first", "second"]);
  });

  it("flushes and cancels future work on dispose", async () => {
    vi.useFakeTimers();
    const received: string[] = [];
    const batcher = new StreamDeltaBatcher({
      onFlush: (_sessionId, deltas) => received.push(deltas.map((delta) => delta.text).join("")),
    });

    batcher.enqueue("s1", "assistant", "last");
    batcher.dispose();
    await vi.advanceTimersByTimeAsync(STREAM_DELTA_FLUSH_MS * 2);
    batcher.dispose();

    expect(received).toEqual(["last"]);
    expect(batcher.pending).toBe(false);
  });
});
