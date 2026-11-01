import { describe, expect, it } from "vitest";

import type { TranscriptMessage } from "./types.js";
import {
  literalFindRegex,
  markdownSearchMatches,
  MAX_TRANSCRIPT_SEARCH_MATCHES,
  searchTranscript,
  TranscriptSearch,
} from "./transcriptSearch.js";

describe("transcript search", () => {
  it("keeps literal search case-insensitive and unicode-safe", () => {
    const regex = literalFindRegex("waku");
    const [matches, limited] = markdownSearchMatches("Waku **waku** WAKU", regex, 20);
    expect(limited).toBe(false);
    expect(matches.map((match) => match.range)).toEqual([
      { start: 0, end: 4 },
      { start: 7, end: 11 },
      { start: 14, end: 18 },
    ]);

    const [unicodeMatches] = markdownSearchMatches("世**界** 世界", literalFindRegex("界"), 20);
    expect(unicodeMatches).toHaveLength(2);
    expect(unicodeMatches.every((match) => match.range.start < match.range.end)).toBe(true);
  });

  it("uses stable markdown block ordinals for reveal targets", () => {
    const [matches, limited] = markdownSearchMatches(
      "first\n\n> needle\n\n```txt\nneedle\n```",
      literalFindRegex("needle"),
      20,
    );
    expect(limited).toBe(false);
    expect(matches).toHaveLength(2);
    expect(matches[0].ordinal).toBe(1 << 16);
    expect(matches[1].ordinal).toBe(2 << 16);
  });

  it("reports when the 20k match cap truncates the result set", () => {
    const source = "x".repeat(MAX_TRANSCRIPT_SEARCH_MATCHES + 1);
    const result = searchTranscript([userMessage("u1", source)], "x");
    expect(result.matches).toHaveLength(MAX_TRANSCRIPT_SEARCH_MATCHES);
    expect(result.limited).toBe(true);
  });

  it("carries message id, virtual row, byte range, and current selection", () => {
    const result = searchTranscript(
      [userMessage("u1", "before"), userMessage("u2", "needle"), userMessage("u3", "after")],
      "needle",
      { originRow: 8, rowIndexForMessage: (index) => index * 4 },
    );
    expect(result.matches[0]).toMatchObject({
      messageIndex: 1,
      rowIndex: 4,
      messageId: "u2",
      text: { range: { start: 0, end: 6 }, ordinal: 0 },
    });
    expect(result.current).toBe(0);
  });

  it("navigates cyclically and exposes the stable off-screen target", () => {
    const search = new TranscriptSearch();
    search.refresh([userMessage("u1", "needle"), userMessage("u2", "needle")], "needle");
    expect(search.target()).toMatchObject({ key: "message-u1", rowIndex: 0, ordinal: 0 });
    search.navigate();
    expect(search.target()).toMatchObject({ key: "message-u2", rowIndex: 1 });
    search.navigate();
    expect(search.target()?.current).toBe(0);
    search.navigate(true);
    expect(search.target()?.current).toBe(1);
  });
});

function userMessage(id: string, content: string): TranscriptMessage {
  return {
    $kind: "user",
    id,
    createdAt: "2026-08-29T00:00:00Z",
    content,
    pinned: false,
  };
}
