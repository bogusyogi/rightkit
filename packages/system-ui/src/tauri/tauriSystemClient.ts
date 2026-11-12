// The Tauri `SystemClient`, `AppsClient` and event subscription: exactly the commands and arguments the 0.1.x
// package called itself (Pulse's hub calls the same ones), now behind the host seam.
import { invoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import type {
  ActivityTimeline,
  AppDetail,
  AppEntry,
  AppsClient,
  CachedApps,
  CleanupApplyResult,
  CleanupFinding,
  CleanupHistory,
  CleanupReport,
  CleanupRestoreResult,
  DiskIndexStatus,
  DuplicateReport,
  DuplicatesTrashResult,
  FileIdentity,
  Folder,
  Growth,
  HealthReport,
  IndexedSize,
  Inspect,
  MediaCompressOutcome,
  MediaCompressStatus,
  MovePlan,
  MoveResult,
  Process,
  ProcessInspection,
  ProcessRow,
  QuitOutcome,
  Sample,
  ScanStatus,
  SearchResult,
  StartupReport,
  Status,
  SystemClient,
  UninstallResult,
  UpdateReport,
  Volume,
} from "../api.js";
import type { Subscribe, SystemEventMap, SystemHost, Unsubscribe } from "../host.js";

/** Subscribes to one native event; resolves to the unsubscribe once registered. Receives the whole event. */
export type ListenFn = <T>(event: string, handler: (event: { payload: T }) => void) => Promise<Unsubscribe>;

export interface TauriSystemOptions {
  /** How native events reach this window. Default: Tauri's `listen`. */
  listen?: ListenFn;
  /** Native event names, per `SystemEventMap` key; the defaults are the names Pulse's engine emits. */
  events?: Partial<Record<keyof SystemEventMap, string>>;
}

const EVENTS: Record<keyof SystemEventMap, string> = {
  "storage-updated": "storage-updated",
  "apps-row": "apps-row",
  "apps-inventory": "apps-inventory",
  "apps-update-row": "apps-update-row",
  "apps-updates-done": "apps-updates-done",
  "apps-icon": "apps-icon",
  "apps-update-job": "apps-update-job",
  "apps-leftovers": "apps-leftovers",
  visibility: "hub-visibility",
};

/** The Tauri `SystemClient` (storage, processes, disks, cleanup, duplicates, drive health, activity, metrics). */
export function tauriSystemClient(): SystemClient {
  return {
    metricsLease: (client, ttlMs) => invoke<void>("metrics_lease", { client, ttlMs }),
    metricsHistory: (sinceMs) => invoke<Sample[]>("metrics_history", { sinceMs: sinceMs ?? null }),
    activityLease: (client) => invoke<void>("activity_lease", { client }),
    status: () => invoke<Status>("status"),
    processes: () => invoke<Process[]>("processes"),
    scan: (path) => invoke<Folder>("scan", { path: path ?? null }),
    scanStatus: () => invoke<ScanStatus>("scan_status"),
    lastScan: () => invoke<Folder | null>("last_scan"),
    growth: () => invoke<Growth>("growth"),
    children: (path) => invoke<Folder>("children", { path }),
    search: (query, limit, extensions, minBytes) =>
      invoke<SearchResult>("search", { query, limit: limit ?? null, extensions: extensions ?? null, minBytes: minBytes ?? null, changed: null }).then((r) => r.rows),
    searchChanged: (query, minBytes, changed, limit) =>
      invoke<SearchResult>("search", { query, limit: limit ?? null, extensions: null, minBytes: minBytes ?? null, changed: changed ?? null }),
    diskIndexStatus: () => invoke<DiskIndexStatus>("disk_index_status"),
    diskIndexSizes: (paths) => invoke<IndexedSize[]>("disk_index_sizes", { paths }),
    apps: () => invoke<AppEntry[]>("apps_list"),
    appDetail: (path) => invoke<AppDetail>("app_detail", { path }),
    uninstall: (path, bundleId, items) => invoke<UninstallResult>("app_uninstall", { path, bundleId, items }),
    processRows: () => invoke<ProcessRow[]>("process_rows"),
    quit: (r) => invoke<QuitOutcome>("process_quit", { key: r.key, pid: r.lead.pid, startTime: r.lead.start_time }),
    forceQuit: (r) => invoke<QuitOutcome>("process_force_quit", { key: r.key, pid: r.lead.pid, startTime: r.lead.start_time }),
    inspectProcess: (r) => invoke<ProcessInspection>("process_inspect", { pid: r.lead.pid, startTime: r.lead.start_time }),
    volumes: () => invoke<Volume[]>("volumes"),
    eject: (mount) => invoke<void>("eject", { mount }),
    openFullDiskAccess: () => invoke<void>("fda_request"),
    fullDiskAccessStatus: () => invoke<string>("fda_status"),
    cleanupScan: () => invoke<CleanupReport>("cleanup_scan"),
    cleanupCached: () => invoke<CleanupReport | null>("cleanup_cached"),
    cleanupApply: (items: CleanupFinding[]) =>
      invoke<CleanupApplyResult>("cleanup_apply", {
        items: items.map((f) => ({ rule_id: f.rule_id, path: f.path, dev: f.dev, ino: f.ino })),
      }),
    cleanupRestore: (id) => invoke<CleanupRestoreResult>("cleanup_restore", { id }),
    cleanupHistory: () => invoke<CleanupHistory>("cleanup_history"),
    reveal: (path) => invoke<void>("reveal", { path }),
    fileIdentity: (path) => invoke<FileIdentity>("file_identity", { path }),
    finderOpen: (path) => invoke<void>("finder_open", { path }),
    fileChooseFolder: () => invoke<string | null>("file_choose_folder"),
    fileMovePlan: (path, id, destination) => invoke<MovePlan>("file_move_plan", { path, dev: id.dev, ino: id.ino, destination }),
    fileMove: (path, id, destination, copyAcrossVolumes) =>
      invoke<MoveResult>("file_move", { path, dev: id.dev, ino: id.ino, destination, copyAcrossVolumes }),
    fileTrash: (path, id) => invoke<void>("file_trash", { path, dev: id.dev, ino: id.ino }),
    mediaCompressStatus: () => invoke<MediaCompressStatus>("media_compress_status"),
    mediaCompress: (path, options) => invoke<MediaCompressOutcome>("media_compress", { path, options }),
    storageInspect: (path) => invoke<Inspect>("storage_inspect", { path }),
    storagePreview: (path) => invoke<ArrayBuffer>("storage_preview", { path }),
    driveHealth: (mounts) => invoke<HealthReport>("drive_health", { mounts }),
    activityTimeline: (limit, before) => invoke<ActivityTimeline>("activity_timeline", { limit, before }),
    homePath: () => invoke<string>("home_path"),
    duplicatesScan: (path) => invoke<DuplicateReport>("duplicates_scan", { path }),
    duplicatesTrash: (items) => invoke<DuplicatesTrashResult>("duplicates_trash", { items }),
  };
}

/** The Tauri `AppsClient` (inventory, updates, startup items). */
export function tauriAppsClient(): AppsClient {
  return {
    // The old screen treated a failure as "no app asked for".
    initialApp: () => invoke<string | null>("initial_app").catch(() => null),
    cached: () => invoke<CachedApps>("apps_cached"),
    refresh: () => invoke<void>("apps_refresh"),
    summary: (path) => invoke<AppEntry>("app_summary", { path }),
    leftovers: (path) => invoke<void>("app_leftovers", { path }),
    icons: (paths) => invoke<Record<string, string>>("app_icons", { paths }),
    updatesCached: () => invoke<UpdateReport>("apps_updates_cached"),
    updatesRefresh: (force) => invoke<void>("apps_updates_refresh", { force }),
    update: (path) => invoke<string>("app_update", { path }),
    startupItems: () => invoke<StartupReport>("startup_items"),
  };
}

/** The typed event subscription over Tauri's `listen` (or `options.listen`). */
export function tauriSystemSubscribe(options: TauriSystemOptions = {}): Subscribe {
  const listen: ListenFn = options.listen ?? ((event, handler) => tauriListen(event, (e) => handler(e as never)));
  const names = { ...EVENTS, ...options.events };
  return (event, handler) => listen(names[event], handler);
}

/**
 * The whole `SystemHost`: `configureSystemHost(tauriSystemHost({ appName, storagePrefix }))`. This is also the one call
 * `@rightkit/agent-ui-approvals`' Monitor, Overview and Settings screens need (they read `api` and `appsApi`).
 */
export function tauriSystemHost(
  config: Pick<SystemHost, "appName" | "storagePrefix">,
  options: TauriSystemOptions = {},
): SystemHost {
  return {
    ...config,
    client: tauriSystemClient(),
    apps: tauriAppsClient(),
    subscribe: tauriSystemSubscribe(options),
  };
}
