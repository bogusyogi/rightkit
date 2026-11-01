import { describe, expect, it } from "vitest";
import type { SessionRow, TranscriptMessage } from "@rightkit/agent-ui-core/lib/types";
import {
  jsonExportFilename,
  renderStructuredSessionExport,
  renderStructuredSessionMarkdown,
} from "@rightkit/agent-ui-hooks/components/app/sessionExport";

const session: SessionRow = {
  id: "session-123456789",
  title: "Trust audit",
  model: "openai/gpt-5",
  workspaceRoot: "D:/work/coderight",
  createdAt: "2026-06-29T09:00:00Z",
  updatedAt: "2026-06-29T10:00:00Z",
  status: "idle",
  workspaceTrust: "trusted",
};

const transcript: TranscriptMessage[] = [
  {
    $kind: "user",
    id: "m1",
    createdAt: "2026-06-29T09:01:00Z",
    content: "Export the trust verdicts",
    pinned: false,
    trust: {
      trust: "user",
      source: "composer",
      instructionsAreData: false,
      contentHash: "u1",
    },
  },
  {
    $kind: "assistant",
    id: "m2",
    createdAt: "2026-06-29T09:02:00Z",
    content: "I changed this diff:\n```diff\n+export const ok = true;\n```",
    pinned: false,
    toolCalls: [
      {
        id: "call-1",
        name: "apply_patch",
        argumentsJson: '{"file":"src/a.ts"}',
      },
    ],
  },
  {
    $kind: "tool",
    id: "m3",
    createdAt: "2026-06-29T09:03:00Z",
    toolCallId: "call-1",
    summary: "Patch applied",
    isError: false,
    pinned: false,
    trust: {
      trust: "toolTrusted",
      source: "apply_patch",
      instructionsAreData: true,
      contentHash: "t1",
    },
  },
];
const toolMessage = transcript[2] as Extract<TranscriptMessage, { $kind: "tool" }>;

describe("structured session export", () => {
  it("builds JSON with transcript, tool calls, diffs, trust verdicts, and cost", () => {
    const exported = renderStructuredSessionExport(session, transcript, {
      exportedAt: "2026-06-29T10:30:00Z",
      costUsd: 1.25,
    });

    expect(exported.session.id).toBe("session-123456789");
    expect(exported.cost).toEqual({ totalUsd: 1.25 });
    expect(exported.toolCalls).toEqual([
      {
        id: "call-1",
        name: "apply_patch",
        argumentsJson: '{"file":"src/a.ts"}',
        createdAt: "2026-06-29T09:02:00Z",
        result: {
          createdAt: "2026-06-29T09:03:00Z",
          isError: false,
          summary: "Patch applied",
          trust: toolMessage.trust,
        },
      },
    ]);
    expect(exported.diffs[0].content).toContain("+export const ok = true;");
    expect(exported.trustVerdicts).toHaveLength(2);
  });

  it("renders Markdown sections for the structured parts", () => {
    const markdown = renderStructuredSessionMarkdown(session, transcript, {
      exportedAt: "2026-06-29T10:30:00Z",
      costUsd: 1.25,
    });

    expect(markdown).toContain("## Cost");
    expect(markdown).toContain("## Tool Calls");
    expect(markdown).toContain("apply_patch");
    expect(markdown).toContain("## Diffs");
    expect(markdown).toContain("## Trust Verdicts");
  });

  it("uses a json filename for JSON exports", () => {
    expect(jsonExportFilename(session)).toBe("coderight-Trust-audit-session-.json");
  });
});
