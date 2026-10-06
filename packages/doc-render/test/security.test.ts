import { describe, expect, it } from "vitest";
import { safeImageHref, safeLinkHref, sanitizeHtml, sanitizeMarkdownHtml } from "../src/security.js";
import { parseToTree } from "../src/parseMarkdown.js";

describe("doc-render security", () => {
  it("accepts only explicit web and mail protocols", () => {
    expect(safeLinkHref("https://example.com/a")).toBe("https://example.com/a");
    expect(safeLinkHref("mailto:user@example.com")).toBe("mailto:user@example.com");
    expect(safeLinkHref("javascript:alert(1)")).toBeUndefined();
    expect(safeLinkHref(" data:text/html,<script>alert(1)</script>")).toBeUndefined();
    expect(safeLinkHref("//evil.example")).toBeUndefined();
    expect(safeLinkHref("https://example.com/\u0000x")).toBeUndefined();
  });

  it("keeps relative images but rejects executable image URLs", () => {
    expect(safeImageHref("images/a.png")).toBe("images/a.png");
    expect(safeImageHref("data:image/svg+xml,<svg/onload=alert(1)>")).toBeUndefined();
    expect(safeImageHref("javascript:alert(1)")).toBeUndefined();
  });

  it("strips executable tags, events, styles and unsafe attributes", () => {
    const html = '<p onclick="alert(1)" style="background:url(javascript:1)">ok</p><script>alert(1)</script><a href="javascript:alert(2)">bad</a><a href="https://example.com">good</a>';
    const clean = sanitizeHtml(html);
    expect(clean).toContain("ok");
    expect(clean).toContain("https://example.com");
    expect(clean).not.toMatch(/script|onclick|style|javascript:/i);
    expect(sanitizeMarkdownHtml("<iframe src=evil>bad</iframe>")).not.toMatch(/iframe|evil|bad/i);
  });

  it("parses GFM source without evaluating raw HTML", () => {
    const tree = parseToTree("# Title\n\n| A | B |\n| - | - |\n| 1 | 2 |\n\n<script>alert(1)</script>");
    expect(tree.children.some((node) => node.type === "heading")).toBe(true);
    expect(tree.children.some((node) => node.type === "table")).toBe(true);
  });
});
