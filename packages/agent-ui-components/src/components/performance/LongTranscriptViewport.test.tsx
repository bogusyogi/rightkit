// @vitest-environment happy-dom
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { TranscriptMessage } from "@rightkit/agent-ui-core/lib/types";
import { LongTranscriptViewport } from "./LongTranscriptViewport.js";
import source from "./LongTranscriptViewport.tsx?raw";

const message = (index: number, kind: "user" | "assistant"): TranscriptMessage =>
  kind === "user"
    ? {
        $kind: "user",
        id: `user-${index}`,
        createdAt: "2026-08-29T00:00:00Z",
        content: `Prompt ${index}`,
        pinned: false,
      }
    : {
        $kind: "assistant",
        id: `assistant-${index}`,
        createdAt: "2026-08-29T00:00:01Z",
        content: `Answer ${index}`,
        toolCalls: [],
        pinned: false,
      };

const messages = (count: number) =>
  Array.from({ length: count }, (_, index) => message(index, index % 2 ? "assistant" : "user"));

function renderGroup(group: { id: string }) {
  return <article data-testid={group.id}>{group.id}</article>;
}

describe("LongTranscriptViewport", () => {
  it("keeps ordinary transcripts in document flow", () => {
    const html = renderToStaticMarkup(
      <LongTranscriptViewport
        density="normal"
        messages={messages(4)}
        pendingQuestion={null}
        renderGroup={renderGroup}
      />,
    );

    expect(html).toContain('data-transcript-render-mode="static"');
    expect(html).toContain('data-testid="user-0"');
    expect(html).toContain('data-testid="assistant-3"');
  });

  it("renders only an overscanned viewport for a long transcript", () => {
    const html = renderToStaticMarkup(
      <LongTranscriptViewport
        density="normal"
        messages={messages(100)}
        pendingQuestion={null}
        renderGroup={renderGroup}
      />,
    );

    expect(html).toContain('data-transcript-render-mode="virtualized"');
    expect(html).toContain('data-index="0"');
    expect(html).not.toContain('data-index="99"');
    expect(html).not.toContain('data-testid="assistant-99"');
  });

  /** Waku's render-path guards reject filesystem/process work in frame code. */
  it("keeps the viewport a prepared-snapshot renderer", () => {
    for (const forbidden of [
      "fetch(",
      "localStorage",
      "sessionStorage",
      "readFile",
      "writeFile",
      "exec(",
      "invoke(",
    ]) {
      expect(source).not.toContain(forbidden);
    }
    expect(source).toContain("projectTranscriptGroups");
    expect(source).toContain("ResizeObserver");
  });
});
