// Pure pieces of the composer's "Why this route" panel (RoutePanel.tsx). No runtime imports: the
// node tests load this file directly. Types come from the engine contract in lib/api.ts.
import type { MediaKind, RouteAlternative, RouteInput, RouteOutcome, RoutePreferences, RouteResult, RoutePrivacy, RouteSpend } from '../lib/api.js';
import type { SettingsSectionId } from '../lib/nav.js';

// ---------------------------------------------------------------- the four states

export type RouteStateKey = 'configured' | 'installed' | 'qualified' | 'usable';
export type RouteStateValue = 'yes' | 'no' | 'n/a';
/** Colour follows the value, and the text always carries it too: ok for yes, bad for no, neutral for n/a. */
export type RouteStateTone = 'ok' | 'bad' | 'neutral';

export interface RouteStateLabel {
  key: RouteStateKey;
  label: string;
  value: RouteStateValue;
  tone: RouteStateTone;
}

const STATE_ORDER: readonly RouteStateKey[] = ['configured', 'installed', 'qualified', 'usable'];
const STATE_LABEL: Record<RouteStateKey, string> = {
  configured: 'Key',
  installed: 'Installed',
  qualified: 'Price fresh',
  usable: 'Usable'
};

/** The four states as separate labels. A null state (the adapter has no check) reads "n/a" and is neutral. */
export function stateLabels(states: RouteAlternative['states']): RouteStateLabel[] {
  return STATE_ORDER.map((key) => {
    const raw = states[key];
    const known = raw === true || raw === false;
    const value: RouteStateValue = known ? (raw ? 'yes' : 'no') : 'n/a';
    const tone: RouteStateTone = value === 'yes' ? 'ok' : value === 'no' ? 'bad' : 'neutral';
    return { key, label: STATE_LABEL[key], value, tone };
  });
}

// ---------------------------------------------------------------- route answers

export type RouteView =
  | { status: 'chosen'; result: RouteResult }
  | { status: 'choice'; result: RouteResult }
  | { status: 'none'; result: RouteResult }
  | { status: 'ambiguous'; result: RouteResult };

export type RouteLoad = { ok: true; view: RouteView } | { ok: false; message: string };

