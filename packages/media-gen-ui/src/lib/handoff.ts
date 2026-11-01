/**
 * Hand-off between views: a view (Production today) sets a prefilled request
 * and navigates; the target view takes it when it mounts.
 *
 *   setHandoff({ view: 'video', kind: 'video', prompt, params })
 *   const h = takeHandoff('video'); // returns it once, then clears it
 *
 * No imports on purpose: node --test loads this file directly.
 */

export type HandoffView = 'image' | 'video' | 'voice';
export type HandoffKind = 'image' | 'video' | 'voice' | 'avatar';

export interface Handoff {
  view: HandoffView;
  kind: HandoffKind;
  prompt: string;
  /** Engine `generate` params to prefill (aspect, resolution, duration, image_url, ...). */
  params?: Record<string, unknown>;
  /** Suggested engine model; the target view may ignore it when unavailable. */
  provider?: string;
  model?: string;
  /** Where it came from, for a "Prefilled from ..." note. */
  source?: string;
  label?: string;
  /** Anything else worth keeping (shot contract, workflow notes). */
  meta?: Record<string, unknown>;
}

type Listener = (handoff: Handoff) => void;

let pending: Handoff | null = null;
const listeners = new Set<Listener>();

export function setHandoff(handoff: Handoff): void {
  pending = { ...handoff, params: handoff.params ? { ...handoff.params } : undefined };
  for (const listener of [...listeners]) listener(pending);
}

/** Peek without consuming. */
export function peekHandoff(view: HandoffView): Handoff | null {
  return pending && pending.view === view ? pending : null;
}

/** Return the pending hand-off for `view` and clear it; null when there is none for that view. */
export function takeHandoff(view: HandoffView): Handoff | null {
  if (!pending || pending.view !== view) return null;
  const taken = pending;
  pending = null;
  return taken;
}

export function clearHandoff(): void {
  pending = null;
}

/** Be told when a hand-off arrives while the target view is already mounted. */
export function onHandoff(listener: Listener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Composer fields a hand-off can prefill. Values are the composer's own types; empty string or null means "leave the default". */
export interface HandoffFormValues {
  prompt: string;
  /** Suggested model to select, when the hand-off names one. */
  model: { provider: string; model: string } | null;
  aspect: string;
  resolution: string;
  duration: number | null;
  seed: string;
  imageUrl: string;
  audioUrl: string;
  videoUrl: string;
  voiceId: string;
  /** Reference images, from `params.image_urls`. */
  references: string[];
  /** Params the composer has no field for. Reported to the person, never sent. */
  unapplied: string[];
}

const APPLIED_PARAMS = new Set(['aspect', 'resolution', 'duration', 'seed', 'image_url', 'image_urls', 'audio_url', 'video_url', 'voice_id']);

/** Map a hand-off's engine params onto composer fields. Pure: no state, no engine call. */
export function handoffFormValues(h: Handoff): HandoffFormValues {
  const p = h.params ?? {};
  const text = (v: unknown) => (typeof v === 'string' ? v.trim() : '');
  const references = Array.isArray(p.image_urls)
    ? p.image_urls.map((r) => (typeof r === 'string' ? r.trim() : '')).filter(Boolean)
    : [];
  const seed = typeof p.seed === 'number' && Number.isInteger(p.seed) ? String(p.seed) : text(p.seed);
  return {
    prompt: h.prompt ?? '',
    model: h.provider && h.model ? { provider: h.provider, model: h.model } : null,
    aspect: text(p.aspect),
    resolution: text(p.resolution),
    duration: typeof p.duration === 'number' && Number.isFinite(p.duration) ? p.duration : null,
    seed,
    imageUrl: text(p.image_url),
    audioUrl: text(p.audio_url),
    videoUrl: text(p.video_url),
    voiceId: text(p.voice_id),
    references,
    unapplied: Object.keys(p).filter((k) => !APPLIED_PARAMS.has(k))
  };
}
