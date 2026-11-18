// Dictation delivery data adapted to library rows; hosts supply screens, groups & copy.

import { friendlyAppName, type LibraryQuery, type LibraryRow } from "@rightkit/library-ui";
import type { HistoryItem } from "../lib/homeMetrics.js";

/** Devices a record can come from. Records without provenance are this desktop's. */
export type HistorySource = string;

/** Host controls provenance policy & visible labels. Defaults preserve legacy delivery records. */
export interface DeliveryRowOptions {
  localDevice?: string;
  deviceLabels?: Record<string, string>;
  outcomeLabel?: (item: HistoryItem) => string;
  targetLabel?: (item: HistoryItem) => string;
  cancelledLabel?: string;
}

const DEVICE_LABELS: Record<string, string> = { iphone: "iPhone", watch: "Watch" };

/** Mac-local records predate cross-device provenance and carry no device; they count as "mac". */
export function historyDevice(item: HistoryItem, localDevice = "mac"): HistorySource {
  const device = item.source?.device?.trim().toLowerCase();
  return device || localDevice;
}

/**
 * This desktop may edit or delete only its own records. Records synced in from iPhone or Watch are
 * view-only here (the store refuses a foreign delete too).
 */
export function isLocallyDeletable(item: HistoryItem, localDevice = "mac"): boolean {
  const device = item.source?.device?.trim().toLowerCase();
  if (!device) return item.source?.kind !== "cloud_sync";
  return device === localDevice;
}

/** Label for foreign-device provenance, or undefined for local rows. */
export function deviceBadgeLabel(item: HistoryItem, options: DeliveryRowOptions = {}): string | undefined {
  const device = item.source?.device?.trim().toLowerCase();
  if (!device || device === (options.localDevice ?? "mac")) return undefined;
  return options.deviceLabels?.[device] ?? DEVICE_LABELS[device] ?? item.source?.device?.trim();
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

/**
 * The key the delivery store's app filter matches (`app_name`, written as the lower-cased raw label):
 * the file label for a file transcription, else the raw process name, window title or "Unknown app".
 * Never the friendly name the row shows, which the store cannot match.
 */
export function deliveryFilterKey(item: HistoryItem): string {
  if (item.source?.kind === "file_transcription") return targetLabel(item);
  return item.target?.process_name || item.target?.window_title || "Unknown app";
}

/**
 * A delivery record as a library row. `origin.label` is the friendly app name, `origin.filterKey` the raw
 * key the store filters on (`deliveryFilterKey`), `origin.iconKey` the exe path, else the process name.
 */
export function deliveryRow(item: HistoryItem, options: DeliveryRowOptions = {}): LibraryRow {
  const target = targetDisplay(item);
  const app = options.targetLabel?.(item) ?? target.app;
  const windowTitle = target.windowTitle;
  const kind = item.source?.kind === "file_transcription" ? "file" : item.source?.kind === "cloud_sync" ? "synced" : "app";
  return {
    id: item.delivery_id,
    text: item.transcript,
    altText: item.raw_transcript ?? null,
    timestampMs: item.delivered_at_ms,
    origin: {
      label: app,
      // The icon tile's hover title is the raw process name, as the app shipped it.
      iconTitle: item.target?.process_name?.trim() || targetLabel(item),
      processName: item.target?.process_name?.trim() ?? "",
      filterKey: deliveryFilterKey(item),
      detail: windowTitle || undefined,
      iconKey: kind === "app" ? item.target?.exe_path?.trim() || item.target?.process_name?.trim() || undefined : undefined,
      kind,
    },
    status: options.outcomeLabel?.(item) ?? historyOutcomeLabel(item),
    // The device is the row's group glyph, not a badge: only "Cancelled" is.
    badges: item.outcome.kind === "cancelled" ? [options.cancelledLabel ?? "Cancelled"] : [],
    durationMs: item.recording_ms ?? null,
    group: historyDevice(item, options.localDevice),
    groupLabel: deviceBadgeLabel(item, options),
    readOnly: !isLocallyDeletable(item, options.localDevice),
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
