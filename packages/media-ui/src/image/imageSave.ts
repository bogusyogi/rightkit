export type ImageSaveExtension = "png" | "jpeg" | "webp";

export type { ImageSavePayload } from "../adapters.js";
import type { ImageSavePayload } from "../adapters.js";

export type ImageSaveFunction = (
  outputExt?: ImageSaveExtension,
) => Promise<ImageSavePayload | null>;

const BROWSER_IMAGE_EXTS = new Set([
  "png",
  "jpg",
  "jpeg",
  "webp",
  "gif",
  "bmp",
  "svg",
  "ico",
  "avif",
]);
const EDITABLE_OUTPUTS: Record<string, ImageSaveExtension> = {
  png: "png",
  jpg: "jpeg",
  jpeg: "jpeg",
  webp: "webp",
};
const IMAGE_MIMES: Record<string, string> = {
  avif: "image/avif",
  bmp: "image/bmp",
  gif: "image/gif",
  ico: "image/x-icon",
  jpeg: "image/jpeg",
  jpg: "image/jpeg",
  png: "image/png",
  svg: "image/svg+xml",
  webp: "image/webp",
};

function extOf(path: string): string {
  return path.split(".").pop()?.toLowerCase() ?? "";
}

function stemOf(path: string): string {
  const base = path.split(/[\\/]/).pop() ?? path;
  return base.replace(/\.[^.]+$/, "");
}

export function isBrowserImagePath(path: string): boolean {
  return BROWSER_IMAGE_EXTS.has(extOf(path));
}

export function editableImageOutputForPath(path: string): ImageSaveExtension | null {
  return EDITABLE_OUTPUTS[extOf(path)] ?? null;
}

export function imageMimeFromPath(path: string): string | null {
  return IMAGE_MIMES[extOf(path)] ?? null;
}

export function mimeForOutputExtension(ext: ImageSaveExtension): string {
  return ext === "jpeg" ? "image/jpeg" : `image/${ext}`;
}

export function defaultImageSaveName(path: string): string {
  const output = editableImageOutputForPath(path);
  const ext = output === "jpeg" ? extOf(path) || "jpg" : (output ?? "png");
  return `${stemOf(path)}-edited.${ext}`;
}

export function ensureImageOutputPath(path: string, outputExt: ImageSaveExtension): string {
  if (editableImageOutputForPath(path)) return path;
  const ext = outputExt === "jpeg" ? "jpg" : outputExt;
  return `${path}.${ext}`;
}

export async function canvasToArrayBuffer(
  canvas: HTMLCanvasElement,
  mime: string,
  quality = 0.92,
): Promise<ArrayBuffer> {
  const blob = await new Promise<Blob>((resolve, reject) => {
    canvas.toBlob(
      (b) => (b ? resolve(b) : reject(new Error("could not export image canvas"))),
      mime,
      quality,
    );
  });
  return blob.arrayBuffer();
}
