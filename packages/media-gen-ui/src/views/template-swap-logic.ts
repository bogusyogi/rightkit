// Pure pieces of the Template swap view (TemplateSwap.tsx). No runtime imports: node tests load this file directly.

export interface SwapSlot {
  name: string;
  label: string;
  type: 'image' | 'video';
  required: boolean;
}

export interface SwapTemplate {
  id: string;
  name: string;
  kind: 'image' | 'video';
  base: { path?: string; url?: string };
  prompt: string;
  slots: SwapSlot[];
  default_model?: { provider: string; model: string } | null;
}

export interface SwapConsidered {
  route: string;
  est_usd?: number | null;
  usable: boolean;
  reason?: string;
}

export interface SwapPreflight {
  ok: boolean;
  code?: string;
  message?: string;
  template_id?: string;
  route?: { provider: string; model: string };
  est_usd?: number;
  considered: SwapConsidered[];
  draft?: { draft_id: string; request_hash: string; est_usd?: number | null };
  upload_confirmation?: { required: boolean; paths: string[]; confirm_field: string };
}

const IMAGE_EXT = ['png', 'jpg', 'jpeg', 'webp', 'gif', 'avif', 'bmp'];
const VIDEO_EXT = ['mp4', 'mov', 'webm', 'm4v'];

export function mediaTypeOf(value: string): 'image' | 'video' | null {
  const ext = value.trim().split(/[?#]/)[0].split('.').pop()?.toLowerCase() ?? '';
  if (IMAGE_EXT.includes(ext)) return 'image';
  if (VIDEO_EXT.includes(ext)) return 'video';
  return null;
}

export const isUrl = (value: string): boolean => /^https?:\/\//i.test(value.trim());

/** Slot values that are set, keyed by slot name (the preflight `slots` object). */
export function filledSlots(slots: SwapSlot[], values: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const s of slots) {
    const v = (values[s.name] ?? '').trim();
    if (v) out[s.name] = v;
  }
  return out;
}

export interface SlotProblem {
  name: string;
  text: string;
}

/** Missing required slots and wrong-media slots, before anything is sent. */
export function slotProblems(slots: SwapSlot[], values: Record<string, string>): SlotProblem[] {
  const problems: SlotProblem[] = [];
  for (const s of slots) {
    const v = (values[s.name] ?? '').trim();
    if (!v) {
      if (s.required) problems.push({ name: s.name, text: `${s.label} is required.` });
      continue;
    }
    if (mediaTypeOf(v) !== s.type) problems.push({ name: s.name, text: `${s.label} takes ${s.type} media.` });
  }
  return problems;
}

export const canPreflight = (slots: SwapSlot[], values: Record<string, string>): boolean => slotProblems(slots, values).length === 0;

export function preflightInput(templateId: string, slots: SwapSlot[], values: Record<string, string>, route?: string): Record<string, unknown> {
  const input: Record<string, unknown> = { template_id: templateId, slots: filledSlots(slots, values) };
  if (route) {
    const i = route.indexOf('/');
    if (i > 0) {
      input.provider = route.slice(0, i);
      input.model = route.slice(i + 1);
    }
  }
  return input;
}

/** Local files the run uploads to fal, as the engine listed them. */
export function uploadPaths(p: SwapPreflight | null): string[] {
  return p?.ok && p.upload_confirmation?.required ? p.upload_confirmation.paths : [];
}

/** Approve needs a draft, and consent when files are to be uploaded. */
export function canApprove(p: SwapPreflight | null, consent: boolean): boolean {
  if (!p?.ok || !p.draft) return false;
  return uploadPaths(p).length === 0 || consent;
}

/** `confirm_fal_upload` is sent only as boolean true, only when uploads exist and the box is ticked. */
export function runRequest(generate: Record<string, unknown>, p: SwapPreflight | null, consent: boolean): Record<string, unknown> {
  return uploadPaths(p).length > 0 && consent ? { ...generate, confirm_fal_upload: true } : { ...generate };
}

export const routeLabel = (p: SwapPreflight | null): string => (p?.route ? `${p.route.provider}/${p.route.model}` : '');

/** Alternatives other than the chosen route, usable first then cheapest. */
export function alternatives(p: SwapPreflight | null): SwapConsidered[] {
  if (!p?.ok || !p.route) return [];
  const chosen = routeLabel(p);
  return p.considered
    .filter((c) => c.route !== chosen)
    .sort((a, b) => Number(b.usable) - Number(a.usable) || (a.est_usd ?? Infinity) - (b.est_usd ?? Infinity));
}

/** Usable route labels from the last preflight, for the route chooser. */
export function usableRoutes(p: SwapPreflight | null): string[] {
  return (p?.considered ?? []).filter((c) => c.usable).map((c) => c.route);
}

export const baseMedia = (t: SwapTemplate): string => t.base.path ?? t.base.url ?? '';

export function importInput(source: string, name: string, prompt: string): Record<string, unknown> {
  const s = source.trim();
  const input: Record<string, unknown> = isUrl(s) ? { url: s, prompt: prompt.trim() } : { path: s, prompt: prompt.trim() };
  if (name.trim()) input.name = name.trim();
  return input;
}

export function canImport(source: string, prompt: string): boolean {
  const s = source.trim();
  return s !== '' && prompt.trim() !== '' && mediaTypeOf(s) !== null;
}
