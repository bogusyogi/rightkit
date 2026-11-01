import { invoke } from "@tauri-apps/api/core";
import { getPlatform } from "@rightkit/platform-ui";
import { formatBytes } from "@rightkit/settings-ui/core";

/** True in the Windows webview: the hub runs one page on both systems and a few strings differ. */
export const isWindows = getPlatform() === "windows";

export interface Metric<T> {
  value: T | null;
  capability: string;
  label: string;
}

export interface Disk {
  mount_point: string;
  total_bytes: number | null;
  /** Physically free: what the file system would let a write use now. */
  available_bytes: number | null;
  /** macOS only: more space the system could reclaim (snapshots, caches); never added to the free figure. */
  reclaimable_bytes?: number | null;
  removable: boolean;
}

export interface Status {
  cpu_usage_percent: Metric<number>;
  memory_used_bytes: Metric<number>;
  memory_total_bytes: Metric<number>;
  memory_pressure: Metric<string>;
  swap_used_bytes: Metric<number>;
  swap_total_bytes: Metric<number>;
  disks: Disk[];
}

export interface Process {
  identity: { pid: number; start_time: number };
  name: string;
  cpu_usage_percent: number;
  memory: Metric<number>;
}

/** State of one volume's whole-disk index. `partial`: some folders were skipped or unreadable (Full Disk Access). */
export interface DiskIndexVolume {
  mount_point: string;
  state: "building" | "ready" | "failed";
  entries: number;
  crawl_ms: number;
  resumed_from_snapshot: boolean;
  full_disk_access: boolean;
  partial: boolean;
  unreadable_dirs: number;
  error: string | null;
  health?: DiskIndexHealth | null;
}
/** `since` is Unix seconds. Absent until the monitor's first tick and on platforms without the index. */
export interface DiskIndexHealth {
  state: "healthy" | "unhealthy";
  reasons: string[];
  since: number;
}
export interface DiskIndexStatus {
  supported: boolean;
  volumes: DiskIndexVolume[];
  health?: DiskIndexHealth | null;
}
/** Image and video compression (the host app #202): the switch, whether a verified FFmpeg is there, and why not. */
export interface MediaCompressStatus {
  enabled: boolean;
  available: boolean;
  formats: string[];
  note: string | null;
}
/** Measured from the written copy, never estimated. */
export interface MediaCompressOutcome {
  source: string;
  output: string;
  source_bytes: number;
  output_bytes: number;
  saved_bytes: number;
}
/** `known: false` means the index does not hold the path yet; the numbers are then meaningless. */
export interface IndexedSize {
  path: string;
  known: boolean;
  allocated: number;
  allocated_unique: number;
  logical: number;
  files: number;
}

export interface Row {
  path: string;
  name: string;
  is_dir: boolean;
  bytes: number;
  /** A "smaller files" total, not a real path. */
  summary?: boolean;
  /** Modified time in Unix seconds (a folder's: the newest below it); 0 or absent when unknown. */
  modified?: number;
}

/** A range of modified times in Unix seconds: `since` inclusive, `before` exclusive. */
export interface ChangedRange {
  since?: number;
  before?: number;
}

/** What a search found, with the matches a date range left out for having no date. */
export interface SearchResult {
  rows: Row[];
  /** Which source answered: Windows Search, the Mac file index, or the saved scan. */
  source?: "windows_search" | "file_index" | "saved_scan";
  undated: number;
  /** The saved scan carries modified times; false until it is scanned again. */
  has_dates: boolean;
}

/** An item's identity when the person asked for an action on it (device and inode). */
export interface FileIdentity {
  dev: number;
  ino: number;
  is_dir: boolean;
}

/** What the Storage inspector reads for one item; null means it could not be read. */
export interface Inspect {
  is_dir: boolean;
  is_link: boolean;
  logical_bytes: number | null;
  created_at: number | null;
  modified_at: number | null;
  files: number | null;
  folders: number | null;
}

export interface MovePlan {
  target: string;
  same_volume: boolean;
}

export interface MoveResult {
  target: string;
  copied: boolean;
}

export interface Folder {
  path: string;
  root: string;
  rows: Row[];
  total_children: number;
  incomplete: boolean;
  needs_access: boolean;
  /** Items the scan could not read (long paths, access denied, entry limit). */
  unread?: number;
  limited: boolean;
  root_label: string;
  /** Unix seconds when this data was scanned. */
  scanned_at: number;
  /** Shown from the last saved scan rather than a scan made this session. */
  from_snapshot: boolean;
  /** Rows carry modified times; false for a scan saved before they were kept. */
  has_dates?: boolean;
}

export interface ScanStatus {
  running: boolean;
  running_root: string | null;
  has_index: boolean;
  root: string | null;
  scanned_at: number | null;
  from_snapshot: boolean;
  /** The index is kept current while the hub runs; without it a saved scan only gets older. */
  live_refresh: boolean;
}

export interface Volume {
  name: string;
  mount_point: string;
  /** Null for a network volume that did not answer in time (never zero). */
  total_bytes: number | null;
  /** Physically free: what the file system would let a write use now. */
  available_bytes: number | null;
  /** macOS only: more space the system could reclaim (snapshots, caches); never added to the free figure. */
  reclaimable_bytes?: number | null;
  removable: boolean;
  internal: boolean;
  disk_image: boolean;
  /** "network" for a mounted SMB, AFP, NFS or WebDAV share or a mapped network drive. */
  kind: "local" | "network";
  /** The server and share of a network volume, when known. */
  detail: string | null;
  /** "not responding" for a network volume that did not answer in time. */
  state: "ok" | "not responding";
}

