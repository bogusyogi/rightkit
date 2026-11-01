import { invoke } from "./invoke";
import { hostAppName } from "../../appName";

export interface Thread {
  account_id: string;
  account_color: string;
  account_name: string;
  thread_id: string;
  from_name: string;
  subject: string;
  snippet: string;
  unread: boolean;
  starred: boolean;
  last_message_at: number;
  labels: string[];
  has_attachments: boolean;
  attachment_names: string[];
  important: boolean;
  message_count: number;
  participants: string[];
  matching_message_id?: string | null;
  matching_message_excerpt?: string | null;
  /** L11: FTS5 snippet() output for the best-matching message, with match
   *  spans wrapped in the sentinel pair from src/lib/highlightSnippet.ts. */
  matching_message_snippet?: string | null;
  match_count?: number;
}

export function listThreads(
  accountId: string | null = null,
  label = "INBOX",
  category: string | null = null,
): Promise<Thread[]> {
  return invoke<Thread[]>("list_threads", { accountId, label, category });
}

export interface LabelRow {
  id: string;
  name: string;
  label_type: string; // "system" | "user"
  total: number;
  unread: number;
  /** Background color hex synced from Gmail, if the user set one server-side. */
  color: string | null;
}

/** The active account's full label catalog (system + custom) with counts. */
export function listLabels(accountId: string): Promise<LabelRow[]> {
  return invoke<LabelRow[]>("list_labels", { accountId });
}

/** Whether remote images load by default. */
export function getImagePolicy(): Promise<boolean> {
  return invoke<boolean>("get_image_policy");
}

export function setImagePolicy(show: boolean): Promise<void> {
  return invoke<void>("set_image_policy", { show });
}

export function clearImageCache(): Promise<number> {
  return invoke<number>("clear_image_cache");
}

/** Whether known email-tracking hosts are short-circuited in the image proxy. */
export function getBlockTrackers(): Promise<boolean> {
  return invoke<boolean>("get_block_trackers");
}

export function setBlockTrackers(enabled: boolean): Promise<void> {
  return invoke<void>("set_block_trackers", { enabled });
}

export interface UnreadCounts {
  total: number;
  per_account: { account_id: string; count: number }[];
}

export function unreadCounts(): Promise<UnreadCounts> {
  return invoke<UnreadCounts>("unread_counts");
}

export function syncAll(): Promise<Thread[]> {
  return invoke<Thread[]>("sync_all");
}

export interface RuntimeHealthService {
  name: string;
  phase: string;
  last_success_ms: number | null;
  last_error_ms: number | null;
  last_error: string | null;
  timings: Record<string, TimingStat>;
}

export interface TimingStat {
  count: number;
  last_ms: number;
  max_ms: number;
}

export interface RuntimeHealthSnapshot {
  captured_at_ms: number;
  db: { status: string; reason: string | null; db_path: string; reader_available: boolean; last_checkpoint_ms: number | null };
  sync: { status: string; reason: string | null; cadence_mode: string; current_lease_owner: string | null; lease_age_ms: number | null; last_account_error: string | null };
  outbox: { status: string; reason: string | null; pending: number; queued: number; retryable_failed: number; stuck: number };
  webview: { status: string; reason: string | null; hidden: boolean; suspended: boolean; hide_show_cycles: number };
  oauth: { status: string; reason: string | null; flow_active: boolean; started_at_ms: number | null };
  services: RuntimeHealthService[];
  timings: Record<string, TimingStat>;
}

export function runtimeHealth(): Promise<RuntimeHealthSnapshot> {
  return invoke<RuntimeHealthSnapshot>("runtime_health");
}

export interface ArchiveProgress {
  total: number;
  with_body: number;
  active: boolean;
  full_archive: boolean;
}
export function archiveProgress(accountId: string): Promise<ArchiveProgress> {
  return invoke<ArchiveProgress>("archive_progress", { accountId });
}
export function startDownloadAll(accountId: string): Promise<void> {
  return invoke<void>("start_download_all", { accountId });
}
export function cancelDownloadAll(accountId: string): Promise<void> {
  return invoke<void>("cancel_download_all", { accountId });
}

/** Pagination: pull the next page of OLDER messages (older than `beforeMs`) for
 *  a scope into the local mirror. `label` is any Gmail label id (INBOX, SENT,
 *  STARRED, TRASH, SPAM, a user Label_* id, …) or the pseudo-scope "ALL_MAIL".
 *  Returns how many messages were fetched (0 = no older mail). */
export function loadOlder(
  accountId: string,
  label: string,
  beforeMs: number,
  limit = 50,
): Promise<number> {
  return invoke<number>("load_older", { accountId, label, beforeMs, limit });
}

export interface ThreadMessage {
  message_id: string;
  from_name: string;
  from_addr: string;
  to_addrs: string;
  cc_addrs: string;
  reply_to: string | null;
  list_unsubscribe: string | null;
  internal_date: number;
  subject: string;
  body: string;
  is_html: boolean;
  unread: boolean;
  has_attachments: boolean;
  /** Parsed SPF/DKIM/DMARC verdicts from Authentication-Results, if present. */
  auth: AuthStatus | null;
}

