/**
 * Pure helpers for the rented-workers rentals panel (RentalsPanel.tsx). The shapes mirror
 * crates/genright-studio/src/model_management/rental.rs (`list`, `limits`, `settle`,
 * `enforce_rentals`) and mod.rs (`worker_action`: `teardown`, `resolve_rental`). Nothing here
 * contacts the engine and nothing here rents anything.
 */

import { EngineError, toError } from '../lib/engine-error.js';
import { hostAppName } from "../appName.js";

/** `enforcement` from `studio.models.rentals`, verbatim from rental.rs (HONEST_LIMITS). Shown when no listing has loaded yet. */
export const RENTAL_ENFORCEMENT_TEXT =
  `enforcement runs only while ${hostAppName()} runs: at engine sidecar start, every 60 s while the app is open, at the start of each CLI command that touches workers or generation, and on studio.models.rentals_enforce. When neither the app nor a CLI command is running nothing enforces any limit, and the instance keeps billing until the next engine start or until you destroy it at the provider. Destroying needs the Vast API key saved in ${hostAppName()} (a rental is only created while one is saved) or VAST_API_KEY in the enforcing process. The real fix is an instance-side maximum lifetime or auto-stop in the create request; it needs the provider's documented field name from a fetched source, which has not been verified, so none is sent`;

/** The first sentence of the panel's notice: what it means for the money. */
export const RENTAL_LEAD = 'A rented worker keeps charging until it is destroyed.';

/** The second sentence: when limits are checked, what happens otherwise, and what destroying here needs. Consistent with the engine statement. */
export const RENTAL_SUMMARY =
  `${hostAppName()} checks limits only while the app is open and when a command-line command that uses rented workers starts. If neither happens, a rented worker can run past its limits and keep charging at Vast.ai until it is destroyed there. Destroying from this screen needs your Vast.ai API key.`;

/** One row of `studio.models.rentals` `rentals[]`. */
export interface RentalListEntry {
  worker_id: string;
  label?: string | null;
  worker_state?: string | null;
  rental_status: string;
  instance_id?: number | string | null;
  elapsed_sec: number | null;
  rate_usd_hr: number | null;
  accrued_usd: number | null;
  budget_usd: number | null;
  max_duration_min: number | null;
  idle_limit_min: number | null;
  idle_for_sec: number | null;
  due_in_sec: { duration: number | null; budget: number | null; idle: number | null };
  due: 'duration' | 'budget' | 'idle' | null;
  auto_destroyed?: { reason?: string; detail?: string; at?: string } | null;
  warnings: string[];
}

/** `studio.models.rentals` output. `rentals` lists every Vast rental; `enforcement` is the engine's statement. */
export interface RentalsListing {
  now_epoch: number;
  clock_overridden: boolean;
  rentals: RentalListEntry[];
  enforcement: string;
}

/** `worker["rental"]` as persisted. Settlement fields exist only once settled. */
export interface RentalRecord {
  status?: string;
  ledger_id?: string;
  attempt?: number;
  reserved_usd?: number | null;
  max_duration_min?: number | null;
  offer_id?: number | null;
  dph_total?: number | null;
  instance_id?: number | string | null;
  billing_warning?: string | null;
  elapsed_sec?: number | null;
  rate_usd_hr?: number | null;
  actual_usd?: number | null;
  overrun_usd?: number | null;
  settled_on_declared_budget?: boolean;
  auto_destroyed?: { reason?: string; detail?: string; at?: string } | null;
  forced_teardown?: { note?: string } | null;
  [key: string]: unknown;
}

/** A worker record as `studio.models.workers` and `studio.models.worker.action` return it. */
export interface RentalWorkerRecord {
  worker_id: string;
  label?: string;
  state?: string;
  rental?: RentalRecord | null;
  [key: string]: unknown;
}

