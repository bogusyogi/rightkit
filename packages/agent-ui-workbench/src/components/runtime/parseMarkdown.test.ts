// D1 — MarkdownArtifactView renderer tests.

import { describe, expect, it } from "vitest";
import { parseMarkdown } from "./MarkdownArtifactView";

describe("parseMarkdown", () => {
  it("parses headings at all levels", () => {
    const blocks = parseMarkdown("# Title\n\n## Subhead\n\n### Sub-sub\n");
    expect(blocks[0]).toMatchObject({ kind: "heading", level: 1, text: "Title" });
    expect(blocks[1]).toMatchObject({ kind: "heading", level: 2, text: "Subhead" });
    expect(blocks[2]).toMatchObject({ kind: "heading", level: 3, text: "Sub-sub" });
  });

  it("parses fenced code blocks", () => {
    const blocks = parseMarkdown("```ts\nconst x = 1;\n```\n");
    expect(blocks[0]).toMatchObject({ kind: "code", lang: "ts", text: "const x = 1;" });
  });

  it("parses unordered lists", () => {
    const blocks = parseMarkdown("- one\n- two\n- three\n");
    expect(blocks[0]).toMatchObject({
      kind: "list",
      items: ["one", "two", "three"],
    });
  });

  it("parses paragraphs across multiple lines", () => {
    const blocks = parseMarkdown("first line\nsecond line\n");
    expect(blocks[0]).toMatchObject({
      kind: "paragraph",
      text: "first line second line",
    });
  });

  it("parses blockquotes", () => {
    const blocks = parseMarkdown("> quoted line\n");
    expect(blocks[0]).toMatchObject({ kind: "quote", text: "quoted line" });
  });

  it("returns an empty array for empty input", () => {
    expect(parseMarkdown("")).toEqual([]);
    expect(parseMarkdown("   \n\n   \n")).toEqual([]);
  });
});
