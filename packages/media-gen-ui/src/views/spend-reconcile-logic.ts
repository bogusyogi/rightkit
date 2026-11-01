// Pure logic for the Settings > Spend "Reconcile with an invoice" panel. No React, no runtime imports
// (type imports only), so node tests load it directly. Nothing here contacts the engine or a provider.

import type { SpendInvoiceLine, SpendInvoicePeriod, SpendPeriodAmounts, SpendReconcileDifferences, SpendReconcileInput, SpendReconcileResult } from '../lib/api.js';

// ---------------------------------------------------------------------------------------------
// Money

/** A plain dollar figure: digits with an optional decimal point. No sign, no `$`, no exponent. */
const PLAIN_USD = /^(\d+\.?\d*|\.\d+)$/;

/** Parses a typed dollar amount. `null` when it is not a plain non-negative number. */
export function parseUsd(text: string): number | null {
  const t = text.trim();
  if (!PLAIN_USD.test(t)) return null;
  const n = Number(t);
  return Number.isFinite(n) ? n : null;
}

/**
 * Dollars for display. Two decimals, more only when the figure needs them (`$0.0065`), so a summary
 * never rounds an amount into a different one.
 */
export function formatUsd(value: number): string {
  if (!Number.isFinite(value)) return 'unknown';
  const sign = value < 0 ? '-' : '';
  const abs = Math.abs(value);
  let text = abs.toFixed(10).replace(/0+$/, '');
  const dot = text.indexOf('.');
  const decimals = dot < 0 ? 0 : text.length - dot - 1;
  if (decimals < 2) text = abs.toFixed(2);
  return `${sign}$${text}`;
}

/** A signed difference: `+$1.20`, `-$0.50`, `$0.00` for none. */
export function formatSignedUsd(value: number): string {
  if (!Number.isFinite(value)) return 'unknown';
  if (Math.abs(value) < 1e-9) return '$0.00';
  return value > 0 ? `+${formatUsd(value)}` : `-${formatUsd(-value)}`;
}

// ---------------------------------------------------------------------------------------------
// Period (UTC)

/** The month the clock is in, in UTC, as `YYYY-MM`. The picker's default. */
export function utcMonthOf(now: Date = new Date()): string {
  const y = now.getUTCFullYear();
  const m = now.getUTCMonth() + 1;
  return `${y}-${String(m).padStart(2, '0')}`;
}

/** Whether `text` is a real `YYYY-MM` calendar month. */
export function isValidMonth(text: string): boolean {
  const m = /^(\d{4})-(\d{2})$/.exec(text);
  return m !== null && Number(m[2]) >= 1 && Number(m[2]) <= 12;
}

/** Whether `text` is a real `YYYY-MM-DD` calendar day (no 31 February). */
export function isValidDay(text: string): boolean {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(text);
  if (!m) return false;
  const y = Number(m[1]);
  const mo = Number(m[2]);
  const d = Number(m[3]);
  const date = new Date(Date.UTC(y, mo - 1, d));
  return date.getUTCFullYear() === y && date.getUTCMonth() === mo - 1 && date.getUTCDate() === d;
}

/** Days in a UTC calendar month given as `YYYY-MM`. Assumes a valid month. */
export function utcDaysInMonth(month: string): number {
  const y = Number(month.slice(0, 4));
  const mo = Number(month.slice(5, 7));
  // Day 0 of the 0-based index `mo` is the last day of 1-based month `mo`.
  return new Date(Date.UTC(y, mo, 0)).getUTCDate();
}

/** The inclusive UTC day bounds of a period sent to the engine. */
export function periodBounds(period: SpendInvoicePeriod): { from: string; to: string } {
  if (typeof period === 'string') {
    return { from: `${period}-01`, to: `${period}-${String(utcDaysInMonth(period)).padStart(2, '0')}` };
  }
  return { from: period.from, to: period.to };
}

/** The period the panel picks: a whole UTC month, or an inclusive range of UTC dates. */
export type PeriodChoice = { mode: 'month'; month: string } | { mode: 'dates'; from: string; to: string };

