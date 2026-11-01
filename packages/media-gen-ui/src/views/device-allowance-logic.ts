// Pure text and parsing for the Settings > Spend "This device" allowance block. No React and no runtime
// imports except the shared money helpers, so node tests load it directly. Nothing here contacts the engine.

import type { DeviceAllowanceAuditRow, DeviceAllowanceView } from '../lib/api.js';
import { formatUsd, parseUsd } from './spend-reconcile-logic.js';

/** The scope line the block always shows: an allowance counts this device's spend and nothing else. */
export const DEVICE_ALLOWANCE_SCOPE = 'This allowance applies to this device only.';

/** Shown when the engine reports no shared allowance (it reports `shared_allowance.available: false`). */
export function sharedAllowanceText(shared: { available: boolean; reason: string }): string {
  if (shared.available) return 'A shared allowance across computers is available.';
  return `No allowance is shared across computers yet (${shared.reason}).`;
}

/** The headline: whether an allowance is set, and what it has counted. */
export function allowanceStatusText(view: Pick<DeviceAllowanceView, 'device_allowance_usd' | 'counted_usd' | 'remaining_usd'>): string {
  if (view.device_allowance_usd === null) {
    return 'No allowance is set for this device. The daily and monthly caps still apply.';
  }
  const counted = view.counted_usd ?? 0;
  const remaining = view.remaining_usd ?? 0;
  return `${formatUsd(view.device_allowance_usd)} on this device. ${formatUsd(counted)} counted since it was set; ${formatUsd(remaining)} remaining.`;
}

/**
 * Parses the typed amount for "Set allowance". `value` is a non-negative dollar figure; `message` says why
 * the text was refused. An empty field is refused, because clearing is a separate action.
 */
export function parseAllowanceAmount(text: string): { ok: true; value: number } | { ok: false; message: string } {
  const value = parseUsd(text);
  if (value === null) return { ok: false, message: 'Enter a dollar amount of 0 or more.' };
  return { ok: true, value };
}

/** One audit row as a line of text: when, who, old to new, and the note if there is one. */
export function auditRowText(row: DeviceAllowanceAuditRow): string {
  const from = row.old_usd === null ? 'none' : formatUsd(row.old_usd);
  const to = row.new_usd === null ? 'cleared' : formatUsd(row.new_usd);
  const note = row.note ? ` (${row.note})` : '';
  return `${row.at} · ${row.who}: ${from} → ${to}${note}`;
}