/** `studio.models.rentals_enforce` output: what one check did. `busy` means another process held the lock. */
export interface RentalsEnforceReport {
  at_epoch: number;
  destroyed: unknown[];
  failed: unknown[];
  blocked: unknown[];
  unresolved: unknown[];
  in_progress: unknown[];
  settled_only: unknown[];
  /** Held creates the check settled by label: `auto_resolved` is `adopted` or `none_found`. */
  auto_resolved?: unknown[];
  persist_errors: unknown[];
  busy: boolean;
  limits?: string;
}

export type RentalChipTone = 'ok' | 'warn' | 'bad' | 'run';

const STATUS_LABEL: Record<string, string> = {
  renting: 'Being created: result not recorded',
  outcome_unknown: 'Outcome unknown',
  active: 'Active, charging',
  destroying: 'Destroying',
  destroy_unconfirmed: 'Destroy not confirmed by Vast.ai',
  destroyed_unsettled: 'Destroyed, charge not yet settled',
  settled: 'Settled',
  released: 'Released, reserved budget returned',
};

/** The engine's worker states in plain words. An unknown state is shown as the engine sent it. */
const WORKER_STATE_LABEL: Record<string, string> = {
  configured: 'not started',
  staging: 'setting up',
  staged: 'set up, not running',
  running: 'running a job',
  idle: 'idle',
  retrieving: 'copying results back',
  terminated: 'stopped',
  error: 'error',
};

export function workerStateText(state: string | null | undefined): string {
  if (!state) return 'state unknown';
  return WORKER_STATE_LABEL[state] ?? state;
}

const REASON_TEXT: Record<string, string> = {
  duration: 'past its duration limit',
  budget: 'past its budget limit',
  idle: 'idle past its limit',
  retry: 'retrying a destroy the provider did not confirm',
};

/** A limit check's reason in words. An unknown reason is shown as the engine sent it. */
export function reasonText(reason: string | null | undefined): string {
  if (!reason) return 'past a limit';
  return REASON_TEXT[reason] ?? reason;
}

/**
 * The engine's own sentences, in plain words. Each rule names the engine text it recognises and
 * the sentence to show instead. Text no rule recognises is shown as the engine wrote it. The engine
 * text itself stays in the panel's Details disclosure.
 */