export interface CleanupFinding {
  id: string;
  rule_id: string;
  rule_name?: string;
  category: string;
  name: string;
  path: string;
  bytes: number;
  apparent_bytes?: number;
  partial: boolean;
  risk: "safe" | "review" | "info";
  eligible: boolean;
  preselected: boolean;
  reason: string;
  action: string;
  dev: string;
  ino: string;
  /** What the item is ("cache", "virtual machine", ...); absent in findings saved before this field. */
  kind?: string;
  /** Plain-words safety note ("old release build"); empty when not described. */
  safety?: string;
  /** Who made the item: the owning app, tool or project; empty when unknown. */
  owner?: string;
  /** Unix seconds the item was last written; 0 when unknown. */
  last_used?: number;
}

export interface ChromeSnapshots {
  count: number;
  apparent_bytes: number;
  reclaimable_bytes: number;
  reclaimable_known: boolean;
  running: boolean;
  since_at: number | null;
  since_count: number | null;
}

export interface CleanupReport {
  chrome_snapshots?: ChromeSnapshots | null;
  findings: CleanupFinding[];
  safe_bytes: number;
  review_bytes: number;
  scanned_at: number;
}

export interface CleanupApplyResult {
  moved_items: number;
  moved_bytes: number;
  skipped: { path: string; reason: string }[];
  activity_id: string | null;
}

export interface AppEntry {
  name: string;
  path: string;
  bundle_id: string | null;
  version: string | null;
  size_bytes: number;
  last_used: number | null;
  running: boolean;
  protected: string | null;
}

export interface RelatedItem {
  path: string;
  /** Library folder, or "Application". */
  label: string;
  /** "Application", "User Library", "System Library" or "Installer receipt". */
  location: string;
  exact: boolean;
  /** exact, helper, group, prefix, team, name or receipt. */
  confidence: string;
  reason: string;
  /** Root-owned: Finder asks for an administrator password. */
  admin: boolean;
  size_bytes: number;
  preselected: boolean;
}

export interface BackgroundEntry {
  kind: string;
  label: string;
  path: string | null;
}

export interface AppDetail {
  app: AppEntry;
  items: RelatedItem[];
  background: BackgroundEntry[];
  receipts: string[];
}

export interface UninstallResult {
  moved: { path: string; bytes: number }[];
  failed: { path: string; error: string }[];
  /** Same value as moved_bytes. Moved to the Trash or Recycle Bin frees nothing until it is emptied. */
  bytes_moved: number;
  moved_bytes: number;
}

/** One listening or bound socket of a process, from `process_inspect`. */
export interface ListeningPort {
  protocol: string;
  family: string;
  address: string;
  port: number;
}

/** On-demand inspection of one process. A null list could not be read; its note says why. */
export interface ProcessConnection {
  protocol: string;
  family: string;
  /** An address, never a looked-up name: nothing is resolved. */
  remote_address: string;
  remote_port: number;
}

export interface ProcessInspection {
  listening: ListeningPort[] | null;
  /** Established connections: the remote ends the process talks to. Null when unreadable; absent from older engines. */
  connections?: ProcessConnection[] | null;
  open_files: string[] | null;
  notes: string[];
}

export interface ProcessMember {
  identity: { pid: number; start_time: number };
  name: string;
  cpu_usage_percent: number;
  /** Memory the process is responsible for: on macOS the physical footprint, including compressed and swapped pages. */
  memory_bytes: number;
  /** Resident size (in RAM now) when it differs from `memory_bytes`; absent when unknown. */
  resident_bytes?: number;
  /** Null when the platform did not report it. */
  threads: number | null;
  /** Windows: share of the busiest GPU engine, 0..100; null when unreadable or switched off. */
  gpu_percent?: number | null;
  /** Unix seconds; null when unreadable. */
  started_at: number | null;
}

export interface ProcessRow {
  key: string;
  name: string;
  bundle_id: string | null;
  app_path: string | null;
  lead: { pid: number; start_time: number };
  cpu_usage_percent: number;
  memory_bytes: number;
  /** Resident size (in RAM now) when it differs from `memory_bytes`; absent when unknown. */
  resident_bytes?: number;
  /** Null when any member's count is unreadable. */
  threads: number | null;
  /** Windows: the members' GPU use added up (of the busiest engine); null when unreadable or switched off. */
  gpu_percent?: number | null;
  /** Bytes read and written since the previous refresh, plus lifetime totals, summed over the group (#324 item 10). Absent from older engines. Windows counts include pipe and network I/O. */
  disk_io?: {
    read_bytes: number;
    written_bytes: number;
    total_read_bytes: number;
    total_written_bytes: number;
  } | null;
  /** Unix seconds the lead started; null when unreadable. */
  started_at: number | null;
  /** The app bundle or the executable; null when unknown. */
  executable: string | null;
  members: ProcessMember[];
  can_act: boolean;
  refusal: string | null;
}

export interface Change {
  path: string;
  /** Signed change in bytes. */
  bytes: number;
}

export interface Growth {
  available: boolean;
  /** Unix seconds of the scan compared against. */
  since: number | null;
  grown: Change[];
  shrunk: Change[];
  reason: string | null;
}

export type QuitOutcome = "quit" | "still_running";

/** One Activity timeline row. `at` is Unix seconds; `open` names the section that owns it. */
export interface ActivityRow {
  at: number;
  kind: "Cleanup" | "Undo" | "Uninstall" | "App update" | "Drive health" | "Scan" | "Send" | "Chat" | "Remote" | "File index" | "Hub";
  summary: string;
  open: "cleanup" | "apps" | "storage" | "chat" | null;
}

export interface ActivityTimeline {
  rows: ActivityRow[];
  /** Sources that exist but could not be read. */
  unreadable: string[];
}