export type PeriodCheck = { ok: true; period: SpendInvoicePeriod } | { ok: false; error: string };

/** Turns the picker's choice into the engine's period. Rejects a bad month, a bad day, or an end before the start. */
export function resolvePeriod(choice: PeriodChoice): PeriodCheck {
  if (choice.mode === 'month') {
    const month = choice.month.trim();
    if (!isValidMonth(month)) return { ok: false, error: 'Pick a month (UTC).' };
    return { ok: true, period: month };
  }
  const from = choice.from.trim();
  const to = choice.to.trim();
  if (!isValidDay(from) || !isValidDay(to)) return { ok: false, error: 'Enter both dates (UTC), such as 2026-09-01.' };
  if (from > to) return { ok: false, error: 'The end date is before the start date.' };
  return { ok: true, period: { from, to } };
}

// ---------------------------------------------------------------------------------------------
// Invoice lines (CSV text)

/**
 * One pasted line: `id, amount` or `id, amount, attempt`.
 * The id is a the host app job id, unless it starts with `provider:` (a provider job id). A `job:` prefix
 * names a the host app job id explicitly. Nothing is guessed: an id that is neither is reported.
 */
export interface LineProblem {
  /** 1-based line number in the pasted text, blank lines included. */
  lineNumber: number;
  text: string;
  reason: string;
}

export interface ParsedInvoiceLines {
  lines: SpendInvoiceLine[];
  problems: LineProblem[];
}

const WHOLE_ATTEMPT = /^[1-9]\d*$/;

/** Parses one line. Returns the line, or the reason it cannot be used. */
export function parseInvoiceLine(text: string): { ok: true; line: SpendInvoiceLine } | { ok: false; reason: string } {
  const fields = text.split(',').map((f) => f.trim());
  if (fields.length < 2 || fields.length > 3) {
    return { ok: false, reason: 'expected a job ID, a comma, and an amount, such as abc123, 4.25 (optionally a third number for the attempt)' };
  }
  const id = fields[0];
  if (!id) return { ok: false, reason: 'the job ID is empty' };
  let kind: 'job' | 'provider' = 'job';
  let key = id;
  const prefixed = /^(job|provider):/i.exec(id);
  if (prefixed) {
    kind = prefixed[1].toLowerCase() === 'provider' ? 'provider' : 'job';
    key = id.slice(prefixed[0].length).trim();
    if (!key) return { ok: false, reason: `the ${kind} ID after "${prefixed[0]}" is empty` };
  }
  if (/\s/.test(key)) return { ok: false, reason: 'the ID has a space in it; check the line (a column-heading row is not a line)' };
  const usd = parseUsd(fields[1]);
  if (usd === null) return { ok: false, reason: 'the amount must be a plain dollar number such as 4.25 (no $ sign, no minus)' };
  if (fields.length === 3 && !WHOLE_ATTEMPT.test(fields[2])) return { ok: false, reason: 'the attempt must be a whole number from 1' };
  const line: SpendInvoiceLine = kind === 'provider' ? { provider_job_id: key, usd } : { job_id: key, usd };
  if (fields.length === 3) line.attempt = Number(fields[2]);
  return { ok: true, line };
}

/** Parses the pasted text. Blank lines are skipped; every other bad line is reported with its line number. */
export function parseInvoiceLines(text: string): ParsedInvoiceLines {
  const lines: SpendInvoiceLine[] = [];
  const problems: LineProblem[] = [];
  text.split(/\r?\n/).forEach((raw, index) => {
    const trimmed = raw.trim();
    if (!trimmed) return;
    const parsed = parseInvoiceLine(trimmed);
    if (parsed.ok) lines.push(parsed.line);
    else problems.push({ lineNumber: index + 1, text: trimmed, reason: parsed.reason });
  });
  return { lines, problems };
}

/** The lines' total, to the cent of float arithmetic. */
export function linesTotal(lines: SpendInvoiceLine[]): number {
  return Math.round(lines.reduce((sum, l) => sum + l.usd, 0) * 1e9) / 1e9;
}

