// Pure helpers and shared classes for the library list (moved from @rightkit/dictation-ui 0.1.0
// `pages/history/historyUtils`, the half that is not about dictation records).

import type { LibraryOrigin, LibraryRow } from "../contract.js";

export const rowActionChip =
  "grid h-8 w-8 shrink-0 place-items-center rounded-[7px] border-0 bg-transparent p-0 text-[var(--muted)] transition-[background-color,color,transform,box-shadow] duration-150 ease-[var(--ease-out-strong)] hover:bg-[rgba(255,255,255,0.06)] hover:text-[var(--paper)] active:scale-[0.94] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]";
// Compact pop-up control for the filter row: a small neutral select on the page surface.
export const filterControl =
  "h-[30px] box-border rounded-[var(--radius-sm)] border border-[color-mix(in_srgb,var(--line),transparent_24%)] bg-[var(--surface-2)] pl-[var(--space-2)] pr-[var(--space-1)] [font:inherit] text-[length:var(--size-sm)] text-[var(--paper)] transition-[border-color,box-shadow] duration-150 ease-[var(--ease-out-strong)] focus:outline-none focus-visible:[box-shadow:var(--focus-ring)]";
// Row-level tags (badges, foreign-group provenance) are quiet neutral pills.
export const badgeClass =
  "inline-flex shrink-0 items-center rounded-full border border-[color-mix(in_srgb,var(--line),transparent_35%)] bg-[var(--surface-3)] px-[7px] py-[1px] text-[10px] font-semibold uppercase leading-[1.4] tracking-[0.04em] text-[var(--muted)]";

export type DateFilter = "all" | "today" | "yesterday" | "week";

/** Every destructive path confirms first: one row, everything in scope, or the selection. */
export type PendingConfirm =
  | { kind: "delete"; row: LibraryRow }
  | { kind: "clear"; count: number }
  | { kind: "selected"; ids: string[] }
  | null;

export function startOfDay(ms: number) {
  const d = new Date(ms);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

export function dayLabel(ms: number) {
  const today = startOfDay(Date.now());
  const day = startOfDay(ms);
  const diffDays = Math.round((today - day) / (24 * 3600_000));
  if (diffDays === 0) return "Today";
  if (diffDays === 1) return "Yesterday";
  const d = new Date(ms);
  return d.toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric" });
}

export function timeLabel(ms: number) {
  const d = new Date(ms);
  return d.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
}

export function formatClip(ms?: number | null): string | null {
  if (!ms || ms < 1000) return null;
  const s = Math.round(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

export function dateFilterRange(filter: DateFilter, now = Date.now()): { fromMs: number | null; untilMs: number | null } {
  if (filter === "all") return { fromMs: null, untilMs: null };
  const today = startOfDay(now);
  if (filter === "today") return { fromMs: today, untilMs: today + 24 * 3600_000 };
  if (filter === "yesterday") return { fromMs: today - 24 * 3600_000, untilMs: today };
  return { fromMs: now - 7 * 24 * 3600_000, untilMs: null };
}

const KNOWN_APP_NAMES: Record<string, string> = {
  msedge: "Microsoft Edge",
  chrome: "Google Chrome",
  code: "Visual Studio Code",
  windowsterminal: "Terminal",
  explorer: "File Explorer",
  winword: "Word",
  excel: "Excel",
  outlook: "Outlook",
  olk: "Outlook",
  slack: "Slack",
  discord: "Discord",
  notepad: "Notepad",
};

/** "chrome.exe" -> "Google Chrome"; a lower-case name is capitalised; anything else is trimmed. */
export function friendlyAppName(name: string | null | undefined): string {
  const stripped = (name ?? "").trim().replace(/\.exe$/i, "").trim();
  const known = KNOWN_APP_NAMES[stripped.toLowerCase()];
  if (known) return known;
  if (stripped && stripped === stripped.toLowerCase()) {
    return stripped.charAt(0).toUpperCase() + stripped.slice(1);
  }
  return stripped;
}

/** Field names (and the origin's value) in the "Export selected" text. Default: `Origin: <label>` and `Text:`. */
export interface LibraryExportFields {
  /** Name of the origin line. */
  origin: string;
  /** Name of the line that introduces the row's text. */
  text: string;
  /** The origin line's value. Default: the origin's `label`. */
  originValue?: (origin: LibraryOrigin) => string;
}

/** Plain-text rendering of rows, for "Export selected". */
export function formatRowsPlainText(rows: LibraryRow[], title: string, fields?: LibraryExportFields): string {
  const heading = `${title} (selected)`;
  const lines: string[] = [heading, "=".repeat(Math.max(30, heading.length)), `Records: ${rows.length}`, ""];
  rows.forEach((row, index) => {
    const at = new Date(row.timestampMs).toISOString();
    lines.push(row.status ? `[${at}] ${row.status}` : `[${at}]`);
    lines.push(`ID: ${row.id}`);
    if (row.origin) lines.push(`${fields?.origin ?? "Origin"}: ${fields?.originValue ? fields.originValue(row.origin) : row.origin.label}`);
    const clip = formatClip(row.durationMs);
    if (clip) lines.push(`Recording time: ${clip}`);
    lines.push(`${fields?.text ?? "Text"}:`);
    lines.push(row.text);
    if (index + 1 < rows.length) lines.push("", "--------------------", "");
  });
  return lines.join("\n");
}

// Empty state and the inset card one day's rows sit in.
export const emptyClass =
  "flex flex-col items-center gap-[var(--space-2)] rounded-[var(--radius-md)] border border-[color-mix(in_srgb,var(--line),transparent_18%)] bg-[var(--surface-2)] px-[var(--space-5)] py-[var(--space-6)] text-center text-[var(--muted)] [box-shadow:var(--surface-shadow)] [text-wrap:pretty]";
export const surfaceClass =
  "min-w-0 rounded-[var(--radius-md)] border border-[color-mix(in_srgb,var(--line),transparent_18%)] bg-[var(--surface-2)] transition-[background-color,border-color] duration-150 ease-[var(--ease-out-strong)]";
// Text field for an edited row.
export const rowInputClass =
  "min-h-[38px] rounded-[var(--radius-sm)] border border-[color-mix(in_srgb,var(--line),transparent_12%)] bg-[var(--surface-1)] px-[var(--space-3)] py-[var(--space-2)] [font:inherit] text-[length:var(--size-md)] text-[var(--paper)] transition-[border-color,box-shadow,background-color] duration-150 ease-[var(--ease-out-strong)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]";
