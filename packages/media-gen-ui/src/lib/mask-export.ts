// Pure mask logic for the Generate view's mask brush (no DOM, no canvas). White = repaint, black = keep.
// Strokes are kept in source-image pixel coordinates, so the exported mask has exactly the source's size.

export type MaskTool = 'brush' | 'eraser';

export interface MaskPoint {
  x: number;
  y: number;
}

export interface MaskStroke {
  tool: MaskTool;
  /** Brush diameter in source-image pixels. */
  size: number;
  points: MaskPoint[];
}

/** An image input the model declares with role `mask` (the param it reads, e.g. `mask_url`). */
export interface MaskInput {
  param: string;
  required?: boolean;
}

/** The mask input a catalogue row declares, or null when the model takes none. Never names a model. */
export function maskInputOf(row: { inputs?: { mask?: MaskInput | null } | null } | null | undefined): MaskInput | null {
  const m = row?.inputs?.mask;
  return m && typeof m.param === 'string' && m.param ? m : null;
}

/** Engine params with the saved mask path set under the model's declared mask param. No mask path leaves params untouched. */
export function withMaskParam(params: Record<string, unknown>, mask: MaskInput | null, path: string | null): Record<string, unknown> {
  if (!mask || !path) return params;
  return { ...params, [mask.param]: path };
}

/** An image input the model declares with role `start` (the single source image, e.g. `image_url`). */
export interface StartInput {
  param: string;
  required?: boolean;
}

/** The start input a catalogue row declares, or null. Never names a model. */
export function startInputOf(row: { inputs?: { start?: StartInput | null } | null } | null | undefined): StartInput | null {
  const m = row?.inputs?.start;
  return m && typeof m.param === 'string' && m.param ? m : null;
}

/**
 * Sends the first reference image as the model's single start param (a string, never a list). Applies only when the
 * row declares a start input and no reference list (`hasReferenceList` false); otherwise params are untouched.
 */
export function withStartParam(params: Record<string, unknown>, start: StartInput | null, refs: string[], hasReferenceList: boolean): Record<string, unknown> {
  if (!start || hasReferenceList || refs.length === 0 || !refs[0]) return params;
  return { ...params, [start.param]: refs[0] };
}

/** Convert a pointer position on a displayed canvas to source-image pixels. */
export function toSourcePoint(clientX: number, clientY: number, box: { left: number; top: number; width: number; height: number }, width: number, height: number): MaskPoint {
  const x = ((clientX - box.left) / Math.max(1, box.width)) * width;
  const y = ((clientY - box.top) / Math.max(1, box.height)) * height;
  return { x: Math.min(width - 1, Math.max(0, x)), y: Math.min(height - 1, Math.max(0, y)) };
}

function stamp(cover: Uint8Array, width: number, height: number, cx: number, cy: number, radius: number, value: number): void {
  const r = Math.max(0.5, radius);
  const x0 = Math.max(0, Math.floor(cx - r));
  const x1 = Math.min(width - 1, Math.ceil(cx + r));
  const y0 = Math.max(0, Math.floor(cy - r));
  const y1 = Math.min(height - 1, Math.ceil(cy + r));
  const r2 = r * r;
  for (let y = y0; y <= y1; y++) {
    const dy = y + 0.5 - cy;
    for (let x = x0; x <= x1; x++) {
      const dx = x + 0.5 - cx;
      if (dx * dx + dy * dy <= r2) cover[y * width + x] = value;
    }
  }
}

/** Paint coverage (1 = repaint) for the strokes, applied in order. Strokes interpolate between points. */
export function rasterizeStrokes(strokes: MaskStroke[], width: number, height: number): Uint8Array {
  const w = Math.max(1, Math.floor(width));
  const h = Math.max(1, Math.floor(height));
  const cover = new Uint8Array(w * h);
  for (const s of strokes) {
    const value = s.tool === 'eraser' ? 0 : 1;
    const radius = s.size / 2;
    const step = Math.max(1, radius / 2);
    let prev: MaskPoint | null = null;
    for (const p of s.points) {
      if (prev) {
        const dist = Math.hypot(p.x - prev.x, p.y - prev.y);
        const n = Math.max(1, Math.ceil(dist / step));
        for (let i = 1; i <= n; i++) stamp(cover, w, h, prev.x + ((p.x - prev.x) * i) / n, prev.y + ((p.y - prev.y) * i) / n, radius, value);
      } else {
        stamp(cover, w, h, p.x, p.y, radius, value);
      }
      prev = p;
    }
  }
  return cover;
}

/**
 * RGBA pixels of the exported mask: exactly width x height, pure white (255) where repaint, pure black (0)
 * elsewhere, fully opaque. `invert` swaps the two.
 */
export function maskRgba(strokes: MaskStroke[], width: number, height: number, invert = false): Uint8ClampedArray {
  const w = Math.max(1, Math.floor(width));
  const h = Math.max(1, Math.floor(height));
  const cover = rasterizeStrokes(strokes, w, h);
  const out = new Uint8ClampedArray(w * h * 4);
  for (let i = 0; i < cover.length; i++) {
    const v = (cover[i] === 1) !== invert ? 255 : 0;
    out[i * 4] = v;
    out[i * 4 + 1] = v;
    out[i * 4 + 2] = v;
    out[i * 4 + 3] = 255;
  }
  return out;
}

/** True when the mask would repaint nothing (so the caller can ask for a stroke before saving). */
export function isMaskEmpty(strokes: MaskStroke[]): boolean {
  return !strokes.some((s) => s.tool === 'brush' && s.points.length > 0);
}
