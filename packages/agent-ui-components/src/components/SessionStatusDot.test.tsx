import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it } from "vitest";

import type { SessionRow } from "@rightkit/agent-ui-core/lib/types";
import { useSessionAttentionStore } from "@rightkit/agent-ui-core/state/sessionAttentionStore";
import SessionStatusDot from "./SessionStatusDot";

const session = (status: string): SessionRow =>
  ({
    id: "s1",
    title: "Session",
    model: "local",
    workspaceRoot: "/workspace",
    createdAt: "",
    updatedAt: "",
    status,
  }) as SessionRow;

beforeEach(() => {
  useSessionAttentionStore.getState().reset();
});

describe("SessionStatusDot", () => {
  it("uses the row status when no live projection exists", () => {
    const html = renderToStaticMarkup(
      <SessionStatusDot storedSessionId="s1" session={session("running")} />,
    );
    expect(html).toContain('data-status="running"');
    expect(html).toContain('aria-label="Running"');
  });

  it("uses the shared attention projection over a stale row status", () => {
    useSessionAttentionStore.getState().markUnread("s1");
    const html = renderToStaticMarkup(
      <SessionStatusDot storedSessionId="s1" session={session("idle")} branchStem="└─ " />,
    );
    expect(html).toContain('data-status="unread"');
    expect(html).toContain('aria-label="Finished — unread"');
    expect(html).toContain("└─");
  });

  it("renders a draft without a durable id", () => {
    const draft = renderToStaticMarkup(<SessionStatusDot storedSessionId={null} />);
    expect(draft).toContain('data-status="draft"');
    expect(draft).toContain('aria-label="Draft session"');
  });
});
