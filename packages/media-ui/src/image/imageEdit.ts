import {
  canvasToArrayBuffer,
  mimeForOutputExtension,
  type ImageSaveExtension,
  type ImageSavePayload,
} from "./imageSave.js";
import { fontCssForImageText, type ImageFontOptions } from "./imageFonts.js";
import type { MediaFontOption } from "../adapters.js";

export const MAX_EXPORT_CANVAS_PIXELS = 50_000_000;

export type ImageAdjustments = {
  brightness: number;
  contrast: number;
  saturation: number;
  hue: number;
  grayscale: number;
  sepia: number;
  blur: number;
};

export type CropRect = { x: number; y: number; width: number; height: number };

export type TextLayer = {
  id: string;
  text: string;
  fontFamily?: string;
  x: number;
  y: number;
  width: number;
  size: number;
  color: string;
  bold: boolean;
  angle: number;
};

export type ExportResize =
  | { mode: "original"; value?: number }
  | { mode: "percent"; value: number }
  | { mode: "width"; value: number }
  | { mode: "height"; value: number };

export type ImageEdit = {
  crop: CropRect | null;
  rotation: number;
  flipX: boolean;
  flipY: boolean;
  adjustments: ImageAdjustments;
  text: TextLayer[];
  resize: ExportResize;
};

export const DEFAULT_ADJUSTMENTS: ImageAdjustments = {
  brightness: 100,
  contrast: 100,
  saturation: 100,
  hue: 0,
  grayscale: 0,
  sepia: 0,
  blur: 0,
};

export function initialImageEdit(): ImageEdit {
  return {
    crop: null,
    rotation: 0,
    flipX: false,
    flipY: false,
    adjustments: { ...DEFAULT_ADJUSTMENTS },
    text: [],
    resize: { mode: "original" },
  };
}

export function normalizeEditRotation(rotation: number): number {
  return ((rotation % 360) + 360) % 360;
}

export function hasAdjustmentEdits(adjustments: ImageAdjustments): boolean {
  return (
    adjustments.brightness !== DEFAULT_ADJUSTMENTS.brightness ||
    adjustments.contrast !== DEFAULT_ADJUSTMENTS.contrast ||
    adjustments.saturation !== DEFAULT_ADJUSTMENTS.saturation ||
    adjustments.hue !== DEFAULT_ADJUSTMENTS.hue ||
    adjustments.grayscale !== DEFAULT_ADJUSTMENTS.grayscale ||
    adjustments.sepia !== DEFAULT_ADJUSTMENTS.sepia ||
    adjustments.blur !== DEFAULT_ADJUSTMENTS.blur
  );
}

export function hasImageEdits(edit: ImageEdit): boolean {
  return (
    !!edit.crop ||
    normalizeEditRotation(edit.rotation) !== 0 ||
    edit.flipX ||
    edit.flipY ||
    hasAdjustmentEdits(edit.adjustments) ||
    edit.text.some((t) => t.text.trim())
  );
}

/** Structural equality for edit history; avoids serializing every text layer on each pointer move. */
export function sameImageEdit(a: ImageEdit, b: ImageEdit): boolean {
  return (
    sameCrop(a.crop, b.crop) &&
    a.rotation === b.rotation &&
    a.flipX === b.flipX &&
    a.flipY === b.flipY &&
    sameAdjustments(a.adjustments, b.adjustments) &&
    a.resize.mode === b.resize.mode &&
    a.resize.value === b.resize.value &&
    a.text.length === b.text.length &&
    a.text.every((layer, index) => sameTextLayer(layer, b.text[index]))
  );
}

function sameCrop(a: CropRect | null, b: CropRect | null): boolean {
  return (
    a === b ||
    (!!a && !!b && a.x === b.x && a.y === b.y && a.width === b.width && a.height === b.height)
  );
}

function sameAdjustments(a: ImageAdjustments, b: ImageAdjustments): boolean {
  return (
    a.brightness === b.brightness &&
    a.contrast === b.contrast &&
    a.saturation === b.saturation &&
    a.hue === b.hue &&
    a.grayscale === b.grayscale &&
    a.sepia === b.sepia &&
    a.blur === b.blur
  );
}

function sameTextLayer(a: TextLayer, b: TextLayer | undefined): boolean {
  return (
    !!b &&
    a.id === b.id &&
    a.text === b.text &&
    a.fontFamily === b.fontFamily &&
    a.x === b.x &&
    a.y === b.y &&
    a.width === b.width &&
    a.size === b.size &&
    a.color === b.color &&
    a.bold === b.bold &&
    a.angle === b.angle
  );
}

