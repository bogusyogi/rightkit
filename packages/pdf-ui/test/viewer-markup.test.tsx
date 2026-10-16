// Characterization: the default PdfViewer markup must stay byte-identical across additive changes.
// The snapshot in test/__snapshots__ was recorded before the 0.1.6 additions and is not regenerated.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { readFileSync } from "node:fs";
import { PdfTruncationNotice, PdfViewer } from "../src/index.js";

const source = { kind: "url" as const, url: "https://example.test/doc.pdf" };

describe("PdfViewer default markup", () => {
  it("renders the default toolbar, find bar and page region", () => {
    expect(renderToStaticMarkup(<PdfViewer source={source} />)).toMatchSnapshot();
  });

  it("renders with the find bar hidden", () => {
    expect(renderToStaticMarkup(<PdfViewer source={source} findBar={false} />)).toMatchSnapshot();
  });

  it("renders with the toolbar hidden and a host toolbar slot", () => {
    expect(renderToStaticMarkup(<PdfViewer source={source} hideToolbar className="host" toolbar={<b>x</b>} />)).toMatchSnapshot();
  });

  it("renders the same markup whether or not documentOptions is passed", () => {
    expect(renderToStaticMarkup(<PdfViewer source={source} documentOptions={{ wasmUrl: "/pdfjs/", cMapUrl: "/cmaps/" }} />)).toBe(renderToStaticMarkup(<PdfViewer source={source} />));
  });

  it("renders the outline panel and a render cap from the initial view", () => {
    expect(renderToStaticMarkup(<PdfViewer source={source} initialView={{ page: 2, scale: 1.5, fit: null, rotation: 90, panel: "outline" }} maxRenderPages={3} />)).toMatchSnapshot();
  });

  it("renders the same markup whether or not truncationNotice is passed (no pages known yet)", () => {
    expect(renderToStaticMarkup(<PdfViewer source={source} maxRenderPages={3} truncationNotice="x" />)).toBe(renderToStaticMarkup(<PdfViewer source={source} maxRenderPages={3} />));
  });
});

describe("PdfTruncationNotice", () => {
  it("renders nothing without a notice or when no page is hidden", () => {
    expect(renderToStaticMarkup(<PdfTruncationNotice numPages={10} shown={3} notice={undefined} />)).toBe("");
    expect(renderToStaticMarkup(<PdfTruncationNotice numPages={3} shown={3} notice="hidden" />)).toBe("");
    expect(renderToStaticMarkup(<PdfTruncationNotice numPages={0} shown={0} notice="hidden" />)).toBe("");
  });
  it("renders a node, or a function of shown and total, in a status paragraph", () => {
    expect(renderToStaticMarkup(<PdfTruncationNotice numPages={10} shown={3} notice="Capped" />)).toBe('<p class="rk-pdf-notice" role="status">Capped</p>');
    expect(renderToStaticMarkup(<PdfTruncationNotice numPages={10} shown={3} notice={({ shown, total }) => `Showing first ${shown} of ${total} pages`} />)).toBe('<p class="rk-pdf-notice" role="status">Showing first 3 of 10 pages</p>');
  });
});

describe("surface colours", () => {
  const css = readFileSync(new URL("../src/styles.css", import.meta.url), "utf8");
  it("read CSS variables and fall back to the previous literals", () => {
    expect(css).toContain("color:var(--rk-pdf-ink,#1d1d1f)");
    expect(css).toContain("background:var(--rk-pdf-bg,#f2f2f2)");
    expect(css).toContain("var(--rk-pdf-chrome-bg,#fff)");
    expect(css).toContain("var(--rk-pdf-border,#d5d5d5)");
    expect(css).toContain("var(--rk-pdf-page-bg,#fff)");
  });
  it("is the file that ships at the package root", () => {
    expect(readFileSync(new URL("../styles.css", import.meta.url), "utf8")).toBe(css);
  });
});