const VIEW_BY_OUTCOME: Record<RouteOutcome, RouteView['status']> = {
  chosen: 'chosen',
  route_choice_required: 'choice',
  no_usable_route: 'none',
  ambiguous_route: 'ambiguous'
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** A `media.route` answer (success body) into a view. Anything else is a message, never a guessed route. */
export function routeFromResponse(raw: unknown): RouteLoad {
  if (!isRecord(raw) || typeof raw.outcome !== 'string' || !Object.hasOwn(VIEW_BY_OUTCOME, raw.outcome)) {
    return { ok: false, message: 'The engine sent a route answer this panel does not recognise.' };
  }
  if (!Array.isArray(raw.alternatives)) {
    return { ok: false, message: 'The route answer has no alternatives list.' };
  }
  const result = raw as unknown as RouteResult;
  return { ok: true, view: { status: VIEW_BY_OUTCOME[raw.outcome as RouteOutcome], result } as RouteView };
}

/**
 * A rejected `media.route` call. The two refusals (`route_choice_required`, `no_usable_route`) carry the
 * full route result in `details`, so they read as a view; any other failure is its text.
 * Callers pass `errorCode`, `errorDetails` and `errorText` of the rejection.
 */
export function routeFromError(code: string | null, details: Record<string, unknown> | null, text: string): RouteLoad {
  if ((code === 'route_choice_required' || code === 'no_usable_route') && details) {
    const loaded = routeFromResponse(details);
    if (loaded.ok) return loaded;
  }
  return { ok: false, message: text.trim() || 'The route check failed.' };
}

// ---------------------------------------------------------------- choices and fixes

export const routeId = (route: { provider: string; model: string }): string => `${route.provider}/${route.model}`;

/** A route that spends money: a cloud route, or any route with a non-zero estimate. */
export function isPaidAlternative(a: RouteAlternative): boolean {
  return a.cloud || a.est_usd > 0;
}

/** Whether the user may pick this route from a `route_choice_required` answer. Picking never submits. */
export function isChoosable(view: RouteView, a: RouteAlternative): boolean {
  return view.status === 'choice' && a.eligible && a.states.usable && isPaidAlternative(a);
}

export function estimateLabel(a: { est_usd: number; est_source: string }): string {
  if (a.est_usd === 0) return 'free';
  const amount = `$${a.est_usd.toFixed(4)}`;
  return a.est_source === 'request' ? `${amount} for this request` : `${amount} at default size`;
}

export type FixSection = 'API keys' | 'Models';

export interface FixHint {
  section: FixSection;
  label: string;
}

/**
 * The Settings section that fixes a route's blockers, one per section. A daily or monthly cap is not
 * a Settings fix here, so a route blocked only by a cap gets no hint and keeps its engine reason.
 */
export function fixHints(a: RouteAlternative): FixHint[] {
  const sections: FixSection[] = [];
  if (!a.states.configured) sections.push('API keys');
  if (a.states.installed === false || a.states.qualified === false) sections.push('Models');
  return sections.map((section) => ({ section, label: `Open Settings > ${section}` }));
}

/** The Settings section a fix hint opens. The ids are the ones Settings lists (settings-ui section ids). */
export function settingsSectionFor(section: FixSection): SettingsSectionId {
  return section === 'API keys' ? 'api-keys' : 'models';
}

export interface PaidChoice {
  id: string;
  provider: string;
  model: string;
  est_usd: number;
  estimate: string;
  /** False when the picker has no option with this id, so picking it would change nothing. */
  pickable: boolean;
}

/** The usable paid alternatives of a `route_choice_required` answer, in the engine's order. */
export function paidChoices(alternatives: RouteAlternative[], optionIds: readonly string[]): PaidChoice[] {
  return alternatives
    .filter((a) => a.eligible && a.states.usable && isPaidAlternative(a))
    .map((a) => {
      const id = routeId(a);
      return { id, provider: a.provider, model: a.model, est_usd: a.est_usd, estimate: estimateLabel(a), pickable: optionIds.includes(id) };
    });
}

// ---------------------------------------------------------------- request and preferences

/** The `media.route` input: the kind, plus provider and model only when the user picked a cloud route. */
export function routeInput(kind: MediaKind, pick: { provider: string; model: string } | null): RouteInput {
  return pick ? { kind, provider: pick.provider, model: pick.model } : { kind };
}

/** Saved preferences the engine stores but does not apply, as one plain sentence. */
export function preferenceNote(result: Pick<RouteResult, 'preferences_not_applied' | 'preferences_not_applied_reason'>): string | null {
  const names = result.preferences_not_applied ?? [];
  if (names.length === 0) return null;
  return `${names.join(' and ')} saved but not applied: ${result.preferences_not_applied_reason ?? 'the engine does not use them'}.`;
}

export const PRIVACY_CHOICES: ReadonlyArray<{ value: RoutePrivacy; label: string }> = [
  { value: 'any', label: 'Any route' },
  { value: 'local_only', label: 'This computer only' }
];

export const SPEND_CHOICES: ReadonlyArray<{ value: RouteSpend; label: string }> = [
  { value: 'ask', label: 'Ask me' },
  { value: 'free_only', label: 'Free only' },
  { value: 'cheapest', label: 'Cheapest' }
];

export function preferenceLabel(prefs: Pick<RoutePreferences, 'privacy' | 'spend'>, field: 'privacy' | 'spend'): string {
  const list: ReadonlyArray<{ value: string; label: string }> = field === 'privacy' ? PRIVACY_CHOICES : SPEND_CHOICES;
  return list.find((c) => c.value === prefs[field])?.label ?? prefs[field];
}