export function cssFilterForAdjustments(adjustments: ImageAdjustments): string {
  const parts = [
    `brightness(${adjustments.brightness}%)`,
    `contrast(${adjustments.contrast}%)`,
    `saturate(${adjustments.saturation}%)`,
    `hue-rotate(${adjustments.hue}deg)`,
    `grayscale(${adjustments.grayscale}%)`,
    `sepia(${adjustments.sepia}%)`,
  ];
  if (adjustments.blur > 0) parts.push(`blur(${adjustments.blur}px)`);
  return parts.join(" ");
}

export function cropSourceSize(
  image: { width: number; height: number },
  crop: CropRect | null,
): { width: number; height: number } {
  return crop ? { width: Math.max(1, crop.width), height: Math.max(1, crop.height) } : image;
}

export function outputScaleForResize(
  source: { width: number; height: number },
  resize: ExportResize,
): number {
  if (source.width <= 0 || source.height <= 0) return 1;
  if (resize.mode === "percent") return clampScale(resize.value / 100);
  if (resize.mode === "width") return clampScale(resize.value / source.width);
  if (resize.mode === "height") return clampScale(resize.value / source.height);
  return 1;
}

export function exportDimensionsFor(
  source: { width: number; height: number },
  resize: ExportResize,
): { width: number; height: number } {
  const scale = outputScaleForResize(source, resize);
  return {
    width: Math.max(1, Math.round(source.width * scale)),
    height: Math.max(1, Math.round(source.height * scale)),
  };
}

export function validateExportCanvasSize(size: { width: number; height: number }) {
  const pixels = size.width * size.height;
  if (!Number.isFinite(pixels) || pixels > MAX_EXPORT_CANVAS_PIXELS) {
    throw new Error(`image export is too large (${Math.round(pixels).toLocaleString()} pixels)`);
  }
}

function clampScale(scale: number): number {
  if (!Number.isFinite(scale)) return 1;
  return Math.min(8, Math.max(0.01, scale));
}

/**
 * Re-express a recipe authored against a display-sized image in the pixel
 * space of the original it will be rendered from. Crop rectangles and text
 * size/width are stored in display pixels (text x/y are normalized); applying
 * them to a larger original unscaled crops the wrong region and shrinks text.
 */
export function scaleEditToSource(
  edit: ImageEdit,
  editSpace: { width: number; height: number } | null | undefined,
  source: { width: number; height: number },
): ImageEdit {
  if (!editSpace || editSpace.width <= 0 || editSpace.height <= 0) return edit;
  if (editSpace.width === source.width && editSpace.height === source.height) return edit;
  const sx = source.width / editSpace.width;
  const sy = source.height / editSpace.height;
  const crop = edit.crop
    ? clampToImage(
        {
          x: edit.crop.x * sx,
          y: edit.crop.y * sy,
          width: edit.crop.width * sx,
          height: edit.crop.height * sy,
        },
        source,
      )
    : null;
  return {
    ...edit,
    crop,
    text: edit.text.map((layer) => ({
      ...layer,
      size: layer.size * sx,
      width: layer.width * sx,
    })),
  };
}

function clampToImage(crop: CropRect, image: { width: number; height: number }): CropRect {
  const width = Math.min(image.width, Math.max(1, Math.round(crop.width)));
  const height = Math.min(image.height, Math.max(1, Math.round(crop.height)));
  return {
    x: Math.min(image.width - width, Math.max(0, Math.round(crop.x))),
    y: Math.min(image.height - height, Math.max(0, Math.round(crop.y))),
    width,
    height,
  };
}

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    // Without this, the canvas below gets tainted on ANY cross-origin load regardless of the
    // server's Access-Control-Allow-Origin header — that header only takes effect if the element
    // actually requests CORS mode. A tainted canvas throws a SecurityError on the toBlob()/
    // toDataURL() read-back this export always needs.
    image.crossOrigin = "anonymous";
    image.onload = () => resolve(image);
    image.onerror = () => reject(new Error("could not load image for export"));
    image.src = src;
  });
}

function rotatedCanvasSize(
  width: number,
  height: number,
  rotation: number,
): { width: number; height: number } {
  const normalized = normalizeEditRotation(rotation);
  return normalized === 90 || normalized === 270
    ? { width: height, height: width }
    : { width, height };
}

/**
 * Render the edit recipe (crop → rotate/flip → filters → text layers → resize)
 * to a canvas and return encoded image bytes. The app decides where they go
 * (`saveImage` adapter or its own write path).
 */
