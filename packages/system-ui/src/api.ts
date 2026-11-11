import { getPlatform } from "@rightkit/platform-ui";
import { formatBytes } from "@rightkit/settings-ui/core";
import { systemHost } from "./host.js";

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
  /** The size is the most deleting could free; hard links or clones may free less. */
  upper_bound?: boolean;
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
  skipped: CleanupSkipped[];
  activity_id: string | null;
  warnings?: string[];
  free_space?: CleanupVolumeFree[];
}

export interface CleanupSkipped {
  path: string;
  reason: string;
}

/** What the drive itself reported as available before and after a cleanup. */
export interface CleanupVolumeFree {
  volume: string;
  before_available: number;
  after_available: number;
}

export interface CleanupRestoreResult {
  restored_items: number;
  restored_bytes: number;
  skipped: CleanupSkipped[];
  warnings?: string[];
}

export interface CleanupActivityItem {
  path: string;
  trash_path: string | null;
  bytes: number;
  restored: boolean;
  /** Set when a crash left this item's state unknown; Undo refuses it. */
  interrupted?: string | null;
}

export interface CleanupActivity {
  id: string;
  at: number;
  bytes: number;
  items: CleanupActivityItem[];
  /** Absent in older history, where it means 0. */
  skipped?: number;
  /** Items a crash left unconfirmed; absent in older history, where it means 0. */
  unknown?: number;
  /** True when the host app was interrupted during this cleanup and settled it afterwards. */
  recovered?: boolean;
  /** Observed free space per drive; absent when it could not be read and in older history. */
  free_space?: CleanupVolumeFree[];
}

