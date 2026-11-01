import { fireEvent, render, screen } from "@testing-library/react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

import TranscriptStream, {
  ApprovalCard,
  DiffCard,
  RoutingChip,
  ToolCallCard,
  TranscriptEmptyState,
} from "@rightkit/agent-ui-workbench/components/workbench/TranscriptStream";
import type { ProtocolEvent } from "@rightkit/agent-ui-core/lib/types";
import { INITIAL_RUN_CHANGES_STATE } from "../review/runChangesStore";
import { resetStore } from "../review/useRunChangesState";

describe("ToolCallCard", () => {
  it("renders tool name and parsed arg=value pairs in the header", () => {
    const html = renderToStaticMarkup(
      <ToolCallCard
        toolName="run_shell"
        args={JSON.stringify({ command: "ls -la", cwd: "/tmp" })}
        summary="hello world\nExit: 0"
      />,
    );

    expect(html).not.toContain("🔧");
    expect(html).toContain("run_shell");
    expect(html).toContain("command=ls -la");
    expect(html).toContain("cwd=/tmp");
  });

  it("truncates long stdout to 2000 chars and shows a truncation marker", () => {
    const long = "x".repeat(2500);
    const html = renderToStaticMarkup(<ToolCallCard toolName="x" summary={long} />);

    expect(html).toContain("truncated");
    expect(html.length).toBeLessThan(8000);
  });

  it("renders ExitCode chip in the corner when stdout contains an exit line", () => {
    const html = renderToStaticMarkup(<ToolCallCard toolName="x" summary="... exit code: 0" />);

    expect(html).toContain("Exit: 0");
  });

  it("renders the empty-output fallback when summary is blank", () => {
    const html = renderToStaticMarkup(<ToolCallCard toolName="noop" summary="" />);

    expect(html).toContain("(no output)");
  });
});

describe("ApprovalCard", () => {
  it("renders Approve/Reject buttons with tool name and risk label", () => {
    const html = renderToStaticMarkup(
      <ApprovalCard
        toolName="apply_patch"
        riskLabel="file-write"
        proposedChange="diff --git a/foo b/foo\n@@\n-old\n+new"
        config={null}
        sessionId={null}
        toolCallId="t1"
      />,
    );

    expect(html).not.toContain("🔒");
    expect(html).toContain("Permission needed");
    expect(html).toContain("apply_patch");
    expect(html).toContain("file-write");
    expect(html).toContain("Approve");
    expect(html).toContain("Reject");
  });
});

describe("DiffCard", () => {
  it("renders the file path header with +/- chips and a diff body", () => {
    const html = renderToStaticMarkup(
      <DiffCard
        filePath="src/foo.ts"
        additions={3}
        deletions={1}
        diff={"diff --git a/src/foo.ts b/src/foo.ts\n@@ -1,1 +1,1 @@\n-old\n+new"}
      />,
    );

    expect(html).not.toContain("📝");
    expect(html).toContain("src/foo.ts");
    expect(html).toContain("+3");
    expect(html).toContain("-1");
    expect(html).toContain("old");
    expect(html).toContain("new");
  });
});

describe("RoutingChip", () => {
  it("renders model, cost, and tier in tabular-num mono font", () => {
    const html = renderToStaticMarkup(
      <RoutingChip modelId="claude-sonnet-4" cost={0.0034} tier="tier-low" />,
    );

    expect(html).toContain("→");
    expect(html).toContain("claude-sonnet-4");
    expect(html).toContain("$0.0034");
    expect(html).toContain("tier-low");
    expect(html).toContain("tabular-nums");
  });

  it("omits cost and tier segments when not provided", () => {
    const html = renderToStaticMarkup(<RoutingChip modelId="haiku" />);

    expect(html).toContain("haiku");
    expect(html).not.toContain("est.");
    expect(html).not.toContain("reason:");
  });
});

describe("TranscriptEmptyState", () => {
  it("renders the centered empty-state hint", () => {
    const html = renderToStaticMarkup(<TranscriptEmptyState />);

    expect(html).toContain("No messages yet");
    expect(html).toContain("describe a task to start");
  });
});

