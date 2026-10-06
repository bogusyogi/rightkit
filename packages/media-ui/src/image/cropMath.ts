import type { CropRect } from "./imageEdit.js";
import type { ImageSize } from "./imageViewerState.js";

// Pure crop geometry — no react-image-crop dependency, so eager callers don't
// pull the crop bundle + CSS into the always-mounted viewer chunk.

export function clampCrop(crop: CropRect, image: ImageSize): CropRect {
  const width = Math.min(image.width, Math.max(1, Math.round(crop.width)));
  const height = Math.min(image.height, Math.max(1, Math.round(crop.height)));
  return {
    x: Math.min(image.width - width, Math.max(0, Math.round(crop.x))),
    y: Math.min(image.height - height, Math.max(0, Math.round(crop.y))),
    width,
    height,
  };
}

export function defaultCrop(size: ImageSize, aspect: number | null = null): CropRect {
  let width = size.width * 0.82;
  let height = aspect ? width / aspect : size.height * 0.82;
  if (height > size.height * 0.82) {
    height = size.height * 0.82;
    width = aspect ? height * aspect : width;
  }
  return clampCrop(
    {
      x: (size.width - width) / 2,
      y: (size.height - height) / 2,
      width,
      height,
    },
    size,
  );
}