export const api = {
  status: () => invoke<Status>("status"),
  processes: () => invoke<Process[]>("processes"),
  scan: (path?: string) => invoke<Folder>("scan", { path: path ?? null }),
  scanStatus: () => invoke<ScanStatus>("scan_status"),
  lastScan: () => invoke<Folder | null>("last_scan"),
  growth: () => invoke<Growth>("growth"),
  children: (path: string) => invoke<Folder>("children", { path }),
  /** `minBytes` keeps only items larger than that many bytes. */
  search: (query: string, limit?: number, extensions?: string[], minBytes?: number) =>
    invoke<SearchResult>("search", { query, limit: limit ?? null, extensions: extensions ?? null, minBytes: minBytes ?? null, changed: null }).then((r) => r.rows),
  /** `search` with a modified-time range; says how many matches had no date. */
  searchChanged: (query: string, minBytes?: number, changed?: ChangedRange, limit?: number) =>
    invoke<SearchResult>("search", { query, limit: limit ?? null, extensions: null, minBytes: minBytes ?? null, changed: changed ?? null }),
  diskIndexStatus: () => invoke<DiskIndexStatus>("disk_index_status"),
  diskIndexSizes: (paths: string[]) => invoke<IndexedSize[]>("disk_index_sizes", { paths }),
  apps: () => invoke<AppEntry[]>("apps_list"),
  appDetail: (path: string) => invoke<AppDetail>("app_detail", { path }),
  uninstall: (path: string, bundleId: string | null, items: string[]) =>
    invoke<UninstallResult>("app_uninstall", { path, bundleId, items }),
  processRows: () => invoke<ProcessRow[]>("process_rows"),
  quit: (r: ProcessRow) =>
    invoke<QuitOutcome>("process_quit", { key: r.key, pid: r.lead.pid, startTime: r.lead.start_time }),
  forceQuit: (r: ProcessRow) =>
    invoke<QuitOutcome>("process_force_quit", { key: r.key, pid: r.lead.pid, startTime: r.lead.start_time }),
  /** Listening ports and open files of one process, read now. Nothing is sampled in the background. */
  inspectProcess: (r: ProcessRow) =>
    invoke<ProcessInspection>("process_inspect", { pid: r.lead.pid, startTime: r.lead.start_time }),
  volumes: () => invoke<Volume[]>("volumes"),
  eject: (mount: string) => invoke<void>("eject", { mount }),
  /** Opens the Full Disk Access pane in System Settings. */
  openFullDiskAccess: () => invoke<void>("fda_request"),
  fullDiskAccessStatus: () => invoke<string>("fda_status"),
  cleanupScan: () => invoke<CleanupReport>("cleanup_scan"),
  /** The last saved findings, or null when none were saved. Never scans. */
  cleanupCached: () => invoke<CleanupReport | null>("cleanup_cached"),
  cleanupApply: (items: CleanupFinding[]) =>
    invoke<CleanupApplyResult>("cleanup_apply", {
      items: items.map((f) => ({ rule_id: f.rule_id, path: f.path, dev: f.dev, ino: f.ino })),
    }),
  cleanupRestore: (id: string) => invoke<unknown>("cleanup_restore", { id }),
  reveal: (path: string) => invoke<void>("reveal", { path }),
  /** Device and inode of an item now; the file actions check them again before acting. */
  fileIdentity: (path: string) => invoke<FileIdentity>("file_identity", { path }),
  /** Opens a folder, or selects a file, in Finder. Read-only. */
  finderOpen: (path: string) => invoke<void>("finder_open", { path }),
  /** The native folder picker; null when cancelled. */
  fileChooseFolder: () => invoke<string | null>("file_choose_folder"),
  fileMovePlan: (path: string, id: FileIdentity, destination: string) =>
    invoke<MovePlan>("file_move_plan", { path, dev: id.dev, ino: id.ino, destination }),
  /** Renames within one drive; across drives, copies then trashes the original when copyAcrossVolumes is set. */
  fileMove: (path: string, id: FileIdentity, destination: string, copyAcrossVolumes: boolean) =>
    invoke<MoveResult>("file_move", { path, dev: id.dev, ino: id.ino, destination, copyAcrossVolumes }),
  fileTrash: (path: string, id: FileIdentity) => invoke<void>("file_trash", { path, dev: id.dev, ino: id.ino }),
  mediaCompressStatus: () => invoke<MediaCompressStatus>("media_compress_status"),
  mediaCompress: (path: string, options?: { quality?: number; max_dimension?: number }) =>
    invoke<MediaCompressOutcome>("media_compress", { path, options }),
  /** Facts the scan does not hold for one item: read-only, one path, never recursive. */
  storageInspect: (path: string) => invoke<Inspect>("storage_inspect", { path }),
  /** The raw bytes of one image for the inspector's preview; rejects with the reason when there is no preview. */
  storagePreview: (path: string) => invoke<ArrayBuffer>("storage_preview", { path }),
  /** Drive health for these mounts; samples smartctl only when ten minutes have passed. */
  driveHealth: (mounts: string[]) => invoke<HealthReport>("drive_health", { mounts }),
  /** Read-only merge of the records each feature keeps; newest first, at most `limit` rows older than `before`. */
  activityTimeline: (limit: number, before: number | null) => invoke<ActivityTimeline>("activity_timeline", { limit, before }),
  homePath: () => invoke<string>("home_path"),
  duplicatesScan: (path: string) => invoke<DuplicateReport>("duplicates_scan", { path }),
  /** Moves each extra copy to the Trash after re-checking it; the kept copy is never touched. */
  duplicatesTrash: (items: { kept: string; path: string }[]) =>
    invoke<DuplicatesTrashResult>("duplicates_trash", { items }),
};

// ---- Drive health (smartctl, kept by core) ----

export interface SelfTest {
  kind: string;
  result: string;
  passed: boolean | null;
  power_on_hours: number | null;
}

/** One successful SMART reading. `at` is Unix seconds. */
export interface HealthReading {
  at: number;
  passed: boolean | null;
  temperature_c: number | null;
  wear_percent: number | null;
  written_bytes: number | null;
  power_on_hours: number | null;
  critical_warning: number | null;
  media_errors: number | null;
  self_tests: SelfTest[];
}