// ---------------------------------------------------------------------------------------------
// The form: check, then the summary of exactly what will be sent

export interface ReconcileForm {
  provider: string;
  period: PeriodChoice;
  total: string;
  invoiceRef: string;
  linesText: string;
}

export type FormCheck =
  | { ok: true; input: SpendReconcileInput; summary: string[] }
  | { ok: false; errors: string[]; lineProblems: LineProblem[] };

/**
 * Validates the whole form. Returns the exact `spend.reconcile` input and its plain summary, or every
 * problem at once (form errors and bad line numbers) so the person fixes them in one pass.
 */
export function checkReconcileForm(form: ReconcileForm): FormCheck {
  const errors: string[] = [];
  const provider = form.provider.trim();
  if (!provider) errors.push('Choose a provider.');
  const period = resolvePeriod(form.period);
  if (!period.ok) errors.push(period.error);
  const total = parseUsd(form.total);
  if (total === null) errors.push('Enter the invoice total in dollars, 0 or more.');
  const parsed = parseInvoiceLines(form.linesText);
  if (parsed.problems.length > 0) {
    errors.push(`${parsed.problems.length} line${parsed.problems.length === 1 ? '' : 's'} cannot be read. Fix ${parsed.problems.length === 1 ? 'it' : 'them'} before continuing.`);
  }
  if (errors.length > 0 || !period.ok || total === null) {
    return { ok: false, errors, lineProblems: parsed.problems };
  }
  const ref = form.invoiceRef.trim();
  const input: SpendReconcileInput = { period: period.period, provider, invoice_total_usd: total };
  if (ref) input.invoice_ref = ref;
  if (parsed.lines.length > 0) input.lines = parsed.lines;
  return { ok: true, input, summary: summarizeReconcile(input) };
}

/** Plain sentences for what `spend.reconcile` will record. Shown before the confirm; never sent by itself. */
export function summarizeReconcile(input: SpendReconcileInput): string[] {
  const bounds = periodBounds(input.period);
  const periodText = typeof input.period === 'string'
    ? `${input.period}, the whole UTC month (${bounds.from} to ${bounds.to})`
    : `${bounds.from} to ${bounds.to}, inclusive UTC days`;
  const out = [
    `Provider: ${input.provider}`,
    `Period: ${periodText}`,
    `Invoice total: ${formatUsd(input.invoice_total_usd)}`,
  ];
  out.push(input.invoice_ref
    ? `Invoice reference: ${input.invoice_ref}. A second invoice needs its own reference.`
    : 'Invoice reference: none typed. One is generated from the provider and period, so a second invoice for the same provider and period needs its own reference.');
  if (input.lines && input.lines.length > 0) {
    const sum = linesTotal(input.lines);
    out.push(`Lines: ${input.lines.length} (${formatUsd(sum)} in total). Each line is matched to a spend record by its job ID; a line that matches nothing is listed, not guessed.`);
  } else {
    out.push('Lines: none. Only the invoice total is recorded, and it is not allocated to any spend record.');
  }
  out.push('Recorded: the invoice total, and the invoice amount of each matched spend record.');
  out.push('Not changed: spending limits, reserved budget, settled amounts, and what counts against the limits.');
  out.push('Nothing is sent to a provider and nothing is spent.');
  return out;
}

// ---------------------------------------------------------------------------------------------
// After submitting

/** The headline for a recorded result. `already_recorded` is the engine's idempotent no-op for the invoice record. */
export function outcomeHeadline(result: SpendReconcileResult): string {
  if (result.already_recorded) return 'This invoice was already recorded, nothing changed.';
  return `Invoice recorded for ${result.provider}, ${result.period.from} to ${result.period.to}.`;
}

/**
 * Extra sentence for an already-recorded invoice that was sent with lines. The engine applies lines
 * idempotently: a line already on the ledger is left as it was, a new one is added.
 */
export function alreadyRecordedNote(result: SpendReconcileResult): string | null {
  if (!result.already_recorded || !result.lines) return null;
  return 'Lines were applied again safely: a line already on record was left as it was, and a new line was added.';
}

