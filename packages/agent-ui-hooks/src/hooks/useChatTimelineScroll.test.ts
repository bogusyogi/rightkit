// @vitest-environment happy-dom
import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import {
  useChatTimelineScroll,
  type TimelineListHandle,
} from "./useChatTimelineScroll.js";

function listHandle(): TimelineListHandle {
  const node = document.createElement("div");
  const state = {
    data: ["a", "b"],
    scroll: 0,
    scrollLength: 400,
    contentLength: 800,
    positionAtIndex: (index: number) => index * 100,
    sizeAtIndex: () => 100,
    listen: () => () => undefined,
  };
  return {
    getState: () => state,
    getScrollableNode: () => node,
    scrollToEnd: vi.fn(),
    scrollToOffset: vi.fn(),
    scrollToIndex: vi.fn(),
  };
}

describe("useChatTimelineScroll", () => {
  it("starts pinned and registers the virtual list node", () => {
    const { result } = renderHook(() => useChatTimelineScroll({
      currentSessionId: "session-1",
      currentSessionKey: "session-1:1",
      sessionMessageCount: 2,
      composerOverlayHeight: 120,
      lastUserMessageId: null,
    }));
    expect(result.current.isPinned).toBe(true);
    expect(result.current.userOwnsScroll).toBe(false);

    act(() => result.current.registerList(listHandle()));
    expect(result.current.scrollNode).toBeInstanceOf(HTMLDivElement);
  });

  it("hands viewport snapshots to the optional persistence seam", () => {
    const onViewportAnchorChange = vi.fn();
    const { result } = renderHook(() => useChatTimelineScroll({
      currentSessionId: "session-1",
      currentSessionKey: "session-1:1",
      sessionMessageCount: 2,
      composerOverlayHeight: 120,
      lastUserMessageId: null,
      onViewportAnchorChange,
    }));
    const list = listHandle();
    act(() => result.current.registerList(list));
    act(() => {
      result.current.onManualNavigation();
      list.getScrollableNode()?.dispatchEvent(new Event("scroll"));
      result.current.saveSnapshotNow();
    });
    expect(onViewportAnchorChange).toHaveBeenCalledWith(
      "session-1",
      expect.any(Number),
      expect.objectContaining({ scrollHeight: 0, clientHeight: 0 }),
    );
  });

  it("manual navigation claims the viewport and exposes the return affordance", () => {
    const { result } = renderHook(() => useChatTimelineScroll({
      currentSessionId: "session-1",
      currentSessionKey: "session-1:1",
      sessionMessageCount: 2,
      composerOverlayHeight: 120,
      lastUserMessageId: null,
    }));
    act(() => result.current.registerList(listHandle()));
    act(() => result.current.onManualNavigation());
    expect(result.current.userOwnsScroll).toBe(true);
    expect(result.current.isFollowingProgrammatically).toBe(false);
  });
});