/** SPF/DKIM/DMARC verdicts: "pass" | "fail" | "softfail" | "neutral" | "none" | "unknown". */
export interface AuthStatus {
  spf: string;
  dkim: string;
  dmarc: string;
}

export function getThread(
  accountId: string,
  threadId: string,
): Promise<ThreadMessage[]> {
  return invoke<ThreadMessage[]>("get_thread", { accountId, threadId });
}

/** Position of the last thread of the previous search page (keyset cursor). */
export interface SearchCursor {
  last_message_at: number;
  account_id: string;
  thread_id: string;
}

export function searchThreads(
  query: string,
  accountId: string | null = null,
  label: string | null = null,
  category: string | null = null,
  before: SearchCursor | null = null,
): Promise<Thread[]> {
  return invoke<Thread[]>("search_threads", {
    query,
    accountId,
    label,
    category,
    before,
  });
}

export function markThreadRead(
  accountId: string,
  threadId: string,
): Promise<void> {
  return invoke<void>("mark_thread_read", { accountId, threadId });
}

export function markThreadUnread(
  accountId: string,
  threadId: string,
): Promise<void> {
  return invoke<void>("mark_thread_unread", { accountId, threadId });
}

export function archiveThread(
  accountId: string,
  threadId: string,
): Promise<void> {
  return invoke<void>("archive_thread", { accountId, threadId });
}

export function trashThread(
  accountId: string,
  threadId: string,
): Promise<void> {
  return invoke<void>("trash_thread", { accountId, threadId });
}

export function muteThread(accountId: string, threadId: string): Promise<void> {
  return invoke<void>("mute_thread", { accountId, threadId });
}

export function unmuteThread(
  accountId: string,
  threadId: string,
): Promise<void> {
  return invoke<void>("unmute_thread", { accountId, threadId });
}

export function setAccountDisplay(
  accountId: string,
  displayName: string,
  color: string,
): Promise<void> {
  return invoke<void>("set_account_display", { accountId, displayName, color });
}

// ── Desktop shell (tray, notifications, autostart) ──
let lastBadge: number | null = null;
/**
 * Show the unread count: the Dock / taskbar badge (`rk_shell_badge`) and the tray tooltip
 * (`rk_shell_set_tray`). Repeats of the same count are skipped; a failed call is retried by the
 * next count.
 */
export async function setUnreadBadge(count: number): Promise<void> {
  if (lastBadge === count) return;
  const tooltip = count === 0 ? hostAppName() : `${hostAppName()} — ${count} unread`;
  await invoke<number>("rk_shell_badge", { count });
  await invoke<void>("rk_shell_set_tray", { update: { tooltip } });
  lastBadge = count;
}
/** Fire a native OS notification (the shell's `rk_shell_notify`; it holds the toast while Do-Not-Disturb is on). */
export async function notify(title: string, body: string): Promise<void> {
  await invoke("rk_shell_notify", { request: { title, body, soundEnabled: true } });
}
/** Append a perf-telemetry line to the diagnostics log (fire-and-forget). */
export function appendDiag(line: string): Promise<void> {
  return invoke<void>("append_diag", { line });
}
/** Enable/disable launch-at-login (the shell's `rk_shell_autostart`). */
export async function setAutostart(enabled: boolean): Promise<void> {
  await invoke("rk_shell_autostart", { enabled });
}
/** Whether launch-at-login is currently enabled. */
export async function isAutostart(): Promise<boolean> {
  return (await invoke<{ enabled: boolean }>("rk_shell_autostart", { enabled: null })).enabled;
}
/** Final full app exit after the frontend has drained undo-send timers. */
export function finishQuit(): Promise<void> {
  return invoke<void>("finish_quit");
}

export function folderCount(
  label: string,
  accountId: string | null = null,
): Promise<number> {
  return invoke<number>("folder_count", { label, accountId });
}

export interface ThreadRef {
  account_id: string;
  thread_id: string;
}

/** Apply one action to many threads at once: "archive" | "trash" | "untrash" |
 * "unarchive" | "read" | "unread" | "star" | "unstar". Selection may span accounts. */
export function bulkAction(refs: ThreadRef[], action: string): Promise<void> {
  return invoke<void>("bulk_action", { refs, action });
}

/** Permanently delete one or more drafts (Gmail drafts.delete + local DRAFT
 * label drop). Used by the Drafts folder toolbar — `bulk_action("trash")`
 * only flips labels and doesn't drop DRAFT, so drafts wouldn't leave the
 * folder. */
export function deleteDrafts(refs: ThreadRef[]): Promise<void> {
  return invoke<void>("delete_drafts", { refs });
}

/** The labels carried by a thread, for the reader's label chips. */
export function threadLabels(
  accountId: string,
  threadId: string,
): Promise<string[]> {
  return invoke<string[]>("thread_labels", { accountId, threadId });
}

/** Whether permanent-delete is enabled. Off by default. */
export function destructiveEnabled(): Promise<boolean> {
  return invoke<boolean>("destructive_enabled");
}

/** Rotate the SQLCipher database key in place and update the stored credential. */
export function rotateDbKey(): Promise<void> {
  return invoke<void>("rotate_db_key");
}
