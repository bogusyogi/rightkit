// The mapping from a delivery record (a dictation, a transcribed file, a record synced from another device)
// to a `LibraryRow`: the originating device, whether this device may delete the row, the origin label and
// filter key, the status label. Every wire value (device and source-kind names) and every word comes in
// through `DeliveryRowConfig`. Pure; imports only the library contract and `friendlyAppName`.
// Moved from HeardRight `lib/historySource.ts`.
import type { LibraryOrigin, LibraryRow } from "../contract.js";
import { friendlyAppName } from "../list/format.js";

export interface DeliveryRecordLike {
  delivery_id: string;
  transcript: string;
  outcome: { kind: string; reason?: string | null };
  target?: { process_name?: string | null; window_title?: string | null; exe_path?: string | null } | null;
  delivered_at_ms: number;
  raw_transcript?: string | null;
  recording_ms?: number | null;
  source?: { kind: string; file_name?: string | null; device?: string | null } | null;
}

export interface DeliveryRowConfig {
  /** Wire values. `localDevice` is also the device of a record with no `source.device` (it predates sync). */
  localDevice: string;
  knownDevices: readonly string[];
  fileSourceKind: string;
  syncedSourceKind: string;
  /** Badge and label for a foreign device, by lower-cased wire value; an unlisted device shows its wire value. */
  deviceLabels: Readonly<Record<string, string>>;
  /** Name of the device this window runs on (the group label of local rows). */
  thisDeviceName(): string;
  /** The outcome kind of a cancelled dictation (its row carries a `cancelledBadge`). */
  cancelledKind: string;
  labels: {
    fileTranscription: string;
    /** The label of a file row: receives the file name. */
    fileWithName(name: string): string;
    syncedDevice: string;
    unknownApp: string;
    /** Status of a synced-in record. */
    synced: string;
    cancelledBadge: string;
    /** Status by outcome kind. */
    outcomes: Readonly<Record<string, string>>;
    /** Status by outcome kind when the outcome carries a reason (it replaces `outcomes[kind]`). */
    outcomesWithReason?: Readonly<Record<string, string>>;
  };
}

function wireDevice(item: DeliveryRecordLike): string | undefined {
  return item.source?.device?.trim().toLowerCase() || undefined;
}

/** The device a record came from; a record with no (or an unknown) device is the local one. */
export function recordDevice(item: DeliveryRecordLike, config: DeliveryRowConfig): string {
  const device = wireDevice(item);
  return device && config.knownDevices.includes(device) ? device : config.localDevice;
}

/**
 * This device may delete or tombstone ONLY its own rows. Rows synced in from another device are view-only:
 * no delete, no selection and no "Delete all". The store also rejects a foreign-row delete, so the data
 * layer refuses it too.
 */
export function isLocallyDeletable(item: DeliveryRecordLike, config: DeliveryRowConfig): boolean {
  const device = wireDevice(item);
  if (!device) return item.source?.kind !== config.syncedSourceKind;
  return device === config.localDevice;
}

/** Label for foreign-device provenance, or undefined for local rows. */
export function deviceBadgeLabel(item: DeliveryRecordLike, config: DeliveryRowConfig): string | undefined {
  const device = wireDevice(item);
  if (!device || device === config.localDevice) return undefined;
  return config.deviceLabels[device] ?? item.source?.device?.trim();
}

/** The raw label the source filters on: the file, the synced device, or the process or window title. */
export function targetLabel(item: DeliveryRecordLike, config: DeliveryRowConfig): string {
  if (item.source?.kind === config.fileSourceKind) {
    return item.source.file_name
      ? config.labels.fileWithName(item.source.file_name)
      : config.labels.fileTranscription;
  }
  if (item.source?.kind === config.syncedSourceKind) {
    const device = item.source.device?.trim();
    if (device) return config.deviceLabels[device.toLowerCase()] ?? device;
    return config.labels.syncedDevice;
  }
  return item.target?.process_name || item.target?.window_title || config.labels.unknownApp;
}

/**
 * The app half of a row's meta line, shown once: a friendly app name plus the window title only when it adds
 * information. Files and synced rows have no target app: `targetLabel` already names those.
 */
export function targetDisplay(
  item: DeliveryRecordLike,
  config: DeliveryRowConfig,
): { app: string; windowTitle: string } {
  if (item.source?.kind === config.fileSourceKind || item.source?.kind === config.syncedSourceKind) {
    return { app: targetLabel(item, config), windowTitle: "" };
  }
  const windowTitle = item.target?.window_title?.trim() ?? "";
  const app = friendlyAppName(item.target?.process_name) || windowTitle || config.labels.unknownApp;
  const redundant = !windowTitle || windowTitle.toLowerCase().includes(app.toLowerCase());
  return { app, windowTitle: redundant ? "" : windowTitle };
}

function outcomeLabel(kind: string, reason: string | null | undefined, config: DeliveryRowConfig): string {
  if (reason) {
    const withReason = config.labels.outcomesWithReason?.[kind];
    if (withReason !== undefined) return withReason;
  }
  return config.labels.outcomes[kind] ?? kind.replace(/_/g, " ");
}

export function deliveryStatusLabel(item: DeliveryRecordLike, config: DeliveryRowConfig): string {
  if (item.source?.kind === config.syncedSourceKind) return config.labels.synced;
  return outcomeLabel(item.outcome.kind, item.outcome.reason, config);
}

// The executable path behind each origin (the icon lookup prefers it over the process name), keyed by the
// origin object so nothing outlives its row.
const exePathOfOrigin = new WeakMap<LibraryOrigin, string | undefined>();

/** The executable path recorded for an origin built by `deliveryRow`, for `LibrarySource.originIcon`. */
export function originExePath(origin: LibraryOrigin): string | undefined {
  return exePathOfOrigin.get(origin);
}

function originOf(item: DeliveryRecordLike, config: DeliveryRowConfig): LibraryOrigin {
  const kind = item.source?.kind;
  const hasTargetApp = kind !== config.fileSourceKind && kind !== config.syncedSourceKind;
  const processName = item.target?.process_name;
  const exePath = item.target?.exe_path;
  const display = targetDisplay(item, config);
  const origin: LibraryOrigin = {
    label: display.app,
    // The backend matches the app filter on the raw label (process name or window title).
    filterKey: targetLabel(item, config),
    processName: processName ?? "",
    iconTitle: processName?.trim() || targetLabel(item, config),
    detail: display.windowTitle || undefined,
    iconKey: hasTargetApp ? (exePath?.trim() || processName?.trim() || "").toLowerCase() : undefined,
    kind: kind === config.fileSourceKind ? "file" : kind === config.syncedSourceKind ? "synced" : "app",
  };
  exePathOfOrigin.set(origin, hasTargetApp ? (exePath ?? undefined) : undefined);
  return origin;
}

/** The library row for a delivery record. */
export function deliveryRow(item: DeliveryRecordLike, config: DeliveryRowConfig): LibraryRow {
  return {
    id: item.delivery_id,
    text: item.transcript,
    altText: item.raw_transcript ?? null,
    timestampMs: item.delivered_at_ms,
    durationMs: item.recording_ms ?? null,
    origin: originOf(item, config),
    status: deliveryStatusLabel(item, config),
    badges: item.outcome.kind === config.cancelledKind ? [config.labels.cancelledBadge] : undefined,
    group: recordDevice(item, config),
    groupLabel: deviceBadgeLabel(item, config) ?? config.thisDeviceName(),
    readOnly: !isLocallyDeletable(item, config),
  };
}
