import type { HistoryItem } from "../../lib/homeMetrics.js";
import { hostAppName } from "../../appName.js";

export const SKIP_DELETE_CONFIRM = "heardright.skipDeleteHistoryConfirm";
export const historyActionChip =
  "grid h-8 w-8 shrink-0 place-items-center rounded-[7px] border-0 bg-transparent p-0 text-[var(--muted)] transition-[background-color,color,transform,box-shadow] duration-150 ease-[var(--ease-out-strong)] hover:bg-[rgba(255,255,255,0.06)] hover:text-[var(--paper)] active:scale-[0.94] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]";
// Compact pop-up control for the History filter row and the Storage group —
// a small neutral select on the page surface (the mockup's `.pop` grammar).
export const historyFilterControl =
  "h-[30px] box-border rounded-[var(--radius-sm)] border border-[color-mix(in_srgb,var(--line),transparent_24%)] bg-[var(--surface-2)] pl-[var(--space-2)] pr-[var(--space-1)] [font:inherit] text-[length:var(--size-sm)] text-[var(--paper)] transition-[border-color,box-shadow] duration-150 ease-[var(--ease-out-strong)] focus:outline-none focus-visible:[box-shadow:var(--focus-ring)]";

export type DateFilter = "all" | "today" | "yesterday" | "week";

// HR-HISTORY-003: sole visibility control is an INCLUSIVE set of Mac/iPhone/
// Watch checkboxes (replaces the earlier exclusive `DeviceFilter` picker with
// an "all" sentinel — see docs/dispatch/evidence/history-inclusive-sources.md).
// Unchecking a source hides it; there is no separate "Hide on this device."
export type HistorySource = "mac" | "iphone" | "watch";

export type SourceSelection = Record<HistorySource, boolean>;

export const HISTORY_SOURCES: { key: HistorySource; label: string }[] = [
  { key: "mac", label: "Mac" },
  { key: "iphone", label: "iPhone" },
  { key: "watch", label: "Watch" },
];

/** HR-HISTORY-005: desktop defaults to Mac only; iPhone/Watch remain
 *  available but off by default. */
export const DEFAULT_SOURCE_SELECTION: SourceSelection = {
  mac: true,
  iphone: false,
  watch: false,
};

const HISTORY_SOURCE_SELECTION_KEY = "heardright.historySourceSelection.v1";

/** Load the user's per-device source-visibility choice, falling back to the
 *  desktop default on first run, a corrupt value, or a disabled/unavailable
 *  localStorage (private-mode, quota, etc.) — never throws. */
export function loadSourceSelection(): SourceSelection {
  try {
    const raw = localStorage.getItem(HISTORY_SOURCE_SELECTION_KEY);
    if (!raw) return { ...DEFAULT_SOURCE_SELECTION };
    const parsed = JSON.parse(raw) as Partial<SourceSelection>;
    return {
      mac: typeof parsed.mac === "boolean" ? parsed.mac : DEFAULT_SOURCE_SELECTION.mac,
      iphone: typeof parsed.iphone === "boolean" ? parsed.iphone : DEFAULT_SOURCE_SELECTION.iphone,
      watch: typeof parsed.watch === "boolean" ? parsed.watch : DEFAULT_SOURCE_SELECTION.watch,
    };
  } catch {
    return { ...DEFAULT_SOURCE_SELECTION };
  }
}

export function saveSourceSelection(selection: SourceSelection): void {
  try {
    localStorage.setItem(HISTORY_SOURCE_SELECTION_KEY, JSON.stringify(selection));
  } catch {
    // Best-effort persistence only — a write failure just means the next
    // launch falls back to the platform default, not a functional break.
  }
}

/**
 * Mac-local records predate cross-device provenance & intentionally have no
 * source.device. Treat that missing value as Mac; synced records retain their
 * canonical wire value (iphone, watch, or mac).
 */
export function historyDevice(item: HistoryItem): HistorySource {
  const device = item.source?.device?.trim().toLowerCase();
  return device === "iphone" || device === "watch" || device === "mac" ? device : "mac";
}

/**
 * HR-HISTORY-006: this desktop device may delete/tombstone ONLY Mac-origin
 * rows. iPhone- and Watch-origin rows synced in via CloudKit are strictly
 * view-only here — no swipe delete, not selectable for deletion, and
 * excluded from "Delete all". This is the single source of truth the UI
 * layer (row actions, selection checkboxes, Delete all) reads from; the
 * actual deletion call is additionally rejected store-side
 * (`heardright_core::history::delete_scoped`/`delete_matching_scoped`) so a
 * foreign-row delete request is refused in the data layer too, not only by
 * hiding the button.
 */
export function isLocallyDeletable(item: HistoryItem): boolean {
  const device = item.source?.device?.trim().toLowerCase();
  if (!device) return item.source?.kind !== "cloud_sync";
  return device === "mac";
}

// "dictations" = default (cancelled_only=false): backend excludes cancelled+draft.
// "cancelled" = cancelled_only=true: backend returns ONLY cancelled rows
// server-side (no client-side re-filtering — count matches rows exactly).

export type OutcomeFilter = "dictations" | "cancelled";

// Row-level tags are quiet neutral pills now — ember is reserved for the
// active sidebar row, switches, primary buttons, and recording.
export const cancelledBadge =
  "inline-flex shrink-0 items-center rounded-full border border-[color-mix(in_srgb,var(--line),transparent_35%)] bg-[var(--surface-3)] px-[7px] py-[1px] text-[10px] font-semibold uppercase leading-[1.4] tracking-[0.04em] text-[var(--muted)]";

