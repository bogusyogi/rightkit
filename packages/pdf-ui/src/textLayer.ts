// Text layer construction shared by the viewer and headless hosts. The default source is
// `getTextContent()`, as the viewer has always used. `streamTextContent()` returns a ReadableStream
// that pdf.js TextLayer consumes chunk by chunk; some WebKit builds finish a stream-backed layer
// when a parallel `getTextContent()` stays empty, so hosts that need that path opt in.
import { TextLayer, type PDFPageProxy, type PageViewport } from "pdfjs-dist";

export type PdfTextContentMode = "getTextContent" | "streamTextContent";

/** The `textContentSource` TextLayer should render from. */
export async function pdfTextContentSource(
  page: PDFPageProxy,
  mode: PdfTextContentMode = "getTextContent",
): Promise<ReadableStream | Awaited<ReturnType<PDFPageProxy["getTextContent"]>>> {
  if (mode === "streamTextContent") return page.streamTextContent();
  return page.getTextContent();
}

/** Build and render a pdf.js TextLayer into `container`. Defaults match the viewer exactly. */
export async function renderPdfTextLayer(
  page: PDFPageProxy,
  container: HTMLElement,
  viewport: PageViewport,
  mode: PdfTextContentMode = "getTextContent",
): Promise<TextLayer> {
  const textLayer = new TextLayer({ textContentSource: await pdfTextContentSource(page, mode), container, viewport });
  await textLayer.render();
  return textLayer;
}

/** The text of a page as the viewer's find reads it: item strings joined by a space. In
 *  `"streamTextContent"` mode the stream is read to its end, so find sees the same text source the
 *  text layer renders from. */
export async function readPdfPageText(page: PDFPageProxy, mode: PdfTextContentMode = "getTextContent"): Promise<string> {
  const parts: string[] = [];
  const collect = (items: ReadonlyArray<unknown>) => {
    for (const item of items) parts.push(typeof item === "object" && item !== null && "str" in item ? String((item as { str: unknown }).str) : "");
  };
  if (mode === "streamTextContent") {
    const reader = page.streamTextContent().getReader();
    try {
      for (;;) {
        const chunk = await reader.read();
        if (chunk.done) break;
        collect(chunk.value.items);
      }
    } finally {
      reader.releaseLock();
    }
  } else {
    collect((await page.getTextContent()).items);
  }
  return parts.join(" ");
}
