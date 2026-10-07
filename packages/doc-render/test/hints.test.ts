import { describe, expect, it } from "vitest";
import { buildBlocks, parseToTree } from "../src/index.js";

const doc = (prefix: string) => `<!-- ${prefix}: faq -->\n\n## Q: What?\n\nThis.\n\n## Q: Why?\n\nThat.\n`;
const kinds = (src: string, prefixes?: string[]) =>
  buildBlocks(parseToTree(src).children, prefixes ? { hintPrefixes: prefixes } : {}).map((b) => (b.kind === "semantic" ? b.hit.kind : "plain"));

describe("explicit hint prefixes", () => {
  it("accepts doc: and viewright: by default", () => {
    expect(kinds(doc("doc"))).toContain("faq");
    expect(kinds(doc("viewright"))).toContain("faq");
  });

  it("restricts hints to the given prefixes", () => {
    expect(kinds(doc("viewright"), ["viewright"])).toContain("faq");
    const blocks = buildBlocks(parseToTree(doc("doc")).children, { hintPrefixes: ["viewright"] });
    expect(blocks.some((b) => b.kind === "semantic" && b.hit.forced)).toBe(false);
  });
});