export interface CleanupHistory {
  actions: CleanupActivity[];
  warnings: string[];
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

/**
 * Every system call the views make (storage, processes, disks, apps, cleanup, duplicates, drive health, activity).
 * The host implements it over its own transport; the package names no command.
 */
/** One point of the live system history (`metricsHistory`). */
export interface Sample {
  /** Unix time, ms. */
  ts: number;
  /** Overall CPU, 0–100. */
  cpu: number;
  memUsed: number;
  memTotal: number;
  swapUsed: number;
  swapTotal: number;
  pressure: string | null;
  /** Bytes per second; null while unread. */
  netDown: number | null;
  netUp: number | null;
}

export interface SystemClient {
  /** Keeps the host sampling every 2 s while `client` renews within `ttlMs`. */
  metricsLease: (client: string, ttlMs: number) => Promise<void>;
  /** Samples newer than `sinceMs` (all kept samples when null). */
  metricsHistory: (sinceMs: number | null) => Promise<Sample[]>;
  /** Renews the live-activity lease of `client` (pages showing live activity poll faster while it lives). */
  activityLease: (client: string) => Promise<void>;
  status: () => Promise<Status>;
  processes: () => Promise<Process[]>;
  scan: (path?: string) => Promise<Folder>;
  scanStatus: () => Promise<ScanStatus>;
  lastScan: () => Promise<Folder | null>;
  growth: () => Promise<Growth>;
  children: (path: string) => Promise<Folder>;
  /** `minBytes` keeps only items larger than that many bytes. */
  search: (query: string, limit?: number, extensions?: string[], minBytes?: number) => Promise<Row[]>;
  /** `search` with a modified-time range; says how many matches had no date. */
  searchChanged: (query: string, minBytes?: number, changed?: ChangedRange, limit?: number) => Promise<SearchResult>;
  diskIndexStatus: () => Promise<DiskIndexStatus>;
  diskIndexSizes: (paths: string[]) => Promise<IndexedSize[]>;
  apps: () => Promise<AppEntry[]>;
  appDetail: (path: string) => Promise<AppDetail>;
  uninstall: (path: string, bundleId: string | null, items: string[]) => Promise<UninstallResult>;
  processRows: () => Promise<ProcessRow[]>;
  quit: (r: ProcessRow) => Promise<QuitOutcome>;
  forceQuit: (r: ProcessRow) => Promise<QuitOutcome>;
  /** Listening ports and open files of one process, read now. Nothing is sampled in the background. */
  inspectProcess: (r: ProcessRow) => Promise<ProcessInspection>;
  volumes: () => Promise<Volume[]>;
  eject: (mount: string) => Promise<void>;
  /** Opens the Full Disk Access pane in System Settings. */
  openFullDiskAccess: () => Promise<void>;
  fullDiskAccessStatus: () => Promise<string>;
  cleanupScan: () => Promise<CleanupReport>;
  /** The last saved findings, or null when none were saved. Never scans. */
  cleanupCached: () => Promise<CleanupReport | null>;
  cleanupApply: (items: CleanupFinding[]) => Promise<CleanupApplyResult>;
  cleanupRestore: (id: string) => Promise<CleanupRestoreResult>;
  /** Past cleanups, newest first, with what each moved and whether it can be undone. */
  cleanupHistory: () => Promise<CleanupHistory>;
  reveal: (path: string) => Promise<void>;
  /** Device and inode of an item now; the file actions check them again before acting. */
  fileIdentity: (path: string) => Promise<FileIdentity>;
  /** Opens a folder, or selects a file, in Finder. Read-only. */
  finderOpen: (path: string) => Promise<void>;
  /** The native folder picker; null when cancelled. */
  fileChooseFolder: () => Promise<string | null>;
  fileMovePlan: (path: string, id: FileIdentity, destination: string) => Promise<MovePlan>;
  /** Renames within one drive; across drives, copies then trashes the original when copyAcrossVolumes is set. */
  fileMove: (path: string, id: FileIdentity, destination: string, copyAcrossVolumes: boolean) => Promise<MoveResult>;
  fileTrash: (path: string, id: FileIdentity) => Promise<void>;
  mediaCompressStatus: () => Promise<MediaCompressStatus>;
  mediaCompress: (path: string, options?: {
      quality?: number;
      max_dimension?: number;
  }) => Promise<MediaCompressOutcome>;
  /** Facts the scan does not hold for one item: read-only, one path, never recursive. */
  storageInspect: (path: string) => Promise<Inspect>;
  /** The raw bytes of one image for the inspector's preview; rejects with the reason when there is no preview. */
  storagePreview: (path: string) => Promise<ArrayBuffer>;
  /** Drive health for these mounts; samples smartctl only when ten minutes have passed. */
  driveHealth: (mounts: string[]) => Promise<HealthReport>;
  /** Read-only merge of the records each feature keeps; newest first, at most `limit` rows older than `before`. */
  activityTimeline: (limit: number, before: number | null) => Promise<ActivityTimeline>;
  homePath: () => Promise<string>;
  duplicatesScan: (path: string) => Promise<DuplicateReport>;
  /** Moves each extra copy to the Trash after re-checking it; the kept copy is never touched. */
  duplicatesTrash: (items: {
      kept: string;
      path: string;
  }[]) => Promise<DuplicatesTrashResult>;
}


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

/** One login or background item that starts by itself. Read only. */
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

/** The app inventory, updates and startup items. Rows and icons also arrive as `SystemEventMap` events. */
export interface AppsClient {
  /** The app the host was asked to open at start (`--app <path>`), or null. */
  initialApp: () => Promise<string | null>;
  cached: () => Promise<CachedApps>;
  refresh: () => Promise<void>;
  summary: (path: string) => Promise<AppEntry>;
  leftovers: (path: string) => Promise<void>;
  /** Cached icons by path; the rest arrive as `apps-icon` events. */
  icons: (paths: string[]) => Promise<Record<string, string>>;
  updatesCached: () => Promise<UpdateReport>;
  updatesRefresh: (force: boolean) => Promise<void>;
  /** Returns "running", "opened" or "store". */
  update: (path: string) => Promise<string>;
  /** Startup and login items, read only. Called when the Starts by itself section opens. */
  startupItems: () => Promise<StartupReport>;
}

/** The host's system client (see `configureSystemHost`). */
export const api: SystemClient = new Proxy({} as SystemClient, {
  get: (_target, key) => systemHost().client[key as keyof SystemClient],
});

/** The host's apps client (see `configureSystemHost`). */
export const appsApi: AppsClient = new Proxy({} as AppsClient, {
  get: (_target, key) => systemHost().apps[key as keyof AppsClient],
});