export async function exportImageEdit(
  source: string,
  edit: ImageEdit,
  outputExt: ImageSaveExtension,
  fonts: MediaFontOption[] = [],
  /** Pixel size of the image the recipe was authored against (the display variant). */
  editSpace?: { width: number; height: number } | null,
  /** Default font id and fallback stack, so export matches the on-screen text. */
  fontOptions: ImageFontOptions = {},
): Promise<ImageSavePayload> {
  const image = await loadImage(source);
  edit = scaleEditToSource(
    edit,
    editSpace,
    { width: image.naturalWidth || image.width, height: image.naturalHeight || image.height },
  );
  const crop = edit.crop ?? {
    x: 0,
    y: 0,
    width: image.naturalWidth || image.width,
    height: image.naturalHeight || image.height,
  };
  const cropped = cropSourceSize(
    { width: image.naturalWidth || image.width, height: image.naturalHeight || image.height },
    crop,
  );
  const rotated = rotatedCanvasSize(cropped.width, cropped.height, edit.rotation);
  const output = exportDimensionsFor(rotated, edit.resize);
  validateExportCanvasSize(output);
  const scale = output.width / rotated.width;
  const canvas = document.createElement("canvas");
  canvas.width = output.width;
  canvas.height = output.height;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("could not create image export canvas");
  await ensureTextFontsReady(edit.text, fonts, fontOptions);

  ctx.save();
  ctx.filter = cssFilterForAdjustments(edit.adjustments);
  ctx.translate(output.width / 2, output.height / 2);
  ctx.rotate((normalizeEditRotation(edit.rotation) * Math.PI) / 180);
  ctx.scale(edit.flipX ? -scale : scale, edit.flipY ? -scale : scale);
  ctx.drawImage(
    image,
    crop.x,
    crop.y,
    crop.width,
    crop.height,
    -crop.width / 2,
    -crop.height / 2,
    crop.width,
    crop.height,
  );
  for (const layer of edit.text) {
    const text = layer.text.trim();
    if (!text) continue;
    const sourceX = layer.x * (image.naturalWidth || image.width) - crop.x - crop.width / 2;
    const sourceY = layer.y * (image.naturalHeight || image.height) - crop.y - crop.height / 2;
    ctx.save();
    ctx.filter = "none";
    const fontSize = Math.max(8, layer.size);
    const font = fontCssForImageText(fonts, layer.fontFamily, fontSize, layer.bold, fontOptions);
    const lines = wrapText(
      ctx,
      text,
      Math.max(fontSize, layer.width || fontSize * text.length),
      font,
    );
    ctx.translate(sourceX, sourceY);
    ctx.rotate(((layer.angle || 0) * Math.PI) / 180);
    ctx.fillStyle = layer.color;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.font = font;
    ctx.lineWidth = Math.max(2, fontSize * 0.08);
    ctx.strokeStyle = "rgba(0,0,0,0.38)";
    const lineHeight = fontSize * 1.16;
    const firstY = -((lines.length - 1) * lineHeight) / 2;
    lines.forEach((line, index) => {
      const y = firstY + index * lineHeight;
      ctx.strokeText(line, 0, y);
      ctx.fillText(line, 0, y);
    });
    ctx.restore();
  }
  ctx.restore();

  const mime = mimeForOutputExtension(outputExt);
  return { buffer: await canvasToArrayBuffer(canvas, mime), extension: outputExt, mime };
}

async function ensureTextFontsReady(
  text: TextLayer[],
  fonts: MediaFontOption[],
  fontOptions: ImageFontOptions,
): Promise<void> {
  const fontSet = document.fonts;
  if (!fontSet?.load) return;
  await Promise.all(
    text
      .filter((layer) => layer.text.trim())
      .map((layer) =>
        fontSet
          .load(fontCssForImageText(fonts, layer.fontFamily, layer.size, layer.bold, fontOptions))
          .catch(() => undefined),
      ),
  );
}

function wrapText(
  ctx: CanvasRenderingContext2D,
  text: string,
  maxWidth: number,
  font: string,
): string[] {
  ctx.font = font;
  const words = text.split(/\s+/);
  const lines: string[] = [];
  let line = "";
  for (const word of words) {
    const next = line ? `${line} ${word}` : word;
    if (line && ctx.measureText(next).width > maxWidth) {
      lines.push(line);
      line = word;
    } else {
      line = next;
    }
  }
  if (line) lines.push(line);
  return lines.length ? lines : [text];
}
