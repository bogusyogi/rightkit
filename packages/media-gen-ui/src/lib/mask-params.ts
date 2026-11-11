// The mask and start-image params a catalogue row declares, and the request params they produce.
// Pure (no DOM). Painting the mask is @rightkit/media-ui MaskPainter (components/MaskEditor).

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

