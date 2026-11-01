import { describe, expect, it } from "vitest";
import type { TranscriptMessage } from "../types";
import {
  createTranscriptRenderCache,
  layoutTranscriptRows,
  projectTranscriptGroups,
  transcriptRenderFingerprint,
  transcriptViewportWindow,
} from "./longTranscriptRender";
import source from "./longTranscriptRender.ts?raw";

const user = (id: string, content: string): TranscriptMessage => ({
  $kind: "user",
  id,
  createdAt: "2026-08-29T00:00:00Z",
  content,
  pinned: false,
});

const assistant = (id: string, content: string): TranscriptMessage => ({
  $kind: "assistant",
  id,
  createdAt: "2026-08-29T00:00:01Z",
  content,
  toolCalls: [],
  pinned: false,
});

describe("long transcript render projection", () => {
  it("reuses the prepared group snapshot while row references and policy hold still", () => {
    const messages = [user("u-1", "Build it"), assistant("a-1", "Done")];
    const input = { messages, pendingQuestion: null, density: "normal" as const };
    const cache = createTranscriptRenderCache();
    const first = projectTranscriptGroups(cache, input);
    const second = projectTranscriptGroups(cache, { ...input, messages: [...messages] });

    expect(second).toBe(first);
    expect(cache.fingerprint).toBe(transcriptRenderFingerprint(input));
  });

  it("invalidates the cached projection when a streamed row is replaced", () => {
    const messages = [user("u-1", "Build it"), assistant("a-1", "Done")];
    const cache = createTranscriptRenderCache();
    const first = projectTranscriptGroups(cache, {
      messages,
      pendingQuestion: null,
      density: "normal",
    });
    const nextMessages = [messages[0]!, assistant("a-1", "Done!")];
    const second = projectTranscriptGroups(cache, {
      messages: nextMessages,
      pendingQuestion: null,
      density: "normal",
    });

    expect(second).not.toBe(first);
    expect(second[1]).not.toBe(first[1]);
  });

  it("keeps layout arithmetic bounded to the row model and overscans the viewport", () => {
    const layout = layoutTranscriptRows(200, new Map([[0, 120], [1, 144]]), 96, 8);
    const window = transcriptViewportWindow(layout, 0, 240, 2);

    expect(layout.rows[0]).toEqual({ index: 0, start: 0, size: 120 });
    expect(layout.rows[1]).toEqual({ index: 1, start: 128, size: 144 });
    expect(layout.totalSize).toBe(200 * 96 + 199 * 8 + 72);
    expect(window.first).toBe(0);
    expect(window.last).toBeLessThan(200);
    expect(window.rows.length).toBe(window.last - window.first);
    expect(window.rows[0]?.index).toBe(0);
  });

  /**
   * Waku's equivalent source tests protect the render boundary rather than
   * trying to prove a syscall is absent at runtime. The React renderer must
   * remain a pure consumer of its prepared transcript snapshot too.
   */
  it("does no external work while preparing the render snapshot", () => {
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
  });
});
