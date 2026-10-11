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
