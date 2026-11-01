import { describe, expect, it } from "vitest";
import {
  activityActionLabel,
  activityDisplayTitle,
  activityFileChangeStats,
  activityRowDetail,
  type ActivityItem,
} from "./activityLabelProjection";

const activity = (overrides: Partial<ActivityItem>): ActivityItem => ({
  kind: "tool", title: "Tool", complete: true, ...overrides,
});

describe("activityLabelProjection", () => {
  it("humanizes namespaced provider tools and preserves useful details", () => {
    const item = activity({ title: "mcp__threads__create_thread" });
    expect(activityDisplayTitle(item)).toBe("Create thread");
    expect(activityRowDetail(item)).toBe("Create thread");
    expect(activityActionLabel(item)).toBe("Tool");
  });

  it("gives ask-user rows a purpose-specific label", () => {
    const item = activity({ title: "AskUserQuestion", arguments: '{"questions":[]}' });
    expect(activityDisplayTitle(item)).toBe("Ask questions");
    expect(activityRowDetail(item)).toBe("");
    expect(activityActionLabel(item)).toBe("Ask questions");
  });

  it("projects command, web-search, and plan state", () => {
    expect(activityDisplayTitle(activity({ kind: "command", title: "bash", complete: false, arguments: JSON.stringify({ description: "Run focused tests" }) }))).toBe("Running command: Run focused tests");
    expect(activityDisplayTitle(activity({ kind: "search", title: "web_search", arguments: JSON.stringify({ query: "Waku" }) }))).toBe("Searched the web for Waku");
    expect(activityDisplayTitle(activity({ kind: "plan", title: "update_plan", complete: false }))).toBe("Updating plan");
  });

  it("projects file state and aggregate diff statistics", () => {
    const item = activity({
      kind: "file-change",
      title: "apply_patch",
      arguments: JSON.stringify({ patch: "*** Begin Patch\\n*** Update File: /tmp/src/app.rs\\n@@\\n-old\\n+new\\n+more\\n*** End Patch" }),
    });
    expect(activityDisplayTitle(item)).toBe("Edited app.rs");
    expect(activityFileChangeStats(item)).toEqual({ additions: 2, deletions: 1 });
    expect(activityFileChangeStats({ ...item, failed: true })).toBeNull();
  });
});
