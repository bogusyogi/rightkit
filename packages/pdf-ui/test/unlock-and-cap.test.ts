import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PdfPasswordCancelledError, renderPageLimit } from "../src/index.js";
import { attachPasswordHandler, createLoadTimer, MAX_PASSWORD_ATTEMPTS } from "../src/password.js";

function fakeTask() {
  const task = { onPassword: undefined as unknown, destroyed: 0, destroy() { this.destroyed += 1; } };
  const ask = (update: (value: string) => void, code: number) => (task.onPassword as (u: typeof update, c: number) => void)(update, code);
  return { task, ask };
}
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("onPassword flow", () => {
  it("passes the reason and retries with the returned value", async () => {
    const { task, ask } = fakeTask();
    const seen: Array<[string, number]> = [];
    const flow = attachPasswordHandler(task, () => (reason, attempt) => { seen.push([reason, attempt]); return attempt === 1 ? "wrong" : "right"; });
    const updates: string[] = [];
    ask((value) => updates.push(value), 1);
    await flush();
    ask((value) => updates.push(value), 2);
    await flush();
    expect(seen).toEqual([["need", 1], ["incorrect", 2]]);
    expect(updates).toEqual(["wrong", "right"]);
    expect(flow.failure).toBeNull();
    expect(task.destroyed).toBe(0);
  });

  it("awaits an async handler", async () => {
    const { task, ask } = fakeTask();
    attachPasswordHandler(task, () => async () => "later");
    const updates: string[] = [];
    ask((value) => updates.push(value), 1);
    await flush();
    expect(updates).toEqual(["later"]);
  });

  it.each([null, undefined])("cancels cleanly when the handler returns %s", async (answer) => {
    const { task, ask } = fakeTask();
    const flow = attachPasswordHandler(task, () => () => answer);
    const updates: string[] = [];
    ask((value) => updates.push(value), 2);
    await flush();
    expect(updates).toEqual([]);
    expect(task.destroyed).toBe(1);
    expect(flow.failure).toBeInstanceOf(PdfPasswordCancelledError);
    expect((flow.failure as PdfPasswordCancelledError).reason).toBe("incorrect");
  });

  it("reports a throwing handler without crashing and destroys the task", async () => {
    const { task, ask } = fakeTask();
    const flow = attachPasswordHandler(task, () => () => { throw new Error("dialog failed"); });
    ask(() => {}, 1);
    await flush();
    expect(flow.failure?.message).toBe("dialog failed");
    expect(task.destroyed).toBe(1);
  });

  it("stops after the attempt cap", async () => {
    const { task, ask } = fakeTask();
    const flow = attachPasswordHandler(task, () => () => "nope");
    for (let i = 0; i <= MAX_PASSWORD_ATTEMPTS; i++) { ask(() => {}, 2); await flush(); }
    expect(flow.failure?.message).toMatch(/exhausted/);
    expect(task.destroyed).toBe(1);
  });
});

describe("load timer", () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  it("rejects after the budget and stops its clock while paused", async () => {
    const timer = createLoadTimer(1000, "timed out");
    const outcome = timer.promise.then(() => "resolved", (error: Error) => error.message);
    timer.pause();
    await vi.advanceTimersByTimeAsync(5000);
    timer.resume();
    await vi.advanceTimersByTimeAsync(999);
    let settled = false;
    void outcome.then(() => { settled = true; });
    await vi.advanceTimersByTimeAsync(0);
    expect(settled).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(await outcome).toBe("timed out");
  });
});

describe("maxRenderPages", () => {
  it("is unlimited by default and for invalid values", () => {
    expect(renderPageLimit(40)).toBe(40);
    expect(renderPageLimit(40, undefined)).toBe(40);
    expect(renderPageLimit(40, Number.NaN)).toBe(40);
  });
  it("caps, floors and bounds", () => {
    expect(renderPageLimit(40, 5)).toBe(5);
    expect(renderPageLimit(40, 5.9)).toBe(5);
    expect(renderPageLimit(3, 10)).toBe(3);
    expect(renderPageLimit(40, 0)).toBe(0);
    expect(renderPageLimit(40, -2)).toBe(0);
    expect(renderPageLimit(40, Number.POSITIVE_INFINITY)).toBe(40);
    expect(renderPageLimit(0, 5)).toBe(0);
  });
});
