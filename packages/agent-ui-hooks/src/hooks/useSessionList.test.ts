import { describe, expect, it } from "vitest";
import type { SessionSearchHit } from "@rightkit/agent-ui-core/lib/daemon";
import type { SessionRow } from "@rightkit/agent-ui-core/lib/types";
import {
  sessionRowsForSearch,
  sessionSearchSnippetsBySession,
  visibleSidebarSessions,
} from "./useSessionList.js";

const sessions: SessionRow[] = [
  {
    id: "s-live",
    title: "Live refactor",
    model: "openai/gpt-5",
    workspaceRoot: "D:/work/coderight",
    createdAt: "2026-06-29T09:00:00Z",
    updatedAt: "2026-06-29T09:20:00Z",
    status: "idle",
  },
];

describe("session history search shaping", () => {
  it("keeps background child sessions out of left-sidebar Recents", () => {
    const child: SessionRow = {
      ...sessions[0],
      id: "sub-auditor",
      title: "auditor subtask",
      parentSessionId: "s-live",
    };

    expect(visibleSidebarSessions([...sessions, child]).map((row) => row.id)).toEqual(["s-live"]);
  });

  it("keeps empty sessions out of Recents until they contain a real chat turn", () => {
    const empty: SessionRow = {
      ...sessions[0],
      id: "s-empty",
      title: "coderight",
      hasHistory: false,
    };
    const chatted: SessionRow = { ...sessions[0], hasHistory: true };

    expect(visibleSidebarSessions([empty, chatted]).map((row) => row.id)).toEqual(["s-live"]);
  });

  it("keeps a named external-driver session visible while its history flag catches up", () => {
    const driver: SessionRow = {
      ...sessions[0],
      id: "s-driver",
      title: "Codex verification",
      hasHistory: false,
      driverKind: "codex",
    };

    expect(visibleSidebarSessions([driver]).map((row) => row.id)).toEqual(["s-driver"]);
  });

  it("keeps daemon transcript hits as sidebar rows and exposes snippets by session", () => {
    const hits: SessionSearchHit[] = [
      {
        sessionId: "s-old",
        title: "Old trust audit",
        snippet: "middleware blocked the untrusted tool output",
      },
      {
        sessionId: "s-live",
        title: "Live refactor",
        snippet: "refactor plan mentioned middleware trust",
      },
    ];

    expect(sessionRowsForSearch(hits, sessions, "trust")).toMatchObject([
      { id: "s-old", title: "Old trust audit", model: "history", status: "dormant" },
      { id: "s-live", title: "Live refactor", model: "openai/gpt-5" },
    ]);
    // A search hit may carry an indexed title; retain the live SessionRow so
    // selecting a result always targets the durable session identity.
    const indexedTitleHit = { ...hits[1], title: "Indexed title" };
    expect(sessionRowsForSearch([indexedTitleHit], sessions, "trust")[0]).toBe(sessions[0]);
    expect(sessionSearchSnippetsBySession(hits)).toEqual(
      new Map([
        ["s-old", ["middleware blocked the untrusted tool output"]],
        ["s-live", ["refactor plan mentioned middleware trust"]],
      ]),
    );
  });

  it("strips the owner's match markers from engine snippets", () => {
    const hits: SessionSearchHit[] = [
      { sessionId: "s-old", title: "Old", snippet: "middleware blocked the tool" },
    ];
    expect(sessionSearchSnippetsBySession(hits)).toEqual(new Map([["s-old", ["middleware blocked the tool"]]]));
  });

  it("adds local title matches when transcript hits miss active sessions", () => {
    expect(sessionRowsForSearch([], sessions, "refactor")).toHaveLength(1);
    expect(sessionRowsForSearch([], sessions, "refactor")[0].id).toBe("s-live");
  });
});
