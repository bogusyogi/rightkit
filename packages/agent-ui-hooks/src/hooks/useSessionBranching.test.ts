// @vitest-environment happy-dom
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { DaemonConfig, SessionRow } from "@rightkit/agent-ui-core/lib/types";

vi.mock("@rightkit/agent-ui-core/lib/daemon", () => ({
  forkSession: vi.fn(),
  rewindSession: vi.fn(),
}));

import { forkSession, rewindSession } from "@rightkit/agent-ui-core/lib/daemon";
import { nextResponseForkTitle, useSessionBranching } from "./useSessionBranching.js";

const config: DaemonConfig = { url: "http://127.0.0.1:0", token: "test" };

const forkedSession: SessionRow = {
  id: "fork-1",
  title: "Fix the bug (2)",
  model: "model",
  workspaceRoot: "/workspace",
  createdAt: "2026-08-29T00:00:00Z",
  updatedAt: "2026-08-29T00:00:00Z",
  status: "idle",
  parentSessionId: "source",
  forkedFromSessionId: "source",
};

describe("nextResponseForkTitle", () => {
  // These cases are the Waku response_fork_title_tests ported from the donor.
  it("advances one numbered response-fork sequence", () => {
    expect(nextResponseForkTitle("Fix the bug", ["Fix the bug"])).toBe("Fix the bug (2)");
    expect(
      nextResponseForkTitle("Fix the bug", ["Fix the bug", "Fix the bug (2)", "Fix the bug (4)"]),
    ).toBe("Fix the bug (5)");
    expect(nextResponseForkTitle("Fix the bug (2)", ["Fix the bug", "Fix the bug (2)"])).toBe(
      "Fix the bug (3)",
    );
    expect(nextResponseForkTitle("Plan (2026)", ["Plan (2026)"])).toBe("Plan (2026) (2)");
  });
});

describe("useSessionBranching", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("keeps fork title confirmation state and invokes the daemon fork endpoint", async () => {
    vi.mocked(forkSession).mockResolvedValueOnce(forkedSession);
    const onForked = vi.fn();
    const onStatus = vi.fn();
    const { result } = renderHook(() =>
      useSessionBranching({
        config,
        sessionId: "source",
        sessionTitle: "Fix the bug",
        onForked,
        onStatus,
      }),
    );

    act(() => result.current.beginFork());
    expect(result.current.forkInteraction).toMatchObject({
      sourceSessionId: "source",
      title: "Fork of Fix the bug",
    });
    act(() => result.current.setForkTitle("  Fix the bug (2)  "));

    await act(async () => {
      await result.current.confirmFork();
    });

    expect(forkSession).toHaveBeenCalledWith(config, "source", "Fix the bug (2)");
    expect(onForked).toHaveBeenCalledWith(forkedSession);
    expect(result.current.interaction).toBeNull();
    expect(onStatus).toHaveBeenLastCalledWith("Forked session: Fix the bug (2)");
  });

  it("rewinds one daemon turn per requested turn and returns the last restored text", async () => {
    vi.mocked(rewindSession)
      .mockResolvedValueOnce("first prompt")
      .mockResolvedValueOnce("second prompt");
    const onRewound = vi.fn();
    const { result } = renderHook(() =>
      useSessionBranching({ config, sessionId: "source", onRewound }),
    );

    act(() => result.current.beginRewind("message-2", 2));
    expect(result.current.rewindInteraction).toMatchObject({
      sourceSessionId: "source",
      messageId: "message-2",
      turns: 2,
    });

    await act(async () => {
      await result.current.confirmRewind();
    });

    expect(rewindSession).toHaveBeenNthCalledWith(1, config, "source");
    expect(rewindSession).toHaveBeenNthCalledWith(2, config, "source");
    expect(onRewound).toHaveBeenCalledWith({
      sourceSessionId: "source",
      messageId: "message-2",
      turns: 2,
      restoredText: "second prompt",
    });
  });

  it("does not mutate a whole session when a response-scoped fork is requested", async () => {
    const onStatus = vi.fn();
    const { result } = renderHook(() =>
      useSessionBranching({
        config,
        sessionId: "source",
        sessionTitle: "Fix the bug",
        existingSessionTitles: ["Fix the bug"],
        onStatus,
      }),
    );

    act(() => result.current.beginResponseFork("assistant-1", 2));
    expect(result.current.responseForkInteraction?.title).toBe("Fix the bug (2)");

    await act(async () => {
      await result.current.confirmFork();
    });

    expect(forkSession).not.toHaveBeenCalled();
    expect(onStatus).toHaveBeenLastCalledWith(
      "Response branching needs a daemon turn-scoped fork endpoint; the whole-session fork was not used",
    );
  });

  it("rejects fork and rewind actions without an idle selected session", () => {
    const onStatus = vi.fn();
    const { result } = renderHook(() =>
      useSessionBranching({
        config: null,
        sessionId: null,
        isWorking: true,
        onStatus,
      }),
    );

    act(() => result.current.beginFork());
    act(() => result.current.beginRewind("message-1"));

    expect(result.current.interaction).toBeNull();
    expect(onStatus).toHaveBeenNthCalledWith(1, "Select a session before forking");
    expect(onStatus).toHaveBeenNthCalledWith(2, "Select a session before rewinding");
  });
});
