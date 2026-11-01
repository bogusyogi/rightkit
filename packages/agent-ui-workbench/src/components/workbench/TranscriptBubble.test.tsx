import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { TranscriptBubble } from "./TranscriptBubble.js";
import type { TranscriptMessage } from "@rightkit/agent-ui-core/lib/types";

describe("TranscriptBubble image-block rendering", () => {
  it("renders an image-block tool result as an inline <img>", () => {
    const message: TranscriptMessage = {
      $kind: "tool",
      id: "t1",
      createdAt: "2026-07-01T00:00:00Z",
      toolCallId: "tc1",
      summary:
        "[image-block]\npath: test.png\nsize: 100 bytes\nmime: image/png\ndimensions: 1x1\ndata_url: data:image/png;base64,iVBORw0KGgo=",
      isError: false,
      pinned: false,
    } as TranscriptMessage;

    const html = renderToStaticMarkup(<TranscriptBubble message={message} />);

    // The image-block should render an <img> tag with the data URL as src.
    expect(html).toContain("<img");
    expect(html).toContain('src="data:image/png;base64,iVBORw0KGgo="');
    expect(html).toContain('alt="test.png"');
    // Should NOT show the raw [image-block] prefix.
    expect(html).not.toContain("[image-block]");
  });

  it("renders a non-image tool result as plain text (no <img>)", () => {
    const message: TranscriptMessage = {
      $kind: "tool",
      id: "t2",
      createdAt: "2026-07-01T00:00:00Z",
      toolCallId: "tc2",
      summary: "File read successfully: 42 lines",
      isError: false,
      pinned: false,
    } as TranscriptMessage;

    const html = renderToStaticMarkup(<TranscriptBubble message={message} />);

    expect(html).not.toContain("<img");
    expect(html).toContain("File read successfully");
  });
});

describe("TranscriptBubble prose restyle (parity spec §4.3)", () => {
  it("renders compact user prose without a redundant USER trust label", () => {
    const message: TranscriptMessage = {
      $kind: "user",
      id: "u1",
      createdAt: "2026-07-01T00:00:00Z",
      content: "Hello, world.",
      trust: {
        trust: "user",
        source: "desktop",
        instructionsAreData: false,
        contentHash: "hash",
      },
      pinned: false,
    } as TranscriptMessage;

    const html = renderToStaticMarkup(<TranscriptBubble message={message} />);

    expect(html).toContain("text-[14px]");
    expect(html).toContain("leading-[1.55]");
    expect(html).not.toContain(">User<");
  });

  it("renders assistant prose at the same compact reading scale", () => {
    const message: TranscriptMessage = {
      $kind: "assistant",
      id: "a1",
      createdAt: "2026-07-01T00:00:00Z",
      content: "Let me look at that.",
      toolCalls: [],
      pinned: false,
    } as TranscriptMessage;

    const html = renderToStaticMarkup(<TranscriptBubble message={message} />);

    expect(html).toContain("text-[14px]");
    expect(html).toContain("leading-[1.55]");
  });
});

describe("TranscriptBubble message actions", () => {
  it("keeps copy actions keyboard-discoverable", () => {
    const message: TranscriptMessage = {
      $kind: "assistant",
      id: "a-actions",
      createdAt: "2026-07-01T00:00:00Z",
      content: "Copy me.",
      toolCalls: [],
      pinned: false,
    } as TranscriptMessage;

    const html = renderToStaticMarkup(<TranscriptBubble message={message} />);

    expect(html).toContain('aria-label="Copy message"');
    expect(html).toContain("group-focus-within:opacity-100");
    expect(html).toContain("focus-visible:bg-panel-2");
    expect(html).toContain("focus-visible:outline-focus");
    expect(html).toContain("opacity-100");
    expect(html).toContain("[@media(hover:hover)]:opacity-0");
  });
});

describe("TranscriptBubble density", () => {
  const userMsg: TranscriptMessage = {
    $kind: "user",
    id: "u2",
    createdAt: "2026-07-01T00:00:00Z",
    content: "Hello density",
    trust: { trust: "user", source: "desktop", instructionsAreData: false, contentHash: "h" },
    pinned: false,
  } as TranscriptMessage;

  it("exposes data-density and varies padding/font/gap per policy", () => {
    const compact = renderToStaticMarkup(<TranscriptBubble message={userMsg} density="summary" />);
    const normal = renderToStaticMarkup(<TranscriptBubble message={userMsg} density="normal" />);
    const verbose = renderToStaticMarkup(<TranscriptBubble message={userMsg} density="verbose" />);

    expect(compact).toContain('data-density="summary"');
    expect(normal).toContain('data-density="normal"');
    expect(verbose).toContain('data-density="verbose"');
    expect(compact).not.toBe(normal);
    expect(normal).not.toBe(verbose);
    // height/padding/font/gap controls
    expect(compact).toContain("px-3 py-2");
    expect(verbose).toContain("py-3.5");
    expect(compact).toContain("text-[13px]");
    expect(verbose).toContain("text-[15px]");
  });

  it("supports compact/comfortable aliases", () => {
    const c = renderToStaticMarkup(
      <TranscriptBubble message={userMsg} density={"compact" as never} />,
    );
    const cf = renderToStaticMarkup(
      <TranscriptBubble message={userMsg} density={"comfortable" as never} />,
    );
    expect(c).toContain('data-density="summary"');
    expect(cf).toContain('data-density="verbose"');
  });
});

describe("TranscriptGroupView density", () => {
  it("threads density into processing and clarification groups", async () => {
    const { groupTranscriptMessages } = await import("@rightkit/agent-ui-core/lib/transcriptGroups");
    const { default: TranscriptGroupView } = await import("./TranscriptBubble.js");
    const msgs: TranscriptMessage[] = [
      {
        $kind: "assistant",
        id: "a1",
        createdAt: "2026-07-01T00:00:00Z",
        content: "thinking",
        toolCalls: [{ id: "tc1", name: "read_file", argumentsJson: "{}" }],
        pinned: false,
      } as TranscriptMessage,
      {
        $kind: "tool",
        id: "t1",
        createdAt: "2026-07-01T00:00:00Z",
        toolCallId: "tc1",
        summary: "ok",
        isError: false,
        pinned: false,
      } as TranscriptMessage,
    ];
    const summaryGroups = groupTranscriptMessages(msgs, null, "summary");
    const verboseGroups = groupTranscriptMessages(msgs, null, "verbose");
    // summary hides successful tool results
    expect(summaryGroups[0].kind).toBe("assistant:processing");
    if (summaryGroups[0].kind === "assistant:processing") {
      expect(summaryGroups[0].toolResults.length).toBe(0);
    }
    if (verboseGroups[0].kind === "assistant:processing") {
      expect(verboseGroups[0].toolResults.length).toBe(1);
    }
    // rendering passes density
    const html = renderToStaticMarkup(
      <TranscriptGroupView group={summaryGroups[0]} density="summary" />,
    );
    expect(html).toContain('data-density="summary"');
  });
});
