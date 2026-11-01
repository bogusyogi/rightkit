import { describe, expect, it } from "vitest";
import { MarkdownTextBuffer } from "./MarkdownTextBuffer";

describe("MarkdownTextBuffer — raw/logical decomposition", () => {
  it("still rejects a line-ending vector that does not match the logical newlines", () => {
    const buffer = MarkdownTextBuffer.fromParts({
      logicalSource: "a\nb\n",
      lineEndings: ["\n"],
      preferredLineEnding: "\n",
      hasBom: false,
    });
    expect(() => buffer.toRaw()).toThrow(/newline count mismatch/);
  });
});
