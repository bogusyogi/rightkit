/** Fabric-free mask helpers: the painted alpha threshold and the RGBA mask export used for inpainting. */

export interface MaskExportOptions {
  /**
   * `false` (default): painted pixels are opaque white and the rest opaque black.
   * `true`: painted pixels are fully transparent (the edit region) and the rest opaque, the convention
   * image-edit endpoints read from the alpha channel.
   */
  transparentEdit?: boolean;
}

/** A pixel counts as painted when its alpha reaches half coverage. */
const PAINTED_ALPHA = 128;

/** True when any pixel of an RGBA buffer is painted. */
export function maskHasPaint(rgba: Uint8ClampedArray): boolean {
  for (let i = 3; i < rgba.length; i += 4) {
    if (rgba[i]! >= PAINTED_ALPHA) return true;
  }
  return false;
}

/** Converts a painted RGBA buffer (alpha = paint coverage) to the export pixels for `options`. */
export function maskPixels(rgba: Uint8ClampedArray, options: MaskExportOptions = {}): Uint8ClampedArray {
  const out = new Uint8ClampedArray(rgba.length);
  const transparentEdit = options.transparentEdit === true;
  for (let i = 0; i < rgba.length; i += 4) {
    const painted = rgba[i + 3]! >= PAINTED_ALPHA;
    if (transparentEdit) {
      out[i] = 0;
      out[i + 1] = 0;
      out[i + 2] = 0;
      out[i + 3] = painted ? 0 : 255;
    } else {
      const value = painted ? 255 : 0;
      out[i] = value;
      out[i + 1] = value;
      out[i + 2] = value;
      out[i + 3] = 255;
    }
  }
  return out;
}

/**
 * PNG of a mask canvas at its own resolution (one pixel per image pixel). Resolves with the blob; rejects when
 * the canvas has no 2D context or the browser cannot encode it.
 */
export function exportMaskPng(source: HTMLCanvasElement, options: MaskExportOptions = {}): Promise<Blob> {
  const width = source.width;
  const height = source.height;
  const sourceCtx = source.getContext("2d");
  if (!sourceCtx || width === 0 || height === 0) return Promise.reject(new Error("mask is empty"));
  const painted = sourceCtx.getImageData(0, 0, width, height);
  const out = document.createElement("canvas");
  out.width = width;
  out.height = height;
  const outCtx = out.getContext("2d");
  if (!outCtx) return Promise.reject(new Error("mask export is unavailable"));
  const target = outCtx.createImageData(width, height);
  target.data.set(maskPixels(painted.data, options));
  outCtx.putImageData(target, 0, 0);
  return new Promise<Blob>((resolve, reject) => {
    out.toBlob((blob) => (blob ? resolve(blob) : reject(new Error("mask PNG encoding failed"))), "image/png");
  });
}
