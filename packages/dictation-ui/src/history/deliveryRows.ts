// Dictation delivery records in the library's shape. The list itself (filters, selection, export,
// retention, edit and delete) is `@rightkit/library-ui`; this module holds only what is particular
// to delivery records: how a record maps to a `LibraryRow`, the Dictations/Cancelled variants, the
// device groups and the words the list uses.

import { createElement } from "react";
import { Desktop, DeviceMobile, Laptop, Watch } from "@phosphor-icons/react";
import { getPlatform } from "@rightkit/platform-ui";
import { friendlyAppName, type LibraryGroup, type LibraryLabels, type LibraryQuery, type LibraryRow, type LibraryVariant } from "@rightkit/library-ui";
import { hostAppName } from "../host.js";
import type { HistoryItem } from "../lib/homeMetrics.js";

/** Devices a record can come from. Records without provenance are this desktop's. */
export type HistorySource = "mac" | "iphone" | "watch";

const DEVICE_LABELS: Record<string, string> = { iphone: "iPhone", watch: "Watch" };

/** Mac-local records predate cross-device provenance and carry no device; they count as "mac". */
export function historyDevice(item: HistoryItem): HistorySource {
  const device = item.source?.device?.trim().toLowerCase();
  return device === "iphone" || device === "watch" || device === "mac" ? device : "mac";
}

/**
 * This desktop may edit or delete only its own records. Records synced in from iPhone or Watch are
 * view-only here (the store refuses a foreign delete too).
 */
export function isLocallyDeletable(item: HistoryItem): boolean {
  const device = item.source?.device?.trim().toLowerCase();
  if (!device) return item.source?.kind !== "cloud_sync";
  return device === "mac";
}

/** Label for foreign-device provenance, or undefined for local rows. */
export function deviceBadgeLabel(item: HistoryItem): string | undefined {
  const device = item.source?.device?.trim().toLowerCase();
  if (!device || device === "mac") return undefined;
  return DEVICE_LABELS[device] ?? item.source?.device?.trim();
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

/**
 * The app half of a row's meta line: a friendly app name plus the window title only when it adds
 * information. File transcriptions and synced rows have no target app; targetLabel names those.
 */
export function targetDisplay(item: HistoryItem): { app: string; windowTitle: string } {
  if (item.source?.kind === "file_transcription" || item.source?.kind === "cloud_sync") {
    return { app: targetLabel(item), windowTitle: "" };
  }
  const windowTitle = item.target?.window_title?.trim() ?? "";
  const app = friendlyAppName(item.target?.process_name) || windowTitle || "Unknown app";
  const redundant = !windowTitle || windowTitle.toLowerCase().includes(app.toLowerCase());
  return { app, windowTitle: redundant ? "" : windowTitle };
}

/** A delivery record as a library row. `origin.iconKey` is the exe path, else the process name. */
export function deliveryRow(item: HistoryItem): LibraryRow {
  const { app, windowTitle } = targetDisplay(item);
  const kind = item.source?.kind === "file_transcription" ? "file" : item.source?.kind === "cloud_sync" ? "synced" : "app";
  const badge = deviceBadgeLabel(item);
  return {
    id: item.delivery_id,
    text: item.transcript,
    altText: item.raw_transcript ?? null,
    timestampMs: item.delivered_at_ms,
    origin: {
      label: app,
      detail: windowTitle || undefined,
      iconKey: kind === "app" ? item.target?.exe_path?.trim() || item.target?.process_name?.trim() || undefined : undefined,
      kind,
    },
    status: historyOutcomeLabel(item),
    badges: [...(item.outcome.kind === "cancelled" ? ["Cancelled"] : []), ...(badge ? [badge] : [])],
    durationMs: item.recording_ms ?? null,
    group: historyDevice(item),
    readOnly: !isLocallyDeletable(item),
  };
}

/** The variant keys `DELIVERY_VARIANTS` uses. */
export type DeliveryVariant = "dictations" | "cancelled";

/** "Dictations" excludes cancelled and draft records; "Cancelled" returns only cancelled ones. */
export const DELIVERY_VARIANTS: readonly LibraryVariant[] = [
  {
    key: "dictations",
    label: "Dictations",
    emptyTitle: "No text inserts yet.",
    emptyBody: "Delivered text appears here after dictation sends text into another app.",
  },
  {
    key: "cancelled",
    label: "Cancelled",
    emptyTitle: "No cancelled recordings match your filters.",
    emptyBody: "Recordings you cancel before delivery show up here.",
  },
];

/**
 * Device groups: this desktop on by default, iPhone and Watch off. Windows has no Apple sync, so
 * hosts there pass `fixed: true` to `useGroupSelection` (or no groups).
 */
export function deliveryGroups(): readonly LibraryGroup[] {
  const mac = getPlatform() === "macos";
  return [
    { key: "mac", label: mac ? "this Mac" : "this PC", icon: createElement(mac ? Laptop : Desktop, { "aria-hidden": true }), defaultOn: true, local: true },
    { key: "iphone", label: "iPhone", icon: createElement(DeviceMobile, { "aria-hidden": true }), defaultOn: false },
    { key: "watch", label: "Watch", icon: createElement(Watch, { "aria-hidden": true }), defaultOn: false },
  ];
}

export function deliveryLabels(): Partial<LibraryLabels> {
  return {
    item: "transcript",
    items: "transcripts",
    area: "History",
    alt: ["Undo AI polish", "Redo AI polish", "AI", "original"],
    editHint: `Correcting teaches ${hostAppName()}. The fixed text replaces this entry.`,
  };
}

/** The native history query a host passes to its store for a library query. */
export interface DeliveryQueryArgs {
  search: string;
  from_ms: number | null;
  until_ms: number | null;
  app: string;
  cancelled_only: boolean;
  offset: number;
  limit: number;
}

export function deliveryQueryArgs(query: LibraryQuery): DeliveryQueryArgs {
  return {
    search: query.search,
    from_ms: query.fromMs,
    until_ms: query.untilMs,
    app: query.origin,
    cancelled_only: query.variant === "cancelled",
    offset: query.offset,
    limit: query.limit,
  };
}