const ENGINE_TEXT_RULES: Array<[RegExp, string | ((m: RegExpExecArray) => string)]> = [
  [/VAST_API_KEY/, `${hostAppName()} does not have your Vast.ai API key, so this rented worker cannot be destroyed from here. Save the key in Settings.`],
  [/instance_not_owned|is not proven to carry this worker's label/, 'The Vast.ai computer on record does not carry this worker\'s label, so nothing was destroyed. Check it at Vast.ai.'],
  [/force:true destroyed a rental/, 'The destroy was forced because the worker was running, so any work in flight on it was interrupted.'],
  [/destroy was not confirmed by the provider/, 'Vast.ai did not confirm the destroy, so this rented worker may still be charging. The reserved budget stays open.'],
  [/past a limit and may still be billing/, 'This rented worker is past a limit and may still be charging.'],
  [/may have been delivered/, 'Vast.ai may have started a rented worker that is charging. Nothing is retried. The reserved budget stays open until it is found or confirmed absent.'],
  [/no start time is recorded/, 'No start time is recorded, so the duration and budget limits cannot be enforced.'],
  [/no max_duration_min is recorded/, 'No maximum duration is recorded, so the duration limit cannot be enforced.'],
  [/no hourly rate is known/, 'No hourly price is known, so the budget limit cannot be enforced.'],
  [/no instance carries this worker's label/, "No Vast.ai computer carries this worker's label. Confirm that none exists only if you are sure the create never reached Vast.ai. The reserved budget stays open until then."],
  [
    /(\d+) instances carry this worker's label \((.*)\); destroy the extras on Vast, then resolve again/,
    (m) => `${m[1]} Vast.ai computers carry this worker's label (${m[2]}). Destroy the extra computers on Vast.ai, then look again.`,
  ],
];

/** One engine sentence in plain words, or the engine's own text when no rule recognises it. */
export function plainEngineText(text: string): string {
  for (const [re, replacement] of ENGINE_TEXT_RULES) {
    const m = re.exec(text);
    if (m) return typeof replacement === 'string' ? replacement : replacement(m);
  }
  return text;
}

const STATUS_TONE: Record<string, RentalChipTone | undefined> = {
  renting: 'warn',
  outcome_unknown: 'bad',
  active: 'run',
  destroying: 'warn',
  destroy_unconfirmed: 'bad',
  destroyed_unsettled: 'warn',
  settled: 'ok',
  released: 'ok',
};

/** The rental status in words. An unknown status is shown as the engine sent it. */
export function rentalStatusLabel(status: string): string {
  return STATUS_LABEL[status] ?? status;
}

export function rentalStatusTone(status: string): RentalChipTone | undefined {
  return STATUS_TONE[status];
}

/** Statuses whose elapsed time and accrued cost still run on the clock (the listing's figures are live). */
export function usesLiveClock(status: string): boolean {
  return ['active', 'destroying', 'destroy_unconfirmed', 'renting', 'outcome_unknown'].includes(status);
}

/** USD to two places, or four below one cent. Never prints "NaN". */
export function usd(value: number | null | undefined): string {
  if (typeof value !== 'number' || !Number.isFinite(value)) return 'unknown';
  const digits = value !== 0 && Math.abs(value) < 0.01 ? 4 : 2;
  return `$${value.toFixed(digits)}`;
}

/** Seconds as "45 s", "10 min" or "1 h 5 min". Negative input reads as zero. */
export function formatDuration(sec: number | null | undefined): string {
  if (typeof sec !== 'number' || !Number.isFinite(sec)) return 'unknown';
  const s = Math.max(0, Math.round(sec));
  if (s < 60) return `${s} s`;
  const minutes = Math.floor(s / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  const rem = minutes % 60;
  return rem ? `${hours} h ${rem} min` : `${hours} h`;
}

/** Time left on a limit: "due in 20 min", or "past due by 2 min" once it is due. `unknown` says why there is none. */
export function dueText(dueSec: number | null | undefined, unknown: string): string {
  if (typeof dueSec !== 'number' || !Number.isFinite(dueSec)) return unknown;
  if (dueSec <= 0) return `past due by ${formatDuration(-dueSec)}`;
  return `due in ${formatDuration(dueSec)}`;
}

/** The figures a row shows: live from the listing while the clock runs, else the settled record. */
export interface RentalFigures {
  live: boolean;
  elapsedSec: number | null;
  accruedUsd: number | null;
  rateUsdHr: number | null;
}

export function rentalFigures(entry: RentalListEntry, record?: RentalRecord | null): RentalFigures {
  const live = usesLiveClock(entry.rental_status);
  if (live) {
    return { live, elapsedSec: entry.elapsed_sec, accruedUsd: entry.accrued_usd, rateUsdHr: entry.rate_usd_hr };
  }
  return {
    live,
    elapsedSec: typeof record?.elapsed_sec === 'number' ? record.elapsed_sec : null,
    accruedUsd: typeof record?.actual_usd === 'number' ? record.actual_usd : null,
    rateUsdHr: typeof record?.rate_usd_hr === 'number' ? record.rate_usd_hr : null,
  };
}

export interface RentalLimitRow {
  name: 'Duration' | 'Budget' | 'Idle';
  limit: string;
  now: string;
  /** Null for a rental that is not active: a limit is only enforced on an active rental. */
  due: string | null;
}

/** The three limits with their current figure and time left. Only an active rental has a "due in". */
export function limitRows(entry: RentalListEntry, figures: RentalFigures): RentalLimitRow[] {
  const active = entry.rental_status === 'active';
  const rows: RentalLimitRow[] = [];
  rows.push({
    name: 'Duration',
    limit: entry.max_duration_min != null ? `${entry.max_duration_min} min` : 'none recorded',
    now: `elapsed ${formatDuration(figures.elapsedSec)}`,
    due: entry.max_duration_min != null && active ? dueText(entry.due_in_sec.duration, 'time left unknown: no start time on record') : null,
  });
  rows.push({
    name: 'Budget',
    limit: entry.budget_usd != null ? usd(entry.budget_usd) : 'none recorded',
    now: `accrued ${usd(figures.accruedUsd)}`,
    due: entry.budget_usd != null && active ? dueText(entry.due_in_sec.budget, 'cannot be enforced: no hourly price known') : null,
  });
  rows.push({
    name: 'Idle',
    limit: entry.idle_limit_min != null ? `${entry.idle_limit_min} min idle` : 'none: the worker is not idle',
    now: entry.idle_for_sec != null ? `idle ${formatDuration(entry.idle_for_sec)}` : 'not idle',
    due: entry.idle_limit_min != null && active ? dueText(entry.due_in_sec.idle, 'time left unknown') : null,
  });
  return rows;
}

// The match words below are shared by the derived warnings and the engine's own sentences, so a
// warning the engine repeats is shown once. Keep each derived sentence containing its match word.
const BILLING_RE = /may still be (billing|charging)/i;
const DELIVERED_RE = /may have been delivered|may have started|may exist/i;

/**
 * Warnings in display order. The derived ones state the consequence for each status; the engine's
 * own warnings follow in plain words, minus any that repeat a derived one.
 */
export function rentalWarnings(entry: RentalListEntry): string[] {
  const out: string[] = [];
  const status = entry.rental_status;
  if (status === 'renting') {
    out.push('Creation outcome not recorded: the provider may have started a rented worker that is charging. Nothing is retried. Use Find at provider first.');
  } else if (status === 'outcome_unknown') {
    out.push('Outcome unknown: the provider may have started a rented worker that is charging. Nothing is retried. Use Find at provider first.');
  }
  if (status === 'destroy_unconfirmed') {
    out.push('May still be charging: Vast.ai did not confirm the destroy. The reserved budget stays open.');
  }
  if (status === 'destroyed_unsettled') {
    out.push('Destroyed. Its charge is not settled yet; the next limit check settles it.');
  }
  for (const w of entry.warnings ?? []) {
    if (/^rental status is /.test(w)) continue;
    if (BILLING_RE.test(w) && out.some((d) => BILLING_RE.test(d))) continue;
    if (DELIVERED_RE.test(w) && out.some((d) => DELIVERED_RE.test(d) || /Outcome unknown|outcome not recorded/.test(d))) continue;
    const plain = plainEngineText(w);
    if (!out.includes(plain)) out.push(plain);
  }
  return out;
}

/** Why Destroy is refused for this worker state, or null. The engine refuses teardown while staging or retrieving. */
export function destroyBlocker(workerState: string | null | undefined): string | null {
  if (workerState === 'staging' || workerState === 'retrieving') {
    return `The worker is ${workerStateText(workerState)}. Destroy is available once that finishes.`;
  }
  return null;
}

/** A running worker needs `force: true`: the destroy interrupts its work. */
export function destroyNeedsForce(workerState: string | null | undefined): boolean {
  return workerState === 'running';
}

export type RentalActionId = 'destroy' | 'resolve';

export interface RentalAction {
  id: RentalActionId;
  /** Non-null when the action is offered but cannot run yet; the text says why. */
  blocker: string | null;
}

/**
 * The per-rental actions offered for a status. Destroy only for an active or unconfirmed rental (the
 * engine refuses teardown while a create is unresolved). Resolve only for a create whose outcome is
 * not recorded. Nothing is offered while a destroy is in flight, once it is destroyed (settlement is
 * the limit check's job), or once settled or released.
 */
export function rentalActions(status: string, workerState?: string | null): RentalAction[] {
  if (status === 'active' || status === 'destroy_unconfirmed') {
    return [{ id: 'destroy', blocker: destroyBlocker(workerState) }];
  }
  if (status === 'renting' || status === 'outcome_unknown') {
    return [{ id: 'resolve', blocker: null }];
  }
  return [];
}

/** Settlement in words from a settled record, or null while nothing is settled. */
export function settlementText(record: RentalRecord | null | undefined): string | null {
  if (!record || typeof record.actual_usd !== 'number') return null;
  const parts = [`charge settled at ${usd(record.actual_usd)}`];
  const over = typeof record.overrun_usd === 'number' ? record.overrun_usd : 0;
  if (over > 0) parts.push(`${usd(over)} over the ${usd(record.reserved_usd ?? 0)} reserved budget`);
  if (record.settled_on_declared_budget) parts.push('settled at the declared budget, because no start time or hourly price was known');
  return parts.join('; ');
}

function obj(x: unknown): Record<string, unknown> {
  return x && typeof x === 'object' ? (x as Record<string, unknown>) : {};
}

function text(x: unknown): string {
  if (typeof x === 'string') return x;
  if (x && typeof x === 'object' && typeof (x as { message?: unknown }).message === 'string') return (x as { message: string }).message;
  return x == null ? '' : JSON.stringify(x);
}

/** A sentence ends with a full stop unless its text already ends with punctuation. */
function sentence(s: string): string {
  return /[.!?]$/.test(s) ? s : `${s}.`;
}

/** Lines that say what one limit check did, most consequential first. Engine text is put in plain words. */
export function enforceLines(report: RentalsEnforceReport): string[] {
  if (report.busy) {
    return [`Another ${hostAppName()} process is changing the rented worker records. Nothing was checked: try again.`];
  }
  const lines: string[] = [];
  for (const raw of report.destroyed ?? []) {
    const d = obj(raw);
    const settled = settlementText(obj(d.settlement) as RentalRecord);
    lines.push(`Destroyed ${text(d.worker_id)}: ${reasonText(text(d.reason))}.${settled ? ` ${settled[0].toUpperCase()}${settled.slice(1)}.` : ''}`);
  }
  for (const raw of report.failed ?? []) {
    const f = obj(raw);
    const warning = f.billing_warning ? ` ${sentence(plainEngineText(text(f.billing_warning)))}` : '';
    lines.push(`Destroy failed for ${text(f.worker_id)}: ${sentence(plainEngineText(text(f.error)))}${warning}`);
  }
  for (const raw of report.blocked ?? []) {
    const b = obj(raw);
    const warning = b.billing_warning ? ` ${sentence(plainEngineText(text(b.billing_warning)))}` : '';
    lines.push(`Not destroyed ${text(b.worker_id)} (${reasonText(text(b.reason))}): ${sentence(plainEngineText(text(b.error)))}${warning}`);
  }
  for (const raw of report.unresolved ?? []) {
    const u = obj(raw);
    const status = text(u.status);
    lines.push(`${text(u.worker_id)}: ${rentalStatusLabel(status).toLowerCase()}. Use Find at provider first. The check never destroys it.`);
  }
  for (const raw of report.in_progress ?? []) {
    lines.push(`${text(obj(raw).worker_id)}: a destroy is already in progress.`);
  }
  for (const raw of report.settled_only ?? []) {
    const s = obj(raw);
    const settled = settlementText(obj(s.settlement) as RentalRecord);
    lines.push(`Settled ${text(s.worker_id)}${settled ? `: ${settled}` : ''}.`);
  }
  for (const raw of report.auto_resolved ?? []) {
    const a = obj(raw);
    lines.push(
      text(a.auto_resolved) === 'adopted'
        ? `${text(a.worker_id)}: found the rented computer the unrecorded create started and took it over; its limits now apply.`
        : `${text(a.worker_id)}: no rented computer was found on two checks a minute apart, so the reserved budget was released.`,
    );
  }
  for (const raw of report.persist_errors ?? []) {
    const p = obj(raw);
    lines.push(`Could not save ${text(p.worker_id)}: ${sentence(plainEngineText(text(p.error)))}`);
  }
  if (lines.length === 0) lines.push('Nothing was due: no rented worker is past a limit.');
  return lines;
}

/** The engine's error for a failed lookup or destroy, in plain words. Its code is never shown here. */
export function plainErrorText(error: unknown): string {
  const e = toError(error);
  return plainEngineText(e instanceof EngineError ? e.detail : e.message);
}