describe("TranscriptStream", () => {
  const baseEvent = {
    sessionId: "s1",
    seq: 1,
    createdAt: "2026-07-04T00:00:00Z",
  } as const;

  it("renders the empty state when given zero events", () => {
    const html = renderToStaticMarkup(<TranscriptStream events={[]} />);

    expect(html).toContain("No messages yet");
  });

  it("renders a routing chip for providerRouteUpdated events", () => {
    const events: ProtocolEvent[] = [
      {
        ...baseEvent,
        $kind: "providerRouteUpdated",
        state: "fallback",
        provider: "anthropic",
        model: "claude-sonnet-4",
        failureKind: "rate-limit",
        detail: "429",
      },
    ];

    const html = renderToStaticMarkup(<TranscriptStream events={events} />);

    expect(html).toContain("anthropic/claude-sonnet-4");
    expect(html).toContain("fallback");
  });

  it("renders run failures in the visible transcript", () => {
    const events: ProtocolEvent[] = [
      {
        ...baseEvent,
        $kind: "runFailed",
        error: "MiniMax request failed (401)",
        kind: "provider_auth",
        hint: "Add or replace the MiniMax API key in Settings → Providers.",
      },
    ];

    const html = renderToStaticMarkup(<TranscriptStream events={events} />);

    expect(html).toContain("MiniMax request failed (401)");
    expect(html).toContain("Settings");
    expect(html).toContain("Providers");
  });

  it("renders a routing chip for dispatchDecided events with strategy as model id", () => {
    const events: ProtocolEvent[] = [
      {
        ...baseEvent,
        $kind: "dispatchDecided",
        strategy: "multi-agent",
        rationale: "task is multi-domain",
      },
    ];

    const html = renderToStaticMarkup(<TranscriptStream events={events} />);

    expect(html).toContain("multi-agent");
    expect(html).toContain("task is multi-domain");
  });

  it("renders a routing chip for usageUpdated events with cost in dollars", () => {
    const events: ProtocolEvent[] = [
      {
        ...baseEvent,
        $kind: "usageUpdated",
        promptTokens: 100,
        completionTokens: 50,
        costUsd: 0.0042,
        contextTokens: 1024,
        contextPercent: 12,
      },
    ];

    const html = renderToStaticMarkup(<TranscriptStream events={events} />);

    expect(html).toContain("→");
    expect(html).toContain("usage");
    expect(html).toContain("$0.0042");
  });

  it("renders a routing chip for modelRouteDecision events with the est. cost label", () => {
    const events: ProtocolEvent[] = [
      {
        ...baseEvent,
        $kind: "modelRouteDecision",
        agentRole: "coder",
        modelId: "deepseek/deepseek-v4-flash",
        tier: "cheap",
        reason: "routed coder -> deepseek/deepseek-v4-flash (tier=Cheap, confidence=0.82)",
        signalsDigest: "sig-abc123",
        priceSnapshot: {
          promptPerM: 0.435,
          completionPerM: 0.87,
          source: "bundled",
          fetchedAt: "2026-07-04T00:00:00Z",
          hash: "hash-abc123",
        },
        estCostUsd: 0.0021,
        schemaVersion: 1,
      },
    ];

    const html = renderToStaticMarkup(<TranscriptStream events={events} />);

    expect(html).toContain("coder");
    expect(html).toContain("deepseek/deepseek-v4-flash");
    expect(html).toContain("est. $0.0021");
    expect(html).toContain("routed coder");
  });

  it("renders tool-call and approval cards in seq order", () => {
    const events: ProtocolEvent[] = [
      {
        ...baseEvent,
        $kind: "toolCallStarted",
        call: { id: "c1", name: "read_file", argumentsJson: '{"path":"/etc/hosts"}' },
      },
      {
        ...baseEvent,
        $kind: "toolApprovalRequested",
        call: { id: "c2", name: "apply_patch", argumentsJson: "{}" },
        riskLabel: "file-write",
        proposedChange: "diff --git a/x b/x\n@@\n-old\n+new",
        middlewareDecisions: [],
      },
    ];

    const html = renderToStaticMarkup(
      <TranscriptStream events={events} config={null} sessionId={null} />,
    );

    const toolIndex = html.indexOf("read_file");
    const approvalIndex = html.indexOf("apply_patch");
    expect(toolIndex).toBeGreaterThan(-1);
    expect(approvalIndex).toBeGreaterThan(toolIndex);
  });

  it("hides a previous turn's changed-files card after the next user message starts", () => {
    const git = {
      present: true,
      headSha: "abc123",
      branch: "main",
      indexFingerprint: null,
      untrackedCount: 0,
    };
    resetStore({
      ...INITIAL_RUN_CHANGES_STATE,
      runId: "run-1",
      workspaceRoot: "/workspace",
      inventory: {
        runId: "run-1",
        workspaceRoot: "/workspace",
        entries: [],
        truncated: false,
        git,
        lastCaptureAt: "2026-07-21T00:00:00Z",
      },
      summary: {
        runId: "run-1",
        workspaceRoot: "/workspace",
        entryCount: 0,
        truncated: false,
        countsByClassification: {},
        firstPaths: [],
        lastCaptureAt: "2026-07-21T00:00:00Z",
        git,
      },
    });
    const events: ProtocolEvent[] = [
      {
        ...baseEvent,
        $kind: "assistantMessageCompleted",
        message: {
          $kind: "assistant",
          id: "m1",
          createdAt: baseEvent.createdAt,
          content: "done",
          toolCalls: [],
          pinned: false,
        },
      },
      {
        ...baseEvent,
        $kind: "userMessageSubmitted",
        seq: 2,
        text: "next task",
      },
    ];

    const html = renderToStaticMarkup(<TranscriptStream events={events} />);
    expect(html).not.toContain("changed-files-card");
  });

  it("groups every diff-shaped tool result from one turn into a single collapsed EditedFilesSummary row", () => {
    const events: ProtocolEvent[] = [
      {
        ...baseEvent,
        $kind: "toolCallStarted",
        seq: 1,
        call: { id: "c1", name: "edit_file", argumentsJson: '{"filePath":"src/a.ts"}' },
      },
      {
        ...baseEvent,
        $kind: "toolResultAdded",
        seq: 2,
        result: {
          $kind: "tool",
          id: "r1",
          createdAt: baseEvent.createdAt,
          toolCallId: "c1",
          summary: "@@ -1,1 +1,2 @@\n-old\n+new\n+more",
          isError: false,
          pinned: false,
        },
      },
      {
        ...baseEvent,
        $kind: "toolCallStarted",
        seq: 3,
        call: { id: "c2", name: "edit_file", argumentsJson: '{"filePath":"src/b.ts"}' },
      },
      {
        ...baseEvent,
        $kind: "toolResultAdded",
        seq: 4,
        result: {
          $kind: "tool",
          id: "r2",
          createdAt: baseEvent.createdAt,
          toolCallId: "c2",
          summary: "@@ -1,1 +1,1 @@\n-gone",
          isError: false,
          pinned: false,
        },
      },
      {
        ...baseEvent,
        $kind: "assistantMessageCompleted",
        seq: 5,
        message: {
          $kind: "assistant",
          id: "m1",
          createdAt: baseEvent.createdAt,
          content: "done",
          toolCalls: [],
          pinned: false,
        },
      },
    ];

    const onReviewEditedFiles = vi.fn();
    const onUndoEditedFiles = vi.fn();
    render(
      <TranscriptStream
        events={events}
        config={null}
        sessionId={null}
        onReviewEditedFiles={onReviewEditedFiles}
        onUndoEditedFiles={onUndoEditedFiles}
      />,
    );

    expect(screen.getByText("2 files edited")).toBeTruthy();
    expect(screen.getByText("+2")).toBeTruthy();
    expect(screen.getByText("-2")).toBeTruthy();
    expect(screen.queryByText("src/a.ts")).toBeNull();
    expect(screen.queryByText("src/b.ts")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Expand 2 files edited" }));
    expect(screen.getByText("src/a.ts")).toBeTruthy();
    expect(screen.getByText("src/b.ts")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Review edited files" }));
    expect(onReviewEditedFiles).toHaveBeenCalledOnce();
    expect(onReviewEditedFiles.mock.calls[0][0]).toHaveLength(2);

    fireEvent.click(screen.getByRole("button", { name: "Undo edited files" }));
    expect(onUndoEditedFiles).toHaveBeenCalledOnce();
    expect(onUndoEditedFiles.mock.calls[0][0]).toHaveLength(2);
  });
});

// F1: visible failure propagation. When the SSE catch-up or the
// transcript load reports a durability symptom, TranscriptStream
// surfaces the error as a banner above (and independently of) the
// transcript body — including the empty state.
describe("TranscriptStream durability banner", () => {
  it("renders nothing when lastError is null", () => {
    const html = renderToStaticMarkup(<TranscriptStream events={[]} lastError={null} />);
    expect(html).not.toContain("durability-error-banner");
  });

  it("renders nothing when lastError is undefined (default)", () => {
    const html = renderToStaticMarkup(<TranscriptStream events={[]} />);
    expect(html).not.toContain("durability-error-banner");
  });

  it("renders the banner with the error text when lastError is set", () => {
    const html = renderToStaticMarkup(
      <TranscriptStream events={[]} lastError="Event stream reconnecting... (disk full)" />,
    );
    expect(html).toContain("durability-error-banner");
    expect(html).toContain("Event stream degraded");
    expect(html).toContain("disk full");
  });

  it("renders the banner above the empty state (empty events, with error)", () => {
    const html = renderToStaticMarkup(
      <TranscriptStream events={[]} lastError="transcript load failed" />,
    );
    const bannerIdx = html.indexOf("durability-error-banner");
    const emptyIdx = html.indexOf("transcript");
    expect(bannerIdx).toBeGreaterThan(-1);
    expect(emptyIdx).toBeGreaterThan(bannerIdx);
  });

  it("renders the banner even when transcript is non-empty", () => {
    const events: ProtocolEvent[] = [
      {
        $kind: "runFailed",
        sessionId: "s",
        seq: 1,
        createdAt: "t",
        error: "x",
        kind: "other",
        hint: "",
      },
    ];
    const html = renderToStaticMarkup(
      <TranscriptStream events={events} lastError="durability: gap" />,
    );
    expect(html).toContain("durability-error-banner");
    expect(html).toContain("durability: gap");
    expect(html).toContain(">x<");
  });
});

describe("TranscriptStream density", () => {
  const sampleEvents: ProtocolEvent[] = [
    {
      $kind: "toolCallStarted",
      sessionId: "s1",
      seq: 1,
      createdAt: "t",
      call: { id: "c1", name: "read_file", argumentsJson: JSON.stringify({ path: "/a" }) },
    } as ProtocolEvent,
    {
      $kind: "toolResultAdded",
      sessionId: "s1",
      seq: 2,
      createdAt: "t",
      result: { toolCallId: "c1", summary: "ok" },
    } as unknown as ProtocolEvent,
  ];

  it("renders distinct markup per density (gap/padding/font) and exposes data-density", () => {
    const compact = renderToStaticMarkup(
      <TranscriptStream events={sampleEvents} density="summary" />,
    );
    const normal = renderToStaticMarkup(
      <TranscriptStream events={sampleEvents} density="normal" />,
    );
    const spacious = renderToStaticMarkup(
      <TranscriptStream events={sampleEvents} density="verbose" />,
    );
    const compactComfort = renderToStaticMarkup(
      <TranscriptStream events={sampleEvents} density={"compact" as never} />,
    );
    const comfortable = renderToStaticMarkup(
      <TranscriptStream events={sampleEvents} density={"comfortable" as never} />,
    );

    // data-density threading
    expect(compact).toContain('data-density="summary"');
    expect(normal).toContain('data-density="normal"');
    expect(spacious).toContain('data-density="verbose"');
    // alias compact/comfortable
    expect(compactComfort).toContain('data-density="summary"');
    expect(comfortable).toContain('data-density="verbose"');

    // gap/padding/font differ across densities: snapshot-ish asserts
    expect(compact).not.toBe(normal);
    expect(normal).not.toBe(spacious);
    // compact uses tighter header padding / smaller font
    expect(compact).toContain("py-1.5");
    expect(spacious).toContain("py-2.5");
    expect(compact).toContain("text-[10px]");
    expect(spacious).toContain("text-[13px]");
  });

  it("passes density to sub-cards (DiffCard/RoutingChip)", () => {
    const diffEvents: ProtocolEvent[] = [
      {
        $kind: "toolCallStarted",
        sessionId: "s1",
        seq: 1,
        createdAt: "t",
        call: { id: "c2", name: "edit_file", argumentsJson: JSON.stringify({ filePath: "a.ts" }) },
      } as ProtocolEvent,
      {
        $kind: "toolResultAdded",
        sessionId: "s1",
        seq: 2,
        createdAt: "t",
        result: { toolCallId: "c2", summary: "@@ -1 +1 @@\n-old\n+new" },
      } as unknown as ProtocolEvent,
      {
        $kind: "assistantMessageCompleted",
        sessionId: "s1",
        seq: 3,
        createdAt: "t",
      } as ProtocolEvent,
    ];
    const html = renderToStaticMarkup(<TranscriptStream events={diffEvents} density="summary" />);
    expect(html).toContain('data-density="summary"');
  });
});
