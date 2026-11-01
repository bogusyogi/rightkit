import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { TranscriptBubble } from "@rightkit/agent-ui-workbench/components/workbench/TranscriptBubble";
import { detectComposerMention } from "../app/composerBarHelpers";
import type { TranscriptMessage } from "@rightkit/agent-ui-core/lib/types";
import type { ReferenceObject } from "@rightkit/agent-ui-core/lib/types/references";

describe("TranscriptBubble user-with-attachments", () => {
  it("renders the reference chip strip when the user message has attachments", () => {
    const message: TranscriptMessage = {
      $kind: "user",
      id: "u-attach-1",
      createdAt: "2026-07-20T00:00:00Z",
      content: "look at these",
      pinned: false,
      attachments: [
        {
          id: "ref-1",
          kind: "image",
          destination: "shot.png",
          mime: "image/png",
          filename: "shot.png",
          sizeBytes: 2048,
          provenance: {
            source: "composer.attachment",
            createdAt: "2026-07-20T00:00:00Z",
            trust: "user",
          },
          actions: ["remove"],
        },
        {
          id: "ref-2",
          kind: "generated_document",
          destination: "/tmp/notes.md",
          mime: "text/markdown",
          filename: "notes.md",
          sizeBytes: 1024,
          provenance: {
            source: "workspace_resolver",
            createdAt: "2026-07-20T00:00:00Z",
            trust: "workspaceTrusted",
          },
          actions: ["open", "preview"],
        },
      ],
    } as TranscriptMessage;

    const html = renderToStaticMarkup(<TranscriptBubble message={message} />);

    // Chip strip should render in transcript mode.
    expect(html).toContain('data-reference-strip="true"');
    expect(html).toContain('data-reference-strip-mode="transcript"');
    // Both chips present.
    expect(html).toContain('data-reference-id="ref-1"');
    expect(html).toContain('data-reference-id="ref-2"');
    // Filenames visible.
    expect(html).toContain("shot.png");
    expect(html).toContain("notes.md");
  });

  it("keeps an image preview available when attachment bytes are staged locally", () => {
    const previewAttachment: ReferenceObject = {
      id: "ref-preview",
      kind: "image",
      destination: "attachment://ref-preview/shot.png",
      mime: "image/png",
      filename: "shot.png",
      sizeBytes: 12,
      provenance: {
        source: "composer.attachment",
        createdAt: "2026-07-20T00:00:00Z",
        trust: "user",
      },
      previewUrl: "data:image/png;base64,AAAA",
      actions: ["remove", "preview"],
    };
    const message: TranscriptMessage = {
      $kind: "user",
      id: "u-preview-attach",
      createdAt: "2026-07-20T00:00:00Z",
      content: "inspect this image",
      pinned: false,
      attachments: [previewAttachment],
    } as TranscriptMessage;

    const html = renderToStaticMarkup(<TranscriptBubble message={message} />);
    expect(html).toContain('data-reference-id="ref-preview"');
    expect(html).toContain('src="data:image/png;base64,AAAA"');
    expect(html).toContain("shot.png");
  });

  it("parses quoted paths in file mentions without sending delimiters to search", () => {
    expect(detectComposerMention("inspect @file:`src/feature flags.ts")).toMatchObject({
      kind: "file",
      query: "src/feature flags.ts",
      start: 8,
      end: 35,
    });
    expect(detectComposerMention("find @symbol:renderRow")).toMatchObject({
      kind: "symbol",
      query: "renderRow",
    });
  });

  it("renders no chip strip when the user message has no attachments", () => {
    const message: TranscriptMessage = {
      $kind: "user",
      id: "u-no-attach",
      createdAt: "2026-07-20T00:00:00Z",
      content: "plain text",
      pinned: false,
    } as TranscriptMessage;

    const html = renderToStaticMarkup(<TranscriptBubble message={message} />);
    expect(html).not.toContain("data-reference-strip");
  });

  it("renders no chip strip when the user message has an empty attachments array", () => {
    const message: TranscriptMessage = {
      $kind: "user",
      id: "u-empty-attach",
      createdAt: "2026-07-20T00:00:00Z",
      content: "plain text",
      pinned: false,
      attachments: [],
    } as TranscriptMessage;

    const html = renderToStaticMarkup(<TranscriptBubble message={message} />);
    expect(html).not.toContain("data-reference-strip");
  });
});
