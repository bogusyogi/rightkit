// Characterization: the default PdfViewer markup must stay byte-identical across additive changes.
// The snapshot in test/__snapshots__ was recorded before the 0.1.6 additions and is not regenerated.
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { PdfViewer } from "../src/index.js";

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
});