export interface HealthAlert {
  id: string;
  at: number;
  disk: string;
  kind: "warning" | "wear" | "media_errors";
  message: string;
}

export interface DriveCard {
  mount: string;
  disk: string | null;
  model: string | null;
  status: "ok" | "warning" | "unavailable" | "unknown";
  reachable: boolean;
  /** The latest good reading; when the connection hides SMART, the last one kept, with its date. */
  latest: HealthReading | null;
  history: HealthReading[];
}

export interface HealthReport {
  tool_available: boolean;
  sampled_at: number | null;
  drives: DriveCard[];
  /** Newest first. */
  alerts: HealthAlert[];
}

// ---- Duplicates (exact content, bounded by core) ----

export interface DuplicateGroup {
  kept_path: string;
  extras: string[];
  size_bytes: number;
}

export interface DuplicateReport {
  groups: DuplicateGroup[];
  skipped: { path: string; reason: string }[];
  diagnostics: string[];
  truncated: boolean;
  files_considered: number;
  bytes_read: number;
}

export interface DuplicatesTrashResult {
  moved: { path: string; bytes: number }[];
  skipped: { path: string; reason: string }[];
  moved_bytes: number;
}

/** Decimal byte count ("1.5 GB"), or "—" when the value is unknown. */
export function bytes(n: number | null | undefined): string {
  return n == null ? "—" : formatBytes(n);
}

/** "136.3 GB more reclaimable by macOS" when it is at least 1 GB and 2% of the drive, else null. */
export function reclaimableNote(v: { total_bytes: number | null; reclaimable_bytes?: number | null }): string | null {
  const r = v.reclaimable_bytes;
  if (r == null || v.total_bytes == null || v.total_bytes <= 0) return null;
  return r >= 1e9 && r >= v.total_bytes * 0.02 ? `${bytes(r)} more reclaimable by macOS` : null;
}

/** Saved results older than this refresh in the background when their view opens. */
const STALE_SECS = 10 * 60;

/** True when a saved result (Unix seconds) is old enough to refresh. */
export function isStale(secs: number): boolean {
  return Date.now() / 1000 - secs > STALE_SECS;
}

