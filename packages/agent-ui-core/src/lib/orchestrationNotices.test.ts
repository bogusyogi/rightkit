import { describe, expect, it } from "vitest";

import type { ProtocolEvent, TranscriptMessage } from "./types";
import {
  insertOrchestrationNotice,
  insertOrchestrationNoticeFromRoster,
  mergeOrchestrationNotices,
  orchestrationNoticeForSpawn,
} from "./orchestrationNotices";

const spawn: Extract<ProtocolEvent, { $kind: "subAgentSpawned" }> = {
  $kind: "subAgentSpawned",
  sessionId: "parent",
  seq: 2,
  createdAt: "2026-07-15T10:00:01Z",
  subSessionId: "child-1",
  role: "researcher",
  taskTitle: "verify the repository map",
};

const user: TranscriptMessage = {
  $kind: "user",
  id: "user-1",
  createdAt: "2026-07-15T10:00:00Z",
  content: "Review the architecture.",
  pinned: false,
};

describe("orchestration notices", () => {
  it("creates a truthful visible handoff for a spawned agent", () => {
    const notice = orchestrationNoticeForSpawn(spawn);

    expect(notice.$kind).toBe("assistant");
    expect("content" in notice && notice.content).toContain("**Orchestrator**");
    expect("content" in notice && notice.content).toContain("starting a researcher agent");
    expect("content" in notice && notice.content).toContain("verify the repository map");
  });

  it("inserts the handoff once and after the preceding user turn", () => {
    const once = insertOrchestrationNotice([user], spawn);
    const twice = insertOrchestrationNotice(once, spawn);

    expect(twice).toHaveLength(2);
    expect(twice[0].id).toBe("user-1");
    expect(twice[1].id).toBe("orchestrator-subagent-child-1");
  });

  it("preserves a replayed handoff when the transcript request resolves later", () => {
    const current = insertOrchestrationNotice([], spawn);
    const merged = mergeOrchestrationNotices([user], current);

    expect(merged.map((message) => message.id)).toEqual([
      "user-1",
      "orchestrator-subagent-child-1",
    ]);
  });

  it("recovers the handoff from the roster when the spawn event was missed", () => {
    const recovered = insertOrchestrationNoticeFromRoster(
      [user],
      { subSessionId: "child-1", role: "researcher" },
      spawn.createdAt,
    );
    const deduplicated = insertOrchestrationNotice(recovered, spawn);

    expect(deduplicated).toHaveLength(2);
    expect("content" in deduplicated[1] && deduplicated[1].content).toContain(
      "starting a researcher agent",
    );
  });
});
