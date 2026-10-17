import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { LegalDocumentBody, type LegalContentFormat, type LegalDocumentBodyProps } from "./index.js";

describe("public LegalDocumentBody", () => {
  it("renders markdown via package root export & skips host title/comments", () => {
    const props: LegalDocumentBodyProps = {
      content: "<!-- internal -->\n# Terms\n\n## Scope\nFirst  \nSecond **bold** `code`\n\n- item\n  continuation",
      format: "markdown", title: "Terms", markdown: "extended",
    };
    const html = renderToStaticMarkup(<LegalDocumentBody {...props} />);
    expect(html).toBe("<h3>Scope</h3><p>First<br/>Second <strong>bold</strong> <code>code</code></p><ul><li>item continuation</li></ul>");
  });

  it("escapes HTML & blocks unsafe link schemes", () => {
    const html = renderToStaticMarkup(<LegalDocumentBody content={'<script>bad</script> [bad](javascript:bad) [ok](https://example.com)'} format="markdown" />);
    expect(html).toContain("&lt;script&gt;");
    expect(html).not.toContain('href="javascript:');
    expect(html).toContain('href="https://example.com"');
  });

  it("preserves text bytes including comments & line breaks in text mode", () => {
    const format: LegalContentFormat = "text";
    const html = renderToStaticMarkup(<LegalDocumentBody content={"# Terms\n<!-- keep -->\n"} format={format} title="Terms" />);
    expect(html).toBe('<pre class="rk-legal-gate__doc-text"># Terms\n&lt;!-- keep --&gt;\n</pre>');
  });
});
