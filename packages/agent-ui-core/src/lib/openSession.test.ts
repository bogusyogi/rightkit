import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  mainChatOccupied,
  openSession,
  openSessionIntentFromModifiers,
  type SessionOpenAdapter,
} from "./openSession.js";

function adapter(overrides: Partial<SessionOpenAdapter> = {}): SessionOpenAdapter {
  return {
    activeSessionId: null,
    selectedStoredSessionId: null,
    workspaceIsPage: false,
    focusOpenSession: vi.fn(() => null),
    openSessionTile: vi.fn(),
    reuseBlankDraftTile: vi.fn(() => false),
    canOpenSessionWindow: vi.fn(() => true),
    openSessionInNewWindow: vi.fn(),
    sessionRoute: (id) => `/c/${encodeURIComponent(id)}`,
    ...overrides,
  };
}

describe("mainChatOccupied", () => {
  it("is occupied once a conversation is on screen", () => {
    expect(mainChatOccupied("runtime-a", "stored-a")).toBe(true);
  });

  it("is occupied by a live runtime whose stored id has not landed yet", () => {
    expect(mainChatOccupied("runtime-a", null)).toBe(true);
  });

  it("is occupied by a selected session still resuming into a runtime", () => {
    expect(mainChatOccupied(null, "stored-a")).toBe(true);
  });

  it("is free when nothing is open", () => {
    expect(mainChatOccupied(null, null)).toBe(false);
  });
});

describe("openSessionIntentFromModifiers", () => {
  it("defaults to in-place and preserves the caller base", () => {
    expect(openSessionIntentFromModifiers()).toBe("in-place");
    expect(openSessionIntentFromModifiers(null)).toBe("in-place");
    expect(openSessionIntentFromModifiers({}, "stack")).toBe("stack");
  });

  it("reads control/meta as tab and shift plus a modifier as window", () => {
    expect(openSessionIntentFromModifiers({ metaKey: true })).toBe("tab");
    expect(openSessionIntentFromModifiers({ ctrlKey: true })).toBe("tab");
    expect(openSessionIntentFromModifiers({ metaKey: true, shiftKey: true })).toBe("window");
    expect(openSessionIntentFromModifiers({ shiftKey: true })).toBe("in-place");
  });
});

describe("openSession", () => {
  const navigate = vi.fn();

  beforeEach(() => navigate.mockReset());

  it("in-place focuses an existing tile and does not navigate", () => {
    const focusOpenSession = vi.fn(() => "tile" as const);
    const openSessionTile = vi.fn();
    openSession("s1", navigate, "in-place", adapter({ focusOpenSession, openSessionTile }));
    expect(focusOpenSession).toHaveBeenCalledWith("s1");
    expect(navigate).not.toHaveBeenCalled();
    expect(openSessionTile).not.toHaveBeenCalled();
  });

  it("in-place routes when the session is not on screen", () => {
    openSession("s1", navigate, "in-place", adapter());
    expect(navigate).toHaveBeenCalledWith("/c/s1");
  });

  it("in-place routes when main is covered by a page", () => {
    const focusOpenSession = vi.fn(() => "main" as const);
    openSession(
      "s1",
      navigate,
      "in-place",
      adapter({ focusOpenSession, workspaceIsPage: true }),
    );
    expect(navigate).toHaveBeenCalledWith("/c/s1");
  });

  it("tab focuses an existing session instead of stacking another", () => {
    const focusOpenSession = vi.fn(() => "tile" as const);
    const openSessionTile = vi.fn();
    openSession("s1", navigate, "tab", adapter({ focusOpenSession, openSessionTile }));
    expect(focusOpenSession).toHaveBeenCalledWith("s1");
    expect(openSessionTile).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  });

  it("tab opens a stacked session tile when not on screen", () => {
    const openSessionTile = vi.fn();
    openSession("s1", navigate, "tab", adapter({ openSessionTile }));
    expect(openSessionTile).toHaveBeenCalledWith("s1", "center");
    expect(navigate).not.toHaveBeenCalled();
  });

  it("stack opens a tab rather than taking main from a loaded chat", () => {
    const openSessionTile = vi.fn();
    openSession(
      "s1",
      navigate,
      "stack",
      adapter({ selectedStoredSessionId: "s0", openSessionTile }),
    );
    expect(openSessionTile).toHaveBeenCalledWith("s1", "center");
    expect(navigate).not.toHaveBeenCalled();
  });

  it("stack spends an open blank draft tab before stacking", () => {
    const reuseBlankDraftTile = vi.fn(() => true);
    const openSessionTile = vi.fn();
    openSession(
      "s1",
      navigate,
      "stack",
      adapter({ selectedStoredSessionId: "s0", reuseBlankDraftTile, openSessionTile }),
    );
    expect(reuseBlankDraftTile).toHaveBeenCalledWith("s1");
    expect(openSessionTile).not.toHaveBeenCalled();
  });

  it("stack loads into main when it holds only a blank draft", () => {
    openSession("s1", navigate, "stack", adapter());
    expect(navigate).toHaveBeenCalledWith("/c/s1");
  });

  it("window pops out when supported and falls back to a tab otherwise", () => {
    const openSessionInNewWindow = vi.fn();
    openSession(
      "s1",
      navigate,
      "window",
      adapter({ openSessionInNewWindow }),
    );
    expect(openSessionInNewWindow).toHaveBeenCalledWith("s1");

    const openSessionTile = vi.fn();
    openSession(
      "s2",
      navigate,
      "window",
      adapter({ canOpenSessionWindow: vi.fn(() => false), openSessionTile }),
    );
    expect(openSessionTile).toHaveBeenCalledWith("s2", "center");
  });

  it("no-ops on an empty id", () => {
    const focusOpenSession = vi.fn();
    openSession("", navigate, "in-place", adapter({ focusOpenSession }));
    expect(navigate).not.toHaveBeenCalled();
    expect(focusOpenSession).not.toHaveBeenCalled();
  });
});
