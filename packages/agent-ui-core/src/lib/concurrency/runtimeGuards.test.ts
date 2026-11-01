import { describe, expect, it } from "vitest";

import { createRuntimeGuards, RuntimeGuards } from "./runtimeGuards";

describe("RuntimeGuards", () => {
  it("admits only the newest request for the active session", () => {
    const guards = createRuntimeGuards();
    guards.openSession("session-a");
    const first = guards.beginRequest();
    const second = guards.beginRequest();

    expect(guards.isCurrent(first)).toBe(false);
    expect(guards.isCurrent(second)).toBe(true);
  });

  it("rejects a late request after switching sessions", () => {
    const guards = createRuntimeGuards();
    guards.openSession("session-a");
    const oldRequest = guards.beginRequest();

    guards.openSession("session-b");
    const newRequest = guards.beginRequest();

    expect(guards.isCurrent(oldRequest)).toBe(false);
    expect(guards.isCurrent(newRequest)).toBe(true);
  });

  it("rejects a late frame from a previous generation even when the id returns", () => {
    const guards = new RuntimeGuards();
    const oldScope = guards.openSession("session-a");

    guards.openSession("session-b");
    guards.openSession("session-a");
    const currentScope = guards.scope();

    expect(guards.acceptsFrame(oldScope, { sessionId: "session-a" })).toBe(false);
    expect(guards.acceptsFrame(currentScope, { sessionId: "session-a" })).toBe(true);
  });

  it("rejects frames for another session and accepts current frames", () => {
    const guards = createRuntimeGuards();
    const scope = guards.openSession("session-a");

    expect(guards.acceptsFrame(scope, { sessionId: "session-b" })).toBe(false);
    expect(guards.acceptsFrame(scope, { sessionId: "session-a" })).toBe(true);
  });

  it("invalidates the current request without changing the session scope", () => {
    const guards = createRuntimeGuards();
    guards.openSession("session-a");
    const request = guards.beginRequest();

    guards.invalidateRequest();

    expect(guards.isCurrent(request)).toBe(false);
    expect(guards.acceptsFrame(guards.scope(), { sessionId: "session-a" })).toBe(true);
  });

  it("fails closed when callers use an inactive or mismatched session", () => {
    const guards = createRuntimeGuards();

    expect(() => guards.beginRequest("session-a")).toThrow();
    expect(() => guards.openSession("")).toThrow();
    guards.openSession("session-a");
    expect(() => guards.beginRequest("session-b")).toThrow();
  });
});