/** Counts for the result: matched, lines with no row, rows with no line. */
export function outcomeCounts(result: SpendReconcileResult): { matched: number; noRow: number; otherUnmatched: number; rowsWithoutLine: number } {
  const unmatched = result.lines?.unmatched_lines ?? [];
  const noRow = unmatched.filter((u) => u.reason.startsWith('no_ledger_row')).length;
  return {
    matched: result.lines?.matched.length ?? 0,
    noRow,
    otherUnmatched: unmatched.length - noRow,
    rowsWithoutLine: result.lines?.unmatched_rows.length ?? 0,
  };
}

/** Readable text for an engine unmatched-line reason. Unknown reasons pass through verbatim. */
export function unmatchedReasonText(reason: string): string {
  if (reason.startsWith('no_ledger_row')) return 'no spend record for this provider in this period';
  if (reason.startsWith('ambiguous_attempt')) return 'the job has several records in this period; add the attempt as a third column';
  if (reason === 'duplicate_line_for_row') return 'a second line for the same spend record';
  if (reason.startsWith('row_already_reconciled')) return 'this record already has an invoice amount from another invoice or a different amount; not overwritten';
  return reason;
}

/** Readable text for `invoice_ref_conflict`. `detail` is the engine's message, shown after the explanation. */
export function conflictText(detail: string): string {
  const explanation = 'This invoice reference is already recorded with different details, so nothing was changed. Use a different reference for a different invoice.';
  const d = detail.trim().replace(/invoice_ref/g, 'invoice reference');
  return d ? `${explanation} Details: ${d}` : explanation;
}

/** The differences block as label and value rows. Null differences (nothing to compare) are left out. */
export function differenceRows(d: SpendReconcileDifferences): Array<{ label: string; value: string }> {
  const rows: Array<{ label: string; value: string | null }> = [
    { label: 'Invoice total minus settled spend, this period', value: formatSignedUsd(d.invoice_total_minus_settled_usd) },
    { label: 'Settled minus estimated spend, jobs', value: d.settled_minus_estimated_usd === null ? null : formatSignedUsd(d.settled_minus_estimated_usd) },
    { label: 'Provider-reported minus settled spend', value: d.provider_reported_minus_settled_usd === null ? null : formatSignedUsd(d.provider_reported_minus_settled_usd) },
    { label: 'Invoice lines minus settled spend, matched records', value: d.invoice_reconciled_minus_settled_usd === null ? null : formatSignedUsd(d.invoice_reconciled_minus_settled_usd) },
    { label: 'Lines total minus invoice total', value: d.lines_total_minus_invoice_total_usd === null ? null : formatSignedUsd(d.lines_total_minus_invoice_total_usd) },
  ];
  return rows.filter((r): r is { label: string; value: string } => r.value !== null);
}

// ---------------------------------------------------------------------------------------------
// The Spend section's amount rows (read-only)

/** Provider-reported figure for a period: the amount with its row count, or the engine's "not reported" text. */
export function providerReportedText(a: SpendPeriodAmounts): string {
  if (!a.provider_reported_available || a.provider_reported_usd === null) return 'not reported by the provider';
  return `${formatUsd(a.provider_reported_usd)} (${a.provider_reported_rows} ${a.provider_reported_rows === 1 ? 'row' : 'rows'})`;
}

/** Invoice-reconciled figure for a period: the amount with its row count, or "not reconciled yet". */
export function invoiceReconciledText(a: SpendPeriodAmounts): string {
  if (a.invoice_reconciled_usd === null || a.invoice_reconciled_rows === 0) return 'not reconciled yet';
  return `${formatUsd(a.invoice_reconciled_usd)} (${a.invoice_reconciled_rows} ${a.invoice_reconciled_rows === 1 ? 'row' : 'rows'})`;
}

/** Estimated figure, or "no estimate" when no row has a job. */
export function estimatedText(a: SpendPeriodAmounts): string {
  return a.estimated_usd === null ? 'no estimate' : formatUsd(a.estimated_usd);
}
