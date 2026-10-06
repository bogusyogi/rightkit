export type ImageSize = { width: number; height: number };
export type Point = { x: number; y: number };

export type ImageViewMode = "fit" | "actual" | "free";

export type ImageTransform = {
  zoom: number;
  offsetX: number;
  offsetY: number;
  rotation: number;
  flipX: boolean;
  flipY: boolean;
  mode: ImageViewMode;
};

export const MIN_IMAGE_ZOOM = 0.1;
export const MAX_IMAGE_ZOOM = 8;

export function initialImageTransform(): ImageTransform {
  return { zoom: 1, offsetX: 0, offsetY: 0, rotation: 0, flipX: false, flipY: false, mode: "fit" };
}

export function normalizeRotation(rotation: number): number {
  return ((rotation % 360) + 360) % 360;
}

export function displaySizeForRotation(size: ImageSize, rotation: number): ImageSize {
  const normalized = normalizeRotation(rotation);
  return normalized === 90 || normalized === 270
    ? { width: size.height, height: size.width }
    : size;
}

export function clamp(value: number, min: number, max: number): number {
  if (min === 0 && max === 0) return 0;
  const next = Math.min(max, Math.max(min, value));
  return Object.is(next, -0) ? 0 : next;
}

export function clampZoom(zoom: number): number {
  if (!Number.isFinite(zoom)) return 1;
  return clamp(zoom, MIN_IMAGE_ZOOM, MAX_IMAGE_ZOOM);
}

export function fitZoom(viewport: ImageSize, image: ImageSize, rotation: number): number {
  const display = displaySizeForRotation(image, rotation);
  if (viewport.width <= 0 || viewport.height <= 0 || display.width <= 0 || display.height <= 0) {
    return 1;
  }
  const fit = Math.min(viewport.width / display.width, viewport.height / display.height);
  return clampZoom(Math.min(1, fit));
}

export function clampOffset(
  transform: ImageTransform,
  viewport: ImageSize,
  image: ImageSize,
): ImageTransform {
  const display = displaySizeForRotation(image, transform.rotation);
  const boundX = Math.max(0, (display.width * transform.zoom - viewport.width) / 2);
  const boundY = Math.max(0, (display.height * transform.zoom - viewport.height) / 2);
  return {
    ...transform,
    offsetX: clamp(transform.offsetX, -boundX, boundX),
    offsetY: clamp(transform.offsetY, -boundY, boundY),
  };
}

export function zoomAtPoint(
  transform: ImageTransform,
  viewport: ImageSize,
  image: ImageSize,
  nextZoom: number,
  point: Point,
): ImageTransform {
  const zoom = clampZoom(nextZoom);
  const ratio = zoom / transform.zoom;
  const centerX = viewport.width / 2;
  const centerY = viewport.height / 2;
  const offsetX = point.x - centerX - (point.x - centerX - transform.offsetX) * ratio;
  const offsetY = point.y - centerY - (point.y - centerY - transform.offsetY) * ratio;
  return clampOffset({ ...transform, zoom, offsetX, offsetY, mode: "free" }, viewport, image);
}

export function fitTransform(
  transform: ImageTransform,
  viewport: ImageSize,
  image: ImageSize,
): ImageTransform {
  return {
    ...transform,
    zoom: fitZoom(viewport, image, transform.rotation),
    offsetX: 0,
    offsetY: 0,
    mode: "fit",
  };
}

export function actualSizeTransform(
  transform: ImageTransform,
  viewport: ImageSize,
  image: ImageSize,
): ImageTransform {
  return clampOffset(
    { ...transform, zoom: 1, offsetX: 0, offsetY: 0, mode: "actual" },
    viewport,
    image,
  );
}

export function rotateTransform(
  transform: ImageTransform,
  delta: number,
  viewport: ImageSize,
  image: ImageSize,
): ImageTransform {
  const next = { ...transform, rotation: normalizeRotation(transform.rotation + delta) };
  return next.mode === "fit"
    ? fitTransform(next, viewport, image)
    : clampOffset(next, viewport, image);
}

export function canPan(transform: ImageTransform, viewport: ImageSize, image: ImageSize): boolean {
  const display = displaySizeForRotation(image, transform.rotation);
  return (
    display.width * transform.zoom > viewport.width + 1 ||
    display.height * transform.zoom > viewport.height + 1
  );
}