/** "just now", "5 min ago", "2 h ago", or the date for anything older. */
export function ago(secs: number): string {
  const elapsed = Math.max(0, Date.now() / 1000 - secs);
  if (elapsed < 90) return "just now";
  if (elapsed < 3600) return `${Math.round(elapsed / 60)} min ago`;
  if (elapsed < 86400) return `${Math.round(elapsed / 3600)} h ago`;
  return new Date(secs * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** Colour for a 0–1 share, matching the notch rings. */
export function tone(fraction: number): string {
  if (fraction >= 0.85) return "var(--rk-bad)";
  if (fraction >= 0.6) return "var(--rk-warn)";
  return "var(--rk-ok)";
}

export function signedBytes(n: number): string {
  return `${n < 0 ? "−" : "+"}${bytes(Math.abs(n))}`;
}

// ---- Apps: saved inventory, streamed leftovers, icons and updates ----

export interface CachedApps {
  /** Unix seconds when the saved list was written; null when there is none. */
  saved_at: number | null;
  apps: AppEntry[];
}

/** One source's share of an app's leftovers (`apps-leftovers` events). */
export interface LeftoverPart {
  source: "bundle" | "vendor" | "background" | "library" | "receipts";
  items: RelatedItem[];
  background: BackgroundEntry[];
  receipts: string[];
}

export type LeftoversEvent =
  | { kind: "part"; path: string; part: LeftoverPart }
  | { kind: "done"; path: string; error: string | null };

/** Update state of one app. Only Sparkle, Homebrew and App Store apps can have one. */
export interface AppUpdate {
  path: string;
  name: string;
  bundle_id: string | null;
  installed_version: string | null;
  source: "app_store" | "homebrew" | "sparkle" | "winget" | "none";
  state: "available" | "current" | "app_store" | "unknown" | "unavailable";
  latest_version: string | null;
  cask: string | null;
  store_url: string | null;
  reason: string | null;
  /** Unix seconds when this row was checked. */
  checked_at: number;
}

export interface UpdateReport {
  /** Unix seconds of the newest check; null before any check. */
  checked_at: number | null;
  apps: AppUpdate[];
}

export interface UpdateJob {
  path: string;
  state: "running" | "done" | "failed";
  message: string;
}

/** One login or background item that starts by itself (src-tauri/src/startup_items.rs). Read only. */
export interface StartupItem {
  name: string;
  kind: string;
  scope: "user" | "all users" | "system";
  /** The plist, shortcut or executable; null when there is no file. */
  path: string | null;
  /** What it runs; null when unknown. */
  program: string | null;
  /** null when it cannot be told. */
  enabled: boolean | null;
  vendor: string | null;
}

export interface StartupReport {
  rows: StartupItem[];
  /** Plain notes on sources that could not be read or are not listed. */
  notes: string[];
}

export const appsApi = {
  cached: () => invoke<CachedApps>("apps_cached"),
  refresh: () => invoke<void>("apps_refresh"),
  summary: (path: string) => invoke<AppEntry>("app_summary", { path }),
  leftovers: (path: string) => invoke<void>("app_leftovers", { path }),
  /** Cached icons by path; the rest arrive as `apps-icon` events. */
  icons: (paths: string[]) => invoke<Record<string, string>>("app_icons", { paths }),
  updatesCached: () => invoke<UpdateReport>("apps_updates_cached"),
  updatesRefresh: (force: boolean) => invoke<void>("apps_updates_refresh", { force }),
  /** Returns "running", "opened" or "store". */
  update: (path: string) => invoke<string>("app_update", { path }),
  /** Startup and login items, read only. Called when the Starts by itself section opens. */
  startupItems: () => invoke<StartupReport>("startup_items"),
};

// ---- Nearby sharing (LocalSend protocol, run by the hub: src-tauri/src/share.rs) ----

export interface ShareDevice {
  fingerprint: string;
  alias: string;
  deviceModel?: string | null;
  deviceType?: string | null;
  ip: string;
  port: number;
  protocol: string;
}

export interface ShareIncoming {
  id: string;
  from: string;
  fingerprint: string;
  fileCount: number;
  totalBytes: number;
  isMessage: boolean;
  preview?: string | null;
  files: { name: string; size: number }[];
  known: boolean;
}

export interface ShareTransfer {
  id: string;
  direction: "send" | "receive";
  peer: string;
  state: "waiting" | "active" | "done" | "failed" | "cancelled" | "declined";
  totalBytes: number;
  doneBytes: number;
  filesTotal: number;
  filesDone: number;
  current?: string | null;
  savedTo?: string | null;
  savedFiles: string[];
  error?: string | null;
}

export interface ShareSnapshot {
  running: boolean;
  error: string | null;
  alias?: string;
  saveDir?: string;
  devices: ShareDevice[];
  incoming: ShareIncoming[];
  transfers: ShareTransfer[];
  warnings: string[];
  localNetwork: "unknown" | "granted" | "blocked";
}

export const shareApi = {
  state: () => invoke<ShareSnapshot>("share_state"),
  devices: () => invoke<ShareDevice[]>("share_devices"),
  /** Send files and/or text to a device (by fingerprint). Resolves with the transfer id. */
  send: (to: string, paths: string[], text?: string) => invoke<string>("share_send", { to, paths, text }),
  accept: (id: string) => invoke<boolean>("share_accept", { id }),
  decline: (id: string) => invoke<boolean>("share_decline", { id }),
  cancel: (id: string) => invoke<boolean>("share_cancel", { id }),
  dismiss: (id: string) => invoke<void>("share_dismiss", { id }),
  /**
   * Events from the service: `share-devices` (ShareDevice[]), `share-incoming` (ShareIncoming),
   * `share-incoming-resolved` (id), `share-progress` (ShareTransfer).
   */
};

// ---- devices (device link; the resident engine's `device.*` methods, hub/src-tauri/src/commands.rs) ----

/** Effect classes a paired device may request (`agent/effect.rs`, kebab-case). */
export const DEVICE_GRANT_CLASSES = ["reversible", "irreversible-destruction", "spend", "publication"] as const;
export type DeviceGrantClass = (typeof DEVICE_GRANT_CLASSES)[number];

/** One device from `device.list` (the answer is `{ devices: DeviceRecord[] }`). */
export interface DeviceRecord {
  deviceId: string;
  name: string;
  platform: string;
  /** Hex key fingerprint; null until the engine reports it. */
  keyFingerprint: string | null;
  state: string;
  grants: string[];
  grantsExpireAtMs: number | null;
  revision: number;
  /** Null when the engine does not say whether the link is up. */
  linkUp: boolean | null;
  /** The computer may add devices for the circle. */
  admit?: boolean;
}

/** The answer to `device.list`. */
export interface DeviceListResult {
  devices: DeviceRecord[];
}

/**
 * `code`: only the code is known; the owner types it on the other computer. Name, platform, id and
 * fingerprint are empty and Approve is not offered. `confirm`: the other computer has confirmed and
 * the full card with Approve and Reject is shown.
 */
export type PendingPairingStage = "code" | "confirm";

/** The live pairing awaiting the owner (`device.pair.pending` → `pending`). */
export interface PendingPairing {
  pairingId: string;
  /** Eight digits, shown as 4+4. */
  code: string;
  stage: PendingPairingStage;
  deviceName: string;
  platform: string;
  deviceId: string;
  keyFingerprint: string;
  expiresAtMs: number;
}

/** The answer to `device.pair.pending`: `pending` is set only while `open` is true. */
export interface PendingPairingResult {
  open: boolean;
  pending: PendingPairing | null;
}

/** What `device.pair.start` returns (the code is never in it). */
export interface PairingStarted {
  pairingId: string;
  expiresAtMs: number;
  /** The single-use join secret (64 hex) the QR carries as `s=`; a phone that proves it joins with one scan. */
  secret?: string | null;
  /** This computer's iroh endpoint id (64 hex); a phone's QR carries it. */
  endpointId?: string;
  /** Our relay, when one is set; null otherwise. */
  relayUrl?: string | null;
  /** This computer's private-network addresses (ip:port); the QR carries them for a phone. */
  addrs?: string[];
}

/** What `device.join` returns once this computer joined a host. */
export interface JoinedHost {
  state: string;
  deviceId: string;
  hostName: string;
}

/** What `device.join.create` returns. `token` and `command` are a bearer secret until used or expired: show them to the owner only. */
export interface JoinCreated {
  /** `pulse-join:1:...` */
  token: string;
  /** `pulse device join <token>`, the one line to run on the computer being added. */
  command: string;
  pairingId: string;
  expiresAtMs: number;
  /** Guest links only (#493): per `uname -m` arch, the pinned artifact and the copyable install command (a bearer secret, it carries the pass). Absent on older engines. */
  linux?: Record<string, { url: string; sha256: string; size: number; glibc: string; command: string }>;
  /** Why no Linux command was made (no published Linux build, unreadable manifest). */
  linuxUnavailable?: string;
}

/** The engine's refusal text is `code: message`; these codes mean the owner check wants a (new) PIN. */
export const OWNER_CHECK_CODES = /^(unlock_needs_auth|pin_not_set|pin_wrong|pin_locked|pin_invalid): (.*)$/s;

/** A Touch ID proof of the owner: the engine's single-use challenge and the signature the Mac notch made over it. The engine verifies it. */
export interface OwnerProof {
  challenge: string;
  sig: unknown;
}

export type PhoneReadMode = "allow" | "deny";
export type PhoneSendMode = "allow" | "ask" | "deny";

/** One paired phone from `device.phone.access`; `error` is set when its policy file is unreadable. */
export interface PhoneAccess {
  deviceId: string;
  name: string;
  read?: PhoneReadMode;
  send?: PhoneSendMode;
  admin?: boolean;
  error?: string;
}

export interface PhoneAccessResult {
  enabled: boolean;
  defaults: { read: PhoneReadMode; send: PhoneSendMode };
  phones: PhoneAccess[];
}

/** A phone send held for the owner (`device.phone.asks`); it times out after 60 s. */
export interface PhoneAsk {
  askId: string;
  deviceId: string;
  deviceName: string;
  chat: string;
  chatName: string;
  preview: string;
  createdAtMs: number;
}

/** One guest from `device.guest.list` (#492): a cloud box that redeemed a guest link. */
export interface GuestRecord {
  deviceId: string;
  name: string;
  platform: string;
  status: "pending" | "approved" | "lapsed";
  /** The three digits the box printed when it joined; compare before approving. */
  checkCode: string;
  /** What the box said about itself; nothing verifies it. */
  reportedSession: string | null;
  mintedBy: string;
  approvedAtMs: number | null;
  leaseExpiresAtMs: number;
  lastSeenMs: number | null;
  /** What it may not do on any computer: add or manage devices, the phone admin path. */
  refused: string[];
}

export const devicesApi = {
  list: () => invoke<DeviceListResult>("device_list"),
  pending: () => invoke<PendingPairingResult>("device_pair_pending"),
  /** Opens a phone pairing. The engine checks the owner (`pin`, the the host app PIN) first. */
  pairStart: (pin?: string, proof?: OwnerProof) =>
    invoke<PairingStarted>("device_pair_start", { pin: pin ?? null, challenge: proof?.challenge ?? null, sig: proof?.sig ?? null }),
  /** Joins the computer whose owner made `token` (`pulse-join:1:...`); nothing else is asked. */
  joinWith: (token: string) => invoke<JoinedHost>("device_join", { token }),
  /** The engine checks the owner itself: `pin` is the the host app PIN. Without it the engine refuses `unlock_needs_auth`. */
  pairApprove: (pairingId: string, pin?: string, proof?: OwnerProof) =>
    invoke<unknown>("device_pair_approve", { pairingId, pin: pin ?? null, challenge: proof?.challenge ?? null, sig: proof?.sig ?? null }),
  /** Grants or takes away a computer's `admit` (it may add devices for the circle); owner-checked like Add a device. */
  admitSet: (deviceId: string, allowed: boolean, pin?: string, proof?: OwnerProof) =>
    invoke<unknown>("device_admit_set", { deviceId, allowed, pin: pin ?? null, challenge: proof?.challenge ?? null, sig: proof?.sig ?? null }),
  /** The enrolled owner-check keys (`remote.owner.key.status`; the Mac notch's and Windows Hello's). */
  ownerKeys: () =>
    invoke<{
      keys: { keyId: string; device: string; dev: boolean; bound?: boolean }[];
      /** Where the owner-check keys are kept; `weaker` is Windows without the elevated the host app service (a file any process of this account can rewrite). */
      ownerCheck?: { protection?: "service" | "file" | "keychain"; weaker?: boolean };
      /** True once a day while the owner key needs confirming (the engine remembers the offer), so a recreated web view does not ask again. */
      bindDue?: boolean;
    }>("owner_key_status"),
  /** Confirms this computer's owner-check key (binds it to this device) with one owner check. Windows signs in the hub; the Mac's notch signs (`notch_command`, `signOwnerChallenge`, op `bind`). */
  ownerKeyBind: () => invoke<unknown>("owner_key_bind"),
  /** Removes one owner-check key; the engine checks `pin`. */
  ownerKeyRevoke: (keyId: string, pin: string) => invoke<unknown>("owner_key_revoke", { keyId, pin }),
  /** "Add a device": the engine checks `pin` (the the host app PIN) first, then makes one join command valid for ten minutes. */
  joinCreate: (pin?: string, proof?: OwnerProof) =>
    invoke<JoinCreated>("device_join_create", { pin: pin ?? null, challenge: proof?.challenge ?? null, sig: proof?.sig ?? null }),
  /** "Make a guest link" (#492): the same owner check as Add a device, then one single-use link for a cloud box. */
  guestCreate: (leaseMs: number, pin?: string, proof?: OwnerProof) =>
    invoke<JoinCreated>("device_guest_create", { pin: pin ?? null, challenge: proof?.challenge ?? null, sig: proof?.sig ?? null, leaseMs }),
  guests: () => invoke<{ guests: GuestRecord[] }>("device_guest_list"),
  /** Owner UI only: makes a pending cloud agent one of your machines until its lease ends. */
  guestApprove: (deviceId: string) => invoke<unknown>("device_guest_approve", { deviceId }),
  /** Voids the open join command. */
  joinCancel: () => invoke<unknown>("device_join_cancel"),
  pairReject: (pairingId: string) => invoke<unknown>("device_pair_reject", { pairingId }),
  revoke: (deviceId: string) => invoke<unknown>("device_revoke", { deviceId }),
  phoneAccess: () => invoke<PhoneAccessResult>("device_phone_access"),
  /** `read` allow|deny|default; `send` allow|ask|deny|default; omit one to leave it. */
  phoneAccessSet: (deviceId: string, change: { read?: PhoneReadMode | "default"; send?: PhoneSendMode | "default" }) =>
    invoke<unknown>("device_phone_access_set", { deviceId, ...change }),
  /** Off revokes the phone's approval key first (system prompt); rejects if cancelled, leaving admin on. */
  phoneAdminSet: (deviceId: string, allow: boolean) => invoke<unknown>("device_phone_admin_set", { deviceId, allow }),
  phoneAsks: () => invoke<{ asks: PhoneAsk[] }>("device_phone_asks"),
  phoneAnswer: (askId: string, allow: boolean) => invoke<unknown>("device_phone_answer", { askId, allow }),
  computerName: () => invoke<string | null>("device_computer_name"),
  /** `revision` is the one the device was read at; `expiresAtMs` null means no expiry. */
  grantsSet: (deviceId: string, revision: number, classes: DeviceGrantClass[], expiresAtMs: number | null) =>
    invoke<unknown>("device_grants_set", { deviceId, revision, classes, expiresAtMs }),
};

// ---- remote approvals (Settings > Remote, "Waiting for your approval"; the engine's `remote.approvals.*`) ----

/** One remote request waiting for the owner's answer (core/src/remote/approvals.rs `Item`, state `pending`). */
export interface RemoteApproval {
  id: string;
  /** `act` (one change) or `session` (a family's grant). */
  kind: string;
  /** The tool for an act (e.g. `files.patch`), the family (`terminal`) for a session. */
  scope: string;
  /** The door the call came in by: `local` or `gateway`. */
  origin: string;
  /** What the request is bound to; shown to nobody, sent back with the answer so the engine can check it. */
  bind: string;
  /** What it will do and the folder or file name it touches; never file contents or secrets. */
  summary: string;
  /** The external chat's client name (unlock requests). */
  client?: string;
  state: string;
  created: number;
  decided: number;
  lastUsed: number;
}

/** The engine answers `remote.approvals.pending` with `{ items: [...] }` (engine/src/contract.rs); a bare list or `{ approvals }` is read too. */
export function pendingApprovals(answer: unknown): RemoteApproval[] {
  const wrapped = answer as { items?: unknown; approvals?: unknown } | null;
  const list = Array.isArray(answer) ? answer : (wrapped?.items ?? wrapped?.approvals);
  return Array.isArray(list) ? (list as RemoteApproval[]).filter((a) => a && typeof a.id === "string") : [];
}

export const remoteApprovalsApi = {
  pending: () => invoke<unknown>("remote_approvals_pending"),
  /** Approves or denies one pending request, bound to the item as displayed (the engine compares a digest of its fields). Owner UI only: the engine checks that the caller is the the host app hub. */
  decide: (a: RemoteApproval, approve: boolean, pin?: string) =>
    invoke<unknown>("remote_approval_decide", {
      id: a.id,
      approve,
      pin: approve ? pin ?? "" : null,
      kind: a.kind,
      scope: a.scope,
      origin: a.origin,
      bind: a.bind,
      summary: a.summary,
    }),
};

/** The per-chat unlock: the the host app PIN and the owner's answer to an unlock request (`scope === "unlock"`). Never log or keep a PIN. */
export const remoteUnlockApi = {
  pinStatus: () => invoke<unknown>("remote_unlock_pin_status"),
  /** `current` is needed only when a PIN is already set. */
  pinSet: (pin: string, current?: string) => invoke<unknown>("remote_unlock_pin_set", { pin, current: current || null }),
  /** Denies, or approves with the the host app PIN (the hub cannot show the system prompt). */
  decide: (a: RemoteApproval, approve: boolean, pin?: string) =>
    invoke<unknown>("remote_unlock_decide", {
      args: {
        id: a.id,
        approve,
        kind: a.kind,
        scope: a.scope,
        origin: a.origin,
        bind: a.bind,
        summary: a.summary,
        pin: approve ? pin ?? "" : null,
      },
    }),
};

// ---- browser logins (bogusyogi/pulse#460: the engine's `browser.remote.*`, per paired computer) ----
// Each call goes through a hub command named `browser_remote_*`, which forwards to the engine method of
// the same name and refuses without an attached engine. The engine owns the approval: Approve asks for a
// nonce (`challenge`) and then `approve`, and the engine itself runs the owner check.

/** A sign-in request an agent made on a paired computer and that waits for the owner. */
export interface BrowserRequest {
  id: string;
  sites: string[];
  reason: string;
  chat: string;
  expires_ms?: number;
}

/** A sign-in session on a paired computer. `needs-owner` is the state the live view is for. */
export interface BrowserSession {
  session: string;
  sites: string[];
  chat: string;
  state: string;
  url?: string;
  title?: string;
}

/** One frame of a session: JPEG, at most 1280 wide; `seq` rises with each new frame. */
export interface BrowserFrame {
  seq: number;
  jpeg_b64: string;
  width: number;
  height: number;
  url: string;
  title: string;
}

/** An input for the session, with x and y in frame pixels. `text` is never logged or kept. */
export type BrowserInput =
  | { type: "click"; x: number; y: number }
  | { type: "text"; text: string }
  | { type: "key"; key: string }
  | { type: "scroll"; dy: number };

/** The rows a list call answers: a bare list, or the list under `items` or the given key. */
export function browserRows<T>(answer: unknown, key?: string): T[] {
  if (Array.isArray(answer)) return answer as T[];
  const wrapped = answer as Record<string, unknown> | null;
  const list = wrapped?.items ?? (key ? wrapped?.[key] : undefined);
  return Array.isArray(list) ? (list as T[]) : [];
}

export const browserRemoteApi = {
  pending: (device: string) => invoke<unknown>("browser_remote_pending", { device }),
  /** A single-use nonce for one approval (valid for 60 seconds). */
  challenge: (device: string, id: string) => invoke<{ nonce: string }>("browser_remote_challenge", { device, id }),
  approve: (device: string, id: string, nonce: string) => invoke<unknown>("browser_remote_approve", { device, id, nonce }),
  deny: (device: string, id: string) => invoke<unknown>("browser_remote_deny", { device, id }),
  sessions: (device: string) => invoke<unknown>("browser_remote_sessions", { device }),
  frame: (device: string, session: string, afterSeq: number) =>
    invoke<BrowserFrame>("browser_remote_frame", { device, session, afterSeq }),
  input: (device: string, session: string, events: BrowserInput[]) =>
    invoke<unknown>("browser_remote_input", { device, session, events }),
  /** Hands the session back to the agent. */
  done: (device: string, session: string) => invoke<unknown>("browser_remote_done", { device, session }),
};

/** One saved item of the agent secure box, as `remote.secure.list` answers: labels, hosts, last four and limits, never a value. */
export interface SecureItem {
  id: string;
  kind: "login" | "card";
  label: string;
  /** A login's one site. */
  host?: string;
  /** A login has a one-time code saved (never the secret). */
  totp?: boolean;
  /** A card's shops. */
  hosts?: string[];
  last4?: string;
  maxCents?: number;
  autoCents?: number;
  monthCents?: number;
}
export interface SecureList { items: SecureItem[]; month?: string; spentCents?: Record<string, number> }
/** `cancelled` when the system dialog was closed; otherwise the saved item's metadata. */
export type SecureSaved = SecureItem | { cancelled: true };

/**
 * The agent secure box (#345 logins, #346 virtual cards). The renderer sends only non-secret fields and the PIN;
 * the username, password, card number, name, expiry and CVC are typed into a system dialog opened by the command
 * and go straight to the engine, so no secret ever exists in this page.
 */
export const secureBoxApi = {
  list: () => invoke<SecureList>("remote_secure_list"),
  remove: (id: string) => invoke<unknown>("remote_secure_remove", { id }),
  saveLogin: (label: string, host: string, pin: string, withTotp = false) =>
    invoke<SecureSaved>("remote_secure_save_login", { label, host, pin, withTotp }),
  /** Adds or replaces a saved login's one-time code; the secret is typed into the system dialog. */
  setTotp: (id: string, host: string, pin: string) =>
    invoke<SecureSaved>("remote_secure_set_totp", { id, host, pin }),
  saveCard: (c: { label: string; hosts: string[]; zip: string; maxCents: number; autoCents: number; monthCents: number }, pin: string) =>
    invoke<SecureSaved>("remote_secure_save_card", { ...c, pin }),
};

/** The sentence the Devices page shows when the engine refuses pairing with `listen_off`. */
export const LISTEN_OFF_SENTENCE =
  "Turn on Settings > Devices > Accept device links (LAN only) on this computer first.";

/** The engine's `listen_off` refusal (engine/src/devicelink_host.rs, pair_start). The hub's engine link
 *  passes only the refusal's message, not its code, so the message text identifies it. */
export function isListenOff(e: unknown): boolean {
  const text = typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
  return text.startsWith("listen_off:") || text.includes("turn on Accept device links first");
}

/** The owner-only security settings (`PROTECTED_KEYS`, core/src/remote/settings.rs; listed in docs/settings.md).
 *  A `hub-commands` file never changes them: they go through the engine's owner check (`remote_setting_set`). */
const PROTECTED_KEYS = new Set([
  "chatUnlock", "chatUnlockMinutes", "remoteBlockedApps", "agentActions", "agentCliPath", "agentBrowser", "agentBrowserSessionMinutes", "remoteRoots", "remoteGateway",
  "remoteChatRead", "phoneChatReadAll", "remoteDocs", "remoteLaunch", "remoteMedia",
  "remoteReadMaxBytes", "remoteFindMaxResults", "remoteChatReadMaxTurns", "remoteChatReadMaxBytes", "remoteImageMaxBytes", "remoteDocsMaxBytes",
  "remoteFormFill", "remoteFormFillStanding", "remoteFormFillProfile", "remoteScreenObserve", "remoteScreenAct",
  "remoteScreenDisclosure", "remoteTerminals", "remoteTermStanding", "remoteTermAllow", "remoteTermDeny",
  "remoteClipboard", "remoteReadOnly", "ledgerTranscriptSearch", "ledgerTranscriptShare", "remoteFilesWrite", "remoteTools",
]);
/** `PROTECTED_PREFIXES` in core: every key under one of these is owner-only too. */
const PROTECTED_PREFIXES = ["remoteFormFill", "remoteScreen"];
export const isProtectedKey = (key: string): boolean =>
  PROTECTED_KEYS.has(key) || PROTECTED_PREFIXES.some((prefix) => key.startsWith(prefix));

/** A protected change waiting for the the host app PIN (the engine asks for it when a change makes a gate less safe). */
export interface PinAsk { key: string; value: unknown; problem: string | null }
let pinListener: ((ask: PinAsk | null) => void) | null = null;
export function onProtectedPin(fn: ((ask: PinAsk | null) => void) | null): void {
  pinListener = fn;
}

/** Sends one protected change to the engine. Without a PIN a change that loosens a gate is refused with
 *  `unlock_needs_auth`; that opens the PIN prompt (`ProtectedPinPrompt`). Never logs or keeps the PIN. */
export async function setProtectedSetting(key: string, value: unknown, pin?: string): Promise<void> {
  try {
    await invoke<unknown>("remote_setting_set", { key, value, pin: pin ? pin : null });
    pinListener?.(null);
  } catch (e) {
    const text = typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
    const m = /^(unlock_needs_auth|pin_not_set|pin_wrong|pin_locked|pin_invalid): (.*)$/s.exec(text);
    if (m && pinListener) pinListener({ key, value, problem: pin || m[1] !== "unlock_needs_auth" ? m[2] : null });
    else throw e;
  }
}

/** Sets one notch setting the way the Settings page does: `notch_command` with a `set` command
 *  (a protected key goes through the engine instead). */
export function setNotchSetting(key: string, value: unknown): Promise<unknown> {
  if (isProtectedKey(key)) return setProtectedSetting(key, value);
  return invoke<unknown>("notch_command", { command: { command: "set", key, value } });
}
