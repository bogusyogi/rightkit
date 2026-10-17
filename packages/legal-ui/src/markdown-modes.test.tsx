import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { LegalDocumentBody } from "./LegalDocumentBody.js";

// Expected markup for the default (legacy) mode is the 0.3.0 (HEAD) LegalDocumentBody output: HTML comments
// are text, list continuations start a new paragraph, two-space line breaks are dropped, and paragraph
// lines join with a space before inline parsing.
const render = (content: string, markdown?: "legacy" | "extended"): string =>
  renderToStaticMarkup(<LegalDocumentBody content={content} format="markdown" markdown={markdown} />);

describe("LegalDocumentBody default (legacy 0.3.0) markdown", () => {
  it("keeps HTML comments as text", () => {
    expect(render("<!-- internal -->\n# Terms\n\nBody text")).toBe(
      "<p>&lt;!-- internal --&gt;</p><h2>Terms</h2><p>Body text</p>",
    );
  });

  it("keeps indented list continuations as a separate paragraph", () => {
    expect(render("- one\n  more\n- two")).toBe(
      "<ul><li>one</li></ul><p>more</p><ul><li>two</li></ul>",
    );
  });

  it("drops two-space hard breaks, joining lines with a space", () => {
    expect(render("First  \nSecond")).toBe("<p>First Second</p>");
  });

  it("renders emphasis spanning a soft line break", () => {
    expect(render("**first\nsecond**")).toBe("<p><strong>first second</strong></p>");
  });

  it("treats omitted markdown option as legacy", () => {
    const input = "<!-- c -->\n- a\n  b\nX  \nY **z**";
    expect(render(input)).toBe(render(input, "legacy"));
  });
});

describe("LegalDocumentBody markdown: extended", () => {
  it("strips HTML comments", () => {
    expect(render("<!-- internal -->\n# Terms\n\nBody text", "extended")).toBe(
      "<h2>Terms</h2><p>Body text</p>",
    );
  });

  it("keeps indented list continuations inside the item", () => {
    expect(render("- one\n  more\n- two", "extended")).toBe(
      "<ul><li>one more</li><li>two</li></ul>",
    );
  });

  it("renders two-space hard breaks as line breaks", () => {
    expect(render("First  \nSecond", "extended")).toBe("<p>First<br/>Second</p>");
  });

  it("renders emphasis spanning a soft line break", () => {
    expect(render("**first\nsecond**", "extended")).toBe("<p><strong>first second</strong></p>");
  });
});
