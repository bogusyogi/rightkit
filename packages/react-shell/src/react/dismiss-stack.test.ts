// Topmost-modal Escape ownership and implicit toast dedupe. Both are pure helpers so they run without a DOM.
import { describe, expect, it } from "vitest";
import { isTopEscapeOwner, pushEscapeOwner } from "./escape-stack.js";
import { isImplicitDuplicate } from "./toast-dedupe.js";
import type { Toast } from "../toast.js";

describe("escape ownership stack", () => {
  it("only the most recently pushed owner is on top", () => {
    const a = {};
    const b = {};
    const offA = pushEscapeOwner(a);
    const offB = pushEscapeOwner(b);
    expect(isTopEscapeOwner(a)).toBe(false);
    expect(isTopEscapeOwner(b)).toBe(true);
    offB();
    expect(isTopEscapeOwner(a)).toBe(true);
    offA();
  });

  it("removing a lower owner leaves the top owner in place", () => {
    const a = {};
    const b = {};
    const offA = pushEscapeOwner(a);
    const offB = pushEscapeOwner(b);
    offA();
    expect(isTopEscapeOwner(b)).toBe(true);
    offB();
    expect(isTopEscapeOwner(b)).toBe(false);
  });

  it("removal is idempotent and an unregistered owner is never on top", () => {
    const a = {};
    const off = pushEscapeOwner(a);
    off();
    off();
    expect(isTopEscapeOwner(a)).toBe(false);
    expect(isTopEscapeOwner({})).toBe(false);
  });
});

function toast(over: Partial<Toast>): Toast {
  return { id: 1, kind: "info", message: "Deleted", ttl: 4000, ...over } as Toast;
}

describe("implicit toast dedupe", () => {
  it("merges plain repeats of the same kind and message", () => {
    expect(isImplicitDuplicate(toast({}), toast({ id: 2 }))).toBe(true);
  });

  it("never merges when either toast carries an action, so the callback stays with its item", () => {
    // The action shape comes from @rightkit/platform-ui; only its identity matters for dedupe.
    const undoA = { label: "Undo", run: () => undefined } as unknown as Toast["action"];
    const undoB = { label: "Undo", run: () => undefined } as unknown as Toast["action"];
    expect(isImplicitDuplicate(toast({ action: undoA }), toast({ id: 2, action: undoB }))).toBe(false);
    expect(isImplicitDuplicate(toast({}), toast({ id: 2, action: undoB }))).toBe(false);
    expect(isImplicitDuplicate(toast({ action: undoA }), toast({ id: 2 }))).toBe(false);
  });

  it("does not merge different messages or kinds", () => {
    expect(isImplicitDuplicate(toast({}), toast({ message: "Saved" }))).toBe(false);
    expect(isImplicitDuplicate(toast({}), toast({ kind: "error" }))).toBe(false);
  });
});
