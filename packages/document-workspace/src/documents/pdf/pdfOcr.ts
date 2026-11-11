import * as pdfjs from "pdfjs-dist";
import type { PDFPageProxy } from "pdfjs-dist";
import { readPdfPageText } from "./usePdfSearch.js";

export type OcrRect = {
  x: number;
  y: number;
  width: number;
  height: number;
};

export type OcrLine = {
  text: string;
  rect: OcrRect;
  // Windows exposes no confidence on OcrLine/OcrWord; null means unavailable, not low confidence.
  confidence: number | null;
};

// These names are present in older pdf.js operator lists; current pdf.js folds JPEG painting into
// paintImageXObject, so keep the compatibility check without adding undefined to the operator set.
const paintJpegXObject = (pdfjs.OPS as typeof pdfjs.OPS & { paintJpegXObject?: number })
  .paintJpegXObject;
const IMAGE_OPERATOR_CODES = new Set<number>([
  pdfjs.OPS.paintImageXObject,
  ...(paintJpegXObject === undefined ? [] : [paintJpegXObject]),
  pdfjs.OPS.paintInlineImageXObject,
  pdfjs.OPS.paintImageMaskXObject,
]);

/** These prefixes are a cross-language contract with src-tauri/crates/ocr/src/lib.rs; they must not drift. */
export const WINDOWS_OCR_UNAVAILABLE_PREFIX = "windows-ocr-unavailable:";
export const MACOS_VISION_UNAVAILABLE_PREFIX = "macos-vision-unavailable:";
export const OCR_UNSUPPORTED_PLATFORM_PREFIX = "ocr-unsupported-platform:";

/**
 * Fast eligibility check used while browsing. It reads only the streamed text layer, so opening
 * a document or changing pages never asks pdf.js to decode every page's operator list.
 */
export async function isTextlessPage(page: PDFPageProxy): Promise<boolean> {
  const text = await readPdfPageText(page);
  return text.trim() === "";
}

/**
 * Scanned-page recognition is offered only when there is no usable text and the page paints an
 * image: streamed text is authoritative because WKWebView's pdf.js worker can leave a parallel
 * getTextContent() request empty even while the visible streamed text layer has content, and the
 * image check prevents genuinely blank pages from being sent to OCR.
 */
export async function isProbablyScannedPage(page: PDFPageProxy): Promise<boolean> {
  if (!(await isTextlessPage(page))) return false;

  const operatorList = await page.getOperatorList();
  return operatorList.fnArray.some((operator) => IMAGE_OPERATOR_CODES.has(operator));
}

/**
 * Rasterizes one PDF page at 300 DPI by default. 300 DPI is the standard floor for reliable text
 * recognition on scanned documents; pdf.js's default page space is 72 DPI, so the scale converts
 * directly from points to the requested pixel density.
 */
export async function renderPageToPng(
  page: PDFPageProxy,
  dpi = 300,
  rotation?: number,
): Promise<Blob> {
  // `rotation` is the ABSOLUTE total rotation (page rotation + the viewer's), because pdf.js
  // treats getViewport's rotation as absolute rather than additive. It is threaded in here rather
  // than by wrapping the page: a derived or proxied page object breaks page.render(), which writes
  // private class fields on `this` and throws "Cannot write private member #pendingCleanup to an
  // object whose class did not declare it".
  const viewport = page.getViewport(
    rotation === undefined ? { scale: dpi / 72 } : { scale: dpi / 72, rotation },
  );
  const canvas = document.createElement("canvas");
  canvas.width = Math.ceil(viewport.width);
  canvas.height = Math.ceil(viewport.height);
  try {
    const context = canvas.getContext("2d");
    if (!context) throw new Error("Cannot rasterize PDF page: canvas 2D context unavailable.");

    // OCR needs a flat page raster, never interactive annotations. Print intent avoids pdf.js's
    // display-only requestAnimationFrame scheduler, which WKWebView pauses for minimized windows.
    // Disabling annotations also gives OCR a distinct pdf.js intent-state key from thumbnail renders,
    // which use print intent with annotations enabled on the same cached PDFPageProxy.
    await page.render({
      canvas,
      canvasContext: context,
      viewport,
      intent: "print",
      annotationMode: pdfjs.AnnotationMode.DISABLE,
    }).promise;
    return await new Promise<Blob>((resolve, reject) => {
      canvas.toBlob((blob) => {
        if (blob) resolve(blob);
        else reject(new Error("Cannot rasterize PDF page: PNG encoding unavailable."));
      }, "image/png");
    });
  } finally {
    // Drop backing storage promptly: OCR can process long documents, so retaining one 300 DPI
    // canvas per in-flight page is enough to create a large avoidable native allocation.
    canvas.width = 0;
    canvas.height = 0;
  }
}

export { ocrPage, ocrAvailable, ocrBackendMissing } from "./pdfBridge.js";

export function isBackendUnavailable(message: string): boolean {
  return (
    message.startsWith(WINDOWS_OCR_UNAVAILABLE_PREFIX) ||
    message.startsWith(MACOS_VISION_UNAVAILABLE_PREFIX) ||
    message.startsWith(OCR_UNSUPPORTED_PLATFORM_PREFIX)
  );
}