// Provenance badge for a transcript synced in from another device. Same shape
// as cancelledBadge (small neutral pill) so the two read as one family of
// row-level tags. Mac is local on this surface & never gets a badge.
export const deviceBadge =
  "inline-flex shrink-0 items-center rounded-full border border-[color-mix(in_srgb,var(--line),transparent_35%)] bg-[var(--surface-3)] px-[7px] py-[1px] text-[10px] font-medium uppercase leading-[1.4] tracking-[0.04em] text-[var(--muted)]";

const DEVICE_LABELS: Record<string, string> = {
  iphone: "iPhone",
  watch: "Watch",
};

/** Label for foreign-device provenance, or undefined for local Mac rows. */
export function deviceBadgeLabel(item: HistoryItem): string | undefined {
  const device = item.source?.device?.trim().toLowerCase();
  if (!device || device === "mac") return undefined;
  return DEVICE_LABELS[device] ?? item.source?.device?.trim();
}

// HR-HISTORY-011/011A: exactly one destructive "Delete all" control (scoped
// to locally-owned rows in the current source/filter scope), plus per-row
// delete and "Delete selected" — every destructive path confirms first.
export type PendingConfirm =
  | { kind: "delete"; item: HistoryItem }
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

function outcomeLabel(kind: string, reason?: string) {
  if (kind === "pasted") return "Pasted";
  if (kind === "paste_sent_unconfirmed") return "Pasted (unverified)";
  if (kind === "copied_fallback") return reason ? "Copied to clipboard" : "Copied";
  if (kind === "copied") return "Copied";
  if (kind === "cancelled") return "Cancelled";
  return kind.replace(/_/g, " ");
}

export function historyOutcomeLabel(item: HistoryItem) {
  if (item.source?.kind === "cloud_sync") return "Synced";
  return outcomeLabel(item.outcome.kind, item.outcome.reason);
}

/**
 * Plain-text rendering for "Export selected" (HR-HISTORY-009). Desktop's
 * `export_history` Tauri command exports the ENTIRE history unconditionally
 * (see `export_history` in `src-tauri/src/dictation_commands.rs`,
 * outside this lane's exclusive-ownership scope), so it cannot express "just
 * these rows". This mirrors the shape of `history_export::format_plain_text`
 * (Rust, `src-tauri/src/history_export.rs`) closely enough to read the same
 * way, but is generated client-side from the already-loaded `HistoryItem`s
 * and written out via the existing generic `write_text_file` command so no
 * new backend command is required.
 */
export function formatSelectedPlainText(items: HistoryItem[]): string {
  const lines: string[] = [
    `${hostAppName()} History (selected)`,
    "==============================",
    `Records: ${items.length}`,
    "",
  ];
  items.forEach((item, index) => {
    const at = new Date(item.delivered_at_ms).toISOString();
    lines.push(`[${at}] ${historyOutcomeLabel(item)}`);
    lines.push(`ID: ${item.delivery_id}`);
    lines.push(`Target: ${targetLabel(item)}`);
    const clip = formatClip(item.recording_ms);
    if (clip) lines.push(`Recording time: ${clip}`);
    lines.push("Transcript:");
    lines.push(item.transcript);
    if (index + 1 < items.length) lines.push("", "--------------------", "");
  });
  return lines.join("\n");
}

export function targetLabel(item: HistoryItem) {
  if (item.source?.kind === "file_transcription") {
    return item.source.file_name ? `File · ${item.source.file_name}` : "File transcription";
  }
  if (item.source?.kind === "cloud_sync") {
    const device = item.source.device?.trim();
    if (device) return DEVICE_LABELS[device.toLowerCase()] ?? device;
    return "Synced device";
  }
  return item.target?.process_name || item.target?.window_title || "Unknown app";
}

/** "chrome.exe" -> "chrome": the process name without a trailing ".exe"
 *  (case-insensitive). Anything else is returned trimmed and untouched. */
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

export function friendlyAppName(name: string | null | undefined): string {
  const stripped = (name ?? "").trim().replace(/\.exe$/i, "").trim();
  const known = KNOWN_APP_NAMES[stripped.toLowerCase()];
  if (known) return known;
  if (stripped && stripped === stripped.toLowerCase()) {
    return stripped.charAt(0).toUpperCase() + stripped.slice(1);
  }
  return stripped;
}

/**
 * The app half of a row's meta line, shown once: a friendly app name plus the
 * window title only when it adds information (different from the app and not
 * already containing the app name, case-insensitive). File transcriptions and
 * synced rows have no target app - targetLabel already names those.
 */
export function targetDisplay(item: HistoryItem): { app: string; windowTitle: string } {
  if (item.source?.kind === "file_transcription" || item.source?.kind === "cloud_sync") {
    return { app: targetLabel(item), windowTitle: "" };
  }
  const windowTitle = item.target?.window_title?.trim() ?? "";
  const app = friendlyAppName(item.target?.process_name) || windowTitle || "Unknown app";
  const redundant =
    !windowTitle || windowTitle.toLowerCase().includes(app.toLowerCase());
  return { app, windowTitle: redundant ? "" : windowTitle };
}

export function dateFilterRange(filter: DateFilter, now = Date.now()) {
  if (filter === "all") return { from_ms: null, until_ms: null };
  const today = startOfDay(now);
  if (filter === "today") return { from_ms: today, until_ms: today + 24 * 3600_000 };
  if (filter === "yesterday") return { from_ms: today - 24 * 3600_000, until_ms: today };
  return { from_ms: now - 7 * 24 * 3600_000, until_ms: null };
}
