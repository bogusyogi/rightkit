import type { PageViewport } from "pdfjs-dist";
import type { PdfColor, PdfPoint, PdfRect, PdfTextAppearance } from "./pdfEditModel.js";
import type { OcrLine } from "./pdfOcr.js";

export interface PdfOcrHit {
  index: number;
  line: OcrLine;
}

export interface PdfOcrCorrectionGeometry {
  lineId: string;
  rect: PdfRect;
  anchor: PdfPoint;
  appearance: PdfTextAppearance;
}

export function findOcrLineAt(
  lines: readonly OcrLine[],
  normalizedX: number,
  normalizedY: number,
): PdfOcrHit | null {
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (
      normalizedX >= line.rect.x &&
      normalizedX <= line.rect.x + line.rect.width &&
      normalizedY >= line.rect.y &&
      normalizedY <= line.rect.y + line.rect.height
    ) {
      return { index, line };
    }
  }
  return null;
}

function hashText(input: string): string {
  let hash = 0x811c9dc5;
  for (let index = 0; index < input.length; index += 1) {
    hash ^= input.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, "0");
}

export function ocrLineId(page: number, index: number, line: OcrLine): string {
  return `${page}:${index}:${hashText(JSON.stringify([line.text, line.rect]))}`;
}

export function ocrLineCorrectionGeometry(
  page: number,
  index: number,
  line: OcrLine,
  viewport: PageViewport,
): PdfOcrCorrectionGeometry {
  const left = line.rect.x * viewport.width;
  const top = line.rect.y * viewport.height;
  const right = (line.rect.x + line.rect.width) * viewport.width;
  const bottom = (line.rect.y + line.rect.height) * viewport.height;
  const corners = [
    viewport.convertToPdfPoint(left, top),
    viewport.convertToPdfPoint(right, top),
    viewport.convertToPdfPoint(left, bottom),
    viewport.convertToPdfPoint(right, bottom),
  ];
  const xs = corners.map(([x]) => x);
  const ys = corners.map(([, y]) => y);
  const rect = {
    x: Math.min(...xs),
    y: Math.min(...ys),
    width: Math.max(...xs) - Math.min(...xs),
    height: Math.max(...ys) - Math.min(...ys),
  };
  const sizePt = Math.max(6, Math.min(72, rect.height * 0.72));
  return {
    lineId: ocrLineId(page, index, line),
    rect,
    anchor: { x: rect.x, y: rect.y + rect.height * 0.18 },
    appearance: {
      fontId: "source-serif-4",
      family: "Source Serif 4",
      sizePt,
      color: { r: 0, g: 0, b: 0, alpha: 1 },
    },
  };
}

/** Finds one dominant local color while tolerating dark glyph pixels over a uniform scan. */
export function detectNearUniformBackground(data: Uint8ClampedArray): PdfColor | null {
  const buckets = new Map<string, { count: number; r: number; g: number; b: number }>();
  let opaque = 0;
  for (let index = 0; index + 3 < data.length; index += 4) {
    if (data[index + 3] < 128) continue;
    opaque += 1;
    const key = `${data[index] >> 4}:${data[index + 1] >> 4}:${data[index + 2] >> 4}`;
    const bucket = buckets.get(key) ?? { count: 0, r: 0, g: 0, b: 0 };
    bucket.count += 1;
    bucket.r += data[index];
    bucket.g += data[index + 1];
    bucket.b += data[index + 2];
    buckets.set(key, bucket);
  }
  if (opaque === 0) return null;
  const dominant = [...buckets.values()].sort((a, b) => b.count - a.count)[0];
  if (!dominant || dominant.count / opaque < 0.6) return null;
  return {
    r: dominant.r / dominant.count / 255,
    g: dominant.g / dominant.count / 255,
    b: dominant.b / dominant.count / 255,
    alpha: 1,
  };
}

/**
 * Dominant background color under a CSS-pixel rect of a page slot, read from that slot's render
 * canvas (which is rasterized at devicePixelRatio, so CSS px are scaled by canvas/slot width).
 * Used to mask the original glyphs under the inline source-text editor; `null` when the area is
 * not near-uniform (e.g. text over an image), in which case the caller falls back to white.
 */
export function sampleCanvasRectBackground(
  canvas: HTMLCanvasElement,
  rectCss: PdfRect,
  slotWidthCss: number,
): PdfColor | null {
  if (slotWidthCss <= 0 || canvas.width <= 0) return null;
  const context = canvas.getContext("2d", { willReadFrequently: true });
  if (!context) return null;
  const ratio = canvas.width / slotWidthCss;
  const x = Math.max(0, Math.floor(rectCss.x * ratio));
  const y = Math.max(0, Math.floor(rectCss.y * ratio));
  const width = Math.max(1, Math.min(canvas.width - x, Math.ceil(rectCss.width * ratio)));
  const height = Math.max(1, Math.min(canvas.height - y, Math.ceil(rectCss.height * ratio)));
  try {
    return detectNearUniformBackground(context.getImageData(x, y, width, height).data);
  } catch {
    return null;
  }
}

export function sampleOcrBackground(canvas: HTMLCanvasElement, line: OcrLine): PdfColor | null {
  const context = canvas.getContext("2d", { willReadFrequently: true });
  if (!context) return null;
  const x = Math.max(0, Math.floor(line.rect.x * canvas.width));
  const y = Math.max(0, Math.floor(line.rect.y * canvas.height));
  const width = Math.max(1, Math.min(canvas.width - x, Math.ceil(line.rect.width * canvas.width)));
  const height = Math.max(
    1,
    Math.min(canvas.height - y, Math.ceil(line.rect.height * canvas.height)),
  );
  try {
    return detectNearUniformBackground(context.getImageData(x, y, width, height).data);
  } catch {
    return null;
  }
}

export function applyOcrCorrections(
  source: ReadonlyMap<number, OcrLine[]>,
  corrections: readonly {
    kind: string;
    page?: number;
    lineId?: string;
    text?: string;
  }[],
): Map<number, OcrLine[]> {
  const latest = new Map<string, string>();
  for (const command of corrections) {
    if (command.kind === "correctOcrText" && command.lineId && command.text !== undefined) {
      latest.set(command.lineId, command.text);
    }
  }
  const result = new Map<number, OcrLine[]>();
  for (const [page, lines] of source) {
    result.set(
      page,
      lines.map((line, index) => {
        const text = latest.get(ocrLineId(page, index, line));
        return text === undefined ? line : { ...line, text };
      }),
    );
  }
  return result;
}
