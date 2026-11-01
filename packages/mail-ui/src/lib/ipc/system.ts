import { invoke } from "./invoke.js";

export interface DbHealth {
  ok: boolean;
  encrypted: boolean;
  db_path: string;
  tables: number;
  account_count: number;
  error: string | null;
}

/** Compile-time identity of the running build (see src-tauri/src/build_info.rs). */
export interface BuildInfo {
  version: string;
  channel: "dev" | "release";
  commit: string;
  commit_short: string;
  dirty: boolean | null;
  build_time: string;
  profile: string;
  overrides: boolean;
}

export function buildInfo(): Promise<BuildInfo> {
  return invoke<BuildInfo>("build_info");
}

export function dbHealth(): Promise<DbHealth> {
  return invoke<DbHealth>("db_health");
}

/// L13: cheap, non-blocking probe — `true` when the writer connection is
/// currently held (or the DB isn't open). Used to defer the debounced
/// sync-complete refresh by one extra cycle instead of firing straight into
/// a busy writer. Never throws under normal operation; callers should still
/// treat a rejected promise as "unknown, don't defer" (see useAppDataEffects).
export function writerBusy(): Promise<boolean> {
  return invoke<boolean>("writer_busy");
}

export type AppLockState =
  | "disabled"
  | "locked"
  | "unlocked"
  | "unavailable";

export interface AppLockStatus {
  enabled: boolean;
  state: AppLockState;
  available: boolean;
  reason: string | null;
}

export function appLockStatus(): Promise<AppLockStatus> {
  return invoke<AppLockStatus>("app_lock_status");
}

export function setAppLockEnabled(enabled: boolean): Promise<AppLockStatus> {
  return invoke<AppLockStatus>("set_app_lock_enabled", { enabled });
}

export function unlockApp(): Promise<AppLockStatus> {
  return invoke<AppLockStatus>("unlock_app");
}

export function lockApp(): Promise<AppLockStatus> {
  return invoke<AppLockStatus>("lock_app");
}

export function noteWindowHiddenForIdle(): Promise<void> {
  return invoke<void>("rk_shell_idle_note", { hidden: true });
}

export function noteWindowVisibleForIdle(): Promise<void> {
  return invoke<void>("rk_shell_idle_note", { hidden: false });
}

/** Window caption controls through the shell (`rk_shell_window`). */
export function shellWindow(action: "minimize" | "toggle-maximize" | "close"): Promise<void> {
  return invoke<void>("rk_shell_window", { action });
}

/** Mirror the app theme into native window chrome through the shell (`rk_shell_set_theme`). */
export function shellSetTheme(theme: "light" | "dark" | "system"): Promise<void> {
  return invoke<void>("rk_shell_set_theme", { theme });
}

export interface Account {
  id: string;
  display_name: string;
  color: string;
  last_error: string | null;
  last_sync_at: number | null;
}

export function hasGoogleClientId(): Promise<boolean> {
  return invoke<boolean>("has_google_client_id");
}

export function setGoogleClientId(clientId: string): Promise<void> {
  return invoke<void>("set_google_client_id", { clientId });
}

export function hasGoogleClientSecret(): Promise<boolean> {
  return invoke<boolean>("has_google_client_secret");
}

export function setGoogleClientSecret(clientSecret: string): Promise<void> {
  return invoke<void>("set_google_client_secret", { clientSecret });
}

export function listAccounts(): Promise<Account[]> {
  return invoke<Account[]>("list_accounts");
}

export function addAccount(): Promise<Account> {
  return invoke<Account>("add_account");
}

export function removeAccount(accountId: string): Promise<void> {
  return invoke<void>("remove_account", { accountId });
}

export function reauthAccount(accountId: string): Promise<Account> {
  return invoke<Account>("reauth_account", { accountId });
}

// --- Local logs (Settings → Diagnostics) ----------------------------------
// The four log kinds the FE can preview + copy + clear. Resolved against
// `<data_dir>` on the BE side; a name outside this union is a 400.

export type LocalLogName = "diagnostics" | "telemetry" | "startup";

export function tailLog(
  name: LocalLogName,
  maxBytes: number,
): Promise<string> {
  return invoke<string>("tail_log", { name, maxBytes });
}

export function clearLog(name: LocalLogName): Promise<void> {
  return invoke<void>("clear_log", { name });
}

export function dataDirPath(): Promise<string> {
  return invoke<string>("data_dir_path");
}

export function revealInExplorer(path: string): Promise<void> {
  return invoke<void>("reveal_in_explorer", { path });
}

export interface CompactionStatus {
  eligible: boolean;
  free_bytes: number;
  file_bytes: number;
  /** Ready-to-show sentence when not eligible; empty when eligible. */
  reason: string;
}

export interface CompactionResult {
  before_bytes: number;
  after_bytes: number;
  seconds: number;
  rowids_renumbered: boolean;
  rebuild_registered: boolean;
}

export function compactionStatus(): Promise<CompactionStatus> {
  return invoke<CompactionStatus>("compaction_status");
}

/** Rejects with a ready-to-show error string. Can take minutes on a large mailbox. */
export function compactDatabase(): Promise<CompactionResult> {
  return invoke<CompactionResult>("compact_database");
}
