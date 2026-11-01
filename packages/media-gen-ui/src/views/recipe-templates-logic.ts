// Pure helpers for the recipe templates view: slot checks that mirror the engine's rules,
// readable text for engine codes, staged-step enablement, item actions and the batch review
// reducer. The engine decides every refusal; these helpers only order, label and gate what the
// view shows. No runtime imports from the app, so the node tests load this file directly.

import type {
  RecipeBatchItem,
  RecipeBatchView,
  RecipeRefusal,
  RecipeDraftStage,
  RecipeModerationGuide,
  RecipeSlotType,
  RecipeStagePlan,
  RecipeTemplate,
  RecipeTemplateInput,
  RecipeTemplateSlot,
  RecipeViolation
} from '../lib/api';
import { hostAppName } from "../appName";

const IMAGE_EXTENSIONS = new Set(['png', 'jpg', 'jpeg', 'webp', 'gif', 'avif', 'heic', 'bmp', 'tif', 'tiff']);
const VIDEO_EXTENSIONS = new Set(['mp4', 'mov', 'webm', 'm4v', 'mkv', 'avi']);

/** `image` or `video` when the path or URL ends in a known extension (the engine's rule), else null. */
export function mediaTypeOf(value: string): RecipeSlotType | null {
  const path = value.trim().split(/[?#]/)[0] ?? '';
  const dot = path.lastIndexOf('.');
  if (dot < 0) return null;
  const ext = path.slice(dot + 1).toLowerCase();
  if (IMAGE_EXTENSIONS.has(ext)) return 'image';
  if (VIDEO_EXTENSIONS.has(ext)) return 'video';
  return null;
}

export const SLOT_CODE_TEXT: Record<string, string> = {
  slot_order: 'Slots are filled in order, starting from slot 1.',
  slot_type: 'This file is the wrong type for the slot.',
  slot_missing: 'A required slot is missing.',
  slot_value: 'This is not a usable file path or web address.',
  slot_overflow: 'More files were given than the template has slots.',
  slot_unservable: 'The chosen route has no input for this slot, so the request is refused rather than sent without it.',
  param_conflict: 'A setting this slot fills was also set separately.'
};

/** "an image" or "a video": the words the slot messages use for a media type. */
function mediaPhrase(type: RecipeSlotType): string {
  return type === 'image' ? 'an image' : 'a video';
}

/** Readable text for one engine violation: the engine's own problem text, else the code's sentence. */
export function violationText(v: RecipeViolation): string {
  return v.problem || SLOT_CODE_TEXT[v.code] || v.code;
}

/** Violations grouped by slot position, plus those that match no declared slot (overflow). */
export function violationsBySlot(violations: RecipeViolation[], slots: RecipeTemplateSlot[]): { bySlot: Record<number, string[]>; general: string[] } {
  const positions = new Set(slots.map((s) => s.position));
  const bySlot: Record<number, string[]> = {};
  const general: string[] = [];
  for (const v of violations) {
    const text = violationText(v);
    if (positions.has(v.position)) {
      (bySlot[v.position] ??= []).push(text);
    } else {
      general.push(text);
    }
  }
  return { bySlot, general };
}

export interface SlotProblem {
  position: number;
  code: 'slot_missing' | 'slot_order' | 'slot_type';
  text: string;
}

export interface SlotCheck {
  /** Client-side problems, the engine's rules applied early so the view can point at the slot. */
  problems: SlotProblem[];
  /** The entries a compose sends: filled slots only, in declared order, each naming its position. */
  entries: RecipeTemplateInput['slots'];
  /** True when there are no problems and every required slot is filled. */
  ready: boolean;
}

/**
 * Check the values typed into each slot (aligned by index with `slots`, sorted by position).
 * A required slot left empty is missing. An optional slot left empty before a later filled slot
 * breaks the order. A value whose extension names the other media type is a type error.
 */
export function checkSlotFill(slots: RecipeTemplateSlot[], values: string[]): SlotCheck {
  const ordered = [...slots].sort((a, b) => a.position - b.position);
  const filled = ordered.map((_, i) => (values[i] ?? '').trim());
  let lastFilled = -1;
  filled.forEach((v, i) => {
    if (v !== '') lastFilled = i;
  });
  const problems: SlotProblem[] = [];
  const entries: RecipeTemplateInput['slots'] = [];
  ordered.forEach((slot, i) => {
    const value = filled[i];
    if (value === '') {
      if (slot.required) {
        problems.push({ position: slot.position, code: 'slot_missing', text: `${slot.name} is required: add a file path or a web (https) address.` });
      } else if (i < lastFilled) {
        problems.push({ position: slot.position, code: 'slot_order', text: `Fill ${slot.name} first: a later slot is already filled, and slots are filled in order.` });
      }
      return;
    }
    const media = mediaTypeOf(value);
    if (media !== null && media !== slot.type) {
      problems.push({ position: slot.position, code: 'slot_type', text: `This looks like ${mediaPhrase(media)} file, but slot ${slot.position} (${slot.name}) needs ${mediaPhrase(slot.type)}.` });
    }
    entries.push({ type: slot.type, value, slot: slot.position });
  });
  const requiredFilled = ordered.every((slot, i) => !slot.required || filled[i] !== '');
  return { problems, entries, ready: problems.length === 0 && requiredFilled && lastFilled >= 0 };
}

/** The compose input for a template: filled slots in order, and a route only when one was chosen. */
export function buildComposeInput(
  template: RecipeTemplate,
  values: string[],
  options: { route?: { provider: string; model: string } | null; staged?: boolean; resolution?: string } = {}
): RecipeTemplateInput {
  const check = checkSlotFill(template.slots, values);
  const input: RecipeTemplateInput = { template: template.template, slots: check.entries };
  if (options.route) {
    input.provider = options.route.provider;
    input.model = options.route.model;
  }
  // The final resolution rides in params only for staged resolution. The staged flag itself is
  // added by the preflight call (see withStaged), never by compose.
  const resolution = options.resolution?.trim() ?? '';
  if (options.staged && resolution) {
    input.params = { resolution };
  }
  return input;
}

/** The preflight input: the compose input plus `staged`, so the engine drafts the lower resolution first. */
export function withStaged(input: RecipeTemplateInput, staged: boolean): RecipeTemplateInput {
  return staged ? { ...input, staged: true } : { ...input, staged: false };
}

/** A route as `provider/model`, the form the engine and the view both name routes in. */
export function routeKey(route: { provider: string; model: string }): string {
  return `${route.provider}/${route.model}`;
}

/** Routes the catalogue does not serve, named so the unavailable card can say which ones. */
export function unservedRouteNames(template: RecipeTemplate): string[] {
  return template.routes.filter((r) => !r.served).map(routeKey);
}

/** Moderation rows for display. Project notes name their source (shown in Details); generic notes are labelled as such. */
export interface ModerationRow {
  text: string;
  generic: boolean;
  label: string;
  source: string | null;
}

export const PROJECT_NOTE_LABEL = "From the project's notes";
export const GENERIC_NOTE_LABEL = 'General caution, not a provider rule';

export function moderationRows(guide: RecipeModerationGuide): ModerationRow[] {
  const rows = [...guide.notes, ...guide.generic].map((n): ModerationRow => ({
    text: n.text,
    generic: n.generic === true,
    label: n.generic === true ? GENERIC_NOTE_LABEL : PROJECT_NOTE_LABEL,
    source: n.generic === true ? null : n.source
  }));
  return rows;
}

export const STAGE_LABEL: Record<RecipeDraftStage, string> = {
  single: 'One request',
  draft: 'Draft at the lowest resolution',
  final: 'Final at the requested resolution'
};

/** One line describing the staged plan, or why it is unavailable. */
export function stagePlanText(plan: RecipeStagePlan): string {
  if (!plan.available) return `Draft first is not available: ${plan.reason}`;
  if (plan.single_stage) return `${plan.requested} is already the lowest resolution this route offers, so it runs as one request with no draft.`;
  return `A draft at ${plan.lowest}, then the final at ${plan.requested}, with the same prompt. The final waits until the draft is approved.`;
}

export interface StepState {
  draft_id: string;
  stage: RecipeDraftStage;
  label: string;
  approved: boolean;
  canApprove: boolean;
  blockedBy: string | null;
}

/**
 * Enablement of each approve button. A draft can be approved once. A final stage waits for the
 * draft stage it names (requires_approval_of) to be approved. `approved` holds approved draft ids.
 */
export function stagedSteps(
  drafts: Array<{ draft_id: string; stage: RecipeDraftStage; requires_approval_of: string | null }>,
  approved: readonly string[]
): StepState[] {
  const done = new Set(approved);
  return drafts.map((d) => {
    const isDone = done.has(d.draft_id);
    const depDone = d.requires_approval_of === null || done.has(d.requires_approval_of);
    const blockedBy = !isDone && !depDone ? 'Approve the draft stage first; the final waits for it.' : null;
    return {
      draft_id: d.draft_id,
      stage: d.stage,
      label: STAGE_LABEL[d.stage],
      approved: isDone,
      canApprove: !isDone && depDone,
      blockedBy
    };
  });
}

export const APPROVAL_CODE_TEXT: Record<string, string> = {
  dependency_unapproved: 'Approve the draft stage first; the final waits for it.',
  approval_requires_app: `Approval has to come from the ${hostAppName()} app.`,
  approval_token_invalid: 'The approval did not verify. Approve again from the app.',
  approval_token_used: 'This approval was already used. Prepare the drafts again.',
  approval_input_changed: 'The draft changed after it was shown. Prepare the drafts again.',
  draft_changed: 'The draft is not the one that was shown. Prepare the drafts again.',
  draft_tampered: 'The draft no longer matches its hash. Prepare the drafts again.',
  draft_rejected: 'This draft was rejected. Prepare the drafts again.',
  identity_not_attached: 'No character reference is attached: files on this computer are not uploaded. Rebuild the batch with web (https) references, or upload the files.',
  decision_final: 'This item is already decided and cannot change.',
  item_refused: 'The character check refused this item, so it cannot be reviewed.'
};

/** Readable text for an approval failure: the mapped sentence for a known code, else the engine's own text. */
export function approvalFailureText(code: string | null, fallback: string): string {
  if (code && Object.hasOwn(APPROVAL_CODE_TEXT, code)) return APPROVAL_CODE_TEXT[code];
  return fallback;
}

export const REFUSAL_TEXT: Record<string, string> = {
  reference_route_unsupported: 'The route does not take character references, so the lock refused this item.'
};

/** Readable text for a per-item refusal from the character lock. */
export function batchRefusalText(refusal: { code: string; message: string }): string {
  if (Object.hasOwn(REFUSAL_TEXT, refusal.code)) return REFUSAL_TEXT[refusal.code];
  return refusal.message || refusal.code;
}

export interface RefusalView {
  headline: string;
  details: string[];
}

/** Headline and detail lines for a compose or preflight refusal. Slot violations are shown against their slot. */
export function refusalView(refusal: RecipeRefusal): RefusalView {
  if (refusal.code === 'route_not_served') {
    const missing = refusal.missing_routes ?? [];
    return {
      headline: 'Unavailable: no available route for this template.',
      details: missing.map((m) => `${m.provider}/${m.model} is not in the app's catalogue, so it is not available. Nothing was composed or guessed.`)
    };
  }
  if (refusal.code === 'staged_unavailable') {
    return { headline: 'Draft first is not available for this route.', details: [refusal.message] };
  }
  const slotViolations = refusal.violations ?? [];
  if (slotViolations.length > 0) {
    return { headline: 'The slots do not match the template. Each problem is shown against its slot.', details: [] };
  }
  return { headline: refusal.message || refusal.code, details: [] };
}

export function itemActionsFor(item: RecipeBatchItem, hashKnown: boolean): { approve: boolean; reject: boolean; note: string | null } {
  if (item.state === 'refused') return { approve: false, reject: false, note: 'Refused by the character check: there is no draft to review.' };
  if (item.state === 'approved') return { approve: false, reject: false, note: 'Approved. This decision is final.' };
  if (item.state === 'rejected') return { approve: false, reject: false, note: 'Rejected. This decision is final.' };
  const identityNote = item.identity_attached === false ? 'No character reference is attached, so approval will be refused unless the batch used web (https) references or uploads.' : null;
  return {
    approve: hashKnown,
    reject: true,
    note: hashKnown ? identityNote : 'Getting the item ready for approval.'
  };
}

export interface BatchCounts {
  items: number;
  pending: number;
  approved: number;
  rejected: number;
  refused: number;
}

export function countStates(items: RecipeBatchItem[]): BatchCounts {
  const count = (state: RecipeBatchItem['state']) => items.filter((i) => i.state === state).length;
  return { items: items.length, pending: count('pending'), approved: count('approved'), rejected: count('rejected'), refused: count('refused') };
}

/** Scenario lines from a textarea: trimmed, blanks dropped. */
export function scenarioLines(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

/** Why a batch cannot run with these lines, or null when it can. The cap comes from the template data. */
export function scenarioProblem(lines: string[], maxItems: number): string | null {
  if (lines.length === 0) return 'Add at least one scenario line, one per line.';
  if (lines.length > maxItems) {
    const extra = lines.length - maxItems;
    return `Remove ${extra} line${extra === 1 ? '' : 's'}: a batch takes at most ${maxItems} scenario line${maxItems === 1 ? '' : 's'}.`;
  }
  return null;
}

export interface BatchReviewState {
  batchId: string | null;
  view: RecipeBatchView | null;
  /** The request hash shown for each draft, from the creation response or a fresh challenge. */
  hashes: Record<string, string>;
  busyIndex: number | null;
  itemErrors: Record<number, string>;
  loadError: string | null;
}

export const INITIAL_REVIEW_STATE: BatchReviewState = {
  batchId: null,
  view: null,
  hashes: {},
  busyIndex: null,
  itemErrors: {},
  loadError: null
};

export type BatchReviewAction =
  | { type: 'view'; view: RecipeBatchView }
  | { type: 'hash'; draftId: string; hash: string }
  | { type: 'busy'; index: number | null }
  | { type: 'itemError'; index: number; message: string }
  | { type: 'loadError'; message: string | null }
  | { type: 'reset' };

/** Reduce the batch review. A new engine view replaces the items, clears busy and item errors; hashes accumulate. */
export function reviewReducer(state: BatchReviewState, action: BatchReviewAction): BatchReviewState {
  switch (action.type) {
    case 'view':
      return { ...state, batchId: action.view.batch_id, view: action.view, busyIndex: null, itemErrors: {}, loadError: null };
    case 'hash':
      return { ...state, hashes: { ...state.hashes, [action.draftId]: action.hash } };
    case 'busy':
      return { ...state, busyIndex: action.index };
    case 'itemError':
      return { ...state, busyIndex: null, itemErrors: { ...state.itemErrors, [action.index]: action.message } };
    case 'loadError':
      return { ...state, loadError: action.message };
    case 'reset':
      return INITIAL_REVIEW_STATE;
  }
}

/** Draft ids of pending items whose request hash is not known yet, so the view can fetch it before approval. */
export function draftIdsNeedingHash(view: RecipeBatchView | null, hashes: Record<string, string>): string[] {
  if (!view) return [];
  return view.items
    .filter((i) => i.state === 'pending' && i.draft_id !== null && !hashes[i.draft_id])
    .map((i) => i.draft_id as string);
}

/** Whether an item's request hash is known, so its approve button can be enabled. */
export function hashKnownFor(item: RecipeBatchItem, hashes: Record<string, string>): boolean {
  return item.draft_id !== null && Boolean(hashes[item.draft_id]);
}
