import {
  QA_ACCOUNT,
  QA_ACCOUNT_WORK,
  QA_ACCOUNTS,
  QA_LABELS,
  QA_REMOTE_IMAGE_DATA_URL,
  QA_THREADS,
} from "./qaMockData.js";
import { getThread, labelCount, listAttachments, listThreads, searchThreads } from "./qaMockMail.js";
import type { InvokeArgs } from "./qaMockTypes.js";
import { qaApprovalsInvoke } from "./qaMockApprovals.js";
import { qaWorkspaceInvoke } from "./qaMockWorkspace.js";
import { qaSemanticInvoke } from "./qaMockSemantic.js";
import { qaLicenseInvoke } from "./qaMockLicense.js";
import { qaSettingsInvoke } from "./qaMockSettings.js";

let qaAppLock = {
  enabled: false,
  state: "disabled",
  available: true,
  reason: null,
};

// Stateful settings: a set_* followed by the matching getter returns the new
// value, as the real backend does.
const qaSettings = {
  imagePolicy: true,
  blockTrackers: true,
  autostart: true,
  clientIdSet: true,
  clientSecretSet: true,
  signatures: new Map<string, string>(),
};

function clone<T>(value: T): T {
  if (value === undefined) return value;
  return JSON.parse(JSON.stringify(value)) as T;
}

export async function qaInvoke<T = unknown>(
  command: string,
  args?: InvokeArgs,
): Promise<T> {
  const workspaceValue = qaWorkspaceInvoke(command, args);
  if (workspaceValue.handled) return clone(workspaceValue.value) as T;
  const approvalsValue = qaApprovalsInvoke(command, args);
  if (approvalsValue.handled) return clone(approvalsValue.value) as T;
  const semanticValue = qaSemanticInvoke(command, args);
  if (semanticValue.handled) return clone(semanticValue.value) as T;
  const licenseValue = qaLicenseInvoke(command, args);
  if (licenseValue.handled) return clone(licenseValue.value) as T;
  const settingsValue = qaSettingsInvoke(command, args);
  if (settingsValue.handled) return clone(settingsValue.value) as T;
  let value: unknown;
  switch (command) {
    case "db_health":
      value = {
        ok: true,
        encrypted: true,
        db_path: "qa://mailright.sqlite",
        tables: 42,
        account_count: 1,
        error: null,
      };
      break;
    case "build_info":
      value = {
        version: "0.1.0",
        channel: "dev",
        commit: "0123456789abcdef0123456789abcdef01234567",
        commit_short: "0123456",
        dirty: false,
        build_time: "2026-01-01T00:00:00Z",
        profile: "qa",
        overrides: false,
      };
      break;
    case "compaction_status":
      value = {
        eligible: false,
        free_bytes: 0,
        file_bytes: 163000000,
        reason: "Not enough reclaimable space to be worth compacting.",
      };
      break;
    case "agents_list":
    case "agent_audit_list":
      value = [];
      break;
    case "agent_label_drafts":
      value = false;
      break;
    case "agent_set_label_drafts":
      value = undefined;
      break;
    case "agent_clear_exports":
      value = 0;
      break;
    case "agent_import_file":
    case "agent_pair":
    case "agent_set_profile":
    case "agent_revoke":
      throw new Error("Agent pairing is not available in browser QA.");
    case "compact_database":
      throw new Error("Not enough reclaimable space to be worth compacting.");
    case "app_lock_status":
      value = qaAppLock;
      break;
    case "set_app_lock_enabled":
      qaAppLock = {
        enabled: Boolean(args?.enabled),
        state: args?.enabled ? "unlocked" : "disabled",
        available: true,
        reason: null,
      };
      value = qaAppLock;
      break;
    case "unlock_app":
      qaAppLock = qaAppLock.enabled
        ? { enabled: true, state: "unlocked", available: true, reason: null }
        : { enabled: false, state: "disabled", available: true, reason: null };
      value = qaAppLock;
      break;
    case "lock_app":
      qaAppLock = qaAppLock.enabled
        ? { enabled: true, state: "locked", available: true, reason: null }
        : { enabled: false, state: "disabled", available: true, reason: null };
      value = qaAppLock;
      break;
    case "has_google_client_id":
      value = qaSettings.clientIdSet;
      break;
    case "has_google_client_secret":
      value = qaSettings.clientSecretSet;
      break;
    case "get_image_policy":
      value = qaSettings.imagePolicy;
      break;
    case "get_block_trackers":
      value = qaSettings.blockTrackers;
      break;
    case "rk_shell_autostart":
      if (typeof args?.enabled === "boolean") qaSettings.autostart = args.enabled;
      value = { supported: true, enabled: qaSettings.autostart, entry: null };
      break;
    case "set_google_client_id":
      qaSettings.clientIdSet = String(args?.clientId ?? "").trim() !== "";
      value = undefined;
      break;
    case "set_google_client_secret":
      qaSettings.clientSecretSet = String(args?.clientSecret ?? "").trim() !== "";
      value = undefined;
      break;
    case "set_image_policy":
      qaSettings.imagePolicy = Boolean(args?.show);
      value = undefined;
      break;
    case "set_block_trackers":
      qaSettings.blockTrackers = Boolean(args?.enabled);
      value = undefined;
      break;
    case "set_signature":
      qaSettings.signatures.set(String(args?.accountId ?? ""), String(args?.text ?? ""));
      value = undefined;
      break;
    case "runtime_health":
      value = {
        captured_at_ms: Date.now(),
        db: { status: "ok", reason: null, db_path: "qa://mailright.sqlite", reader_available: true, last_checkpoint_ms: null },
        sync: { status: "ok", reason: null, cadence_mode: "idle", current_lease_owner: null, lease_age_ms: null, last_account_error: null },
        outbox: { status: "ok", reason: null, pending: 0, queued: 0, retryable_failed: 0, stuck: 0 },
        webview: { status: "ok", reason: null, hidden: false, suspended: false, hide_show_cycles: 0 },
        oauth: { status: "ok", reason: null, flow_active: false, started_at_ms: null },
        services: [
          { name: "sync", phase: "running", last_success_ms: Date.now(), last_error_ms: null, last_error: null, timings: {} },
          { name: "outbox", phase: "idle", last_success_ms: null, last_error_ms: null, last_error: null, timings: {} },
        ],
        timings: {},
      };
      break;
    case "rightkit_logs_collect":
      value = {
        schema_version: 1,
        generated_at_ms: Date.now(),
        events: [],
        sources: [{ name: "telemetry", entries: 0, truncated: false }],
      };
      break;
    case "rightkit_logs_write":
    case "rightkit_logs_clear":
      value = undefined;
      break;
    case "destructive_enabled":
      value = false;
      break;
    case "list_accounts":
      value = QA_ACCOUNTS;
      break;
    case "add_account":
    case "reauth_account":
      value = QA_ACCOUNT;
      break;
    case "list_labels":
      value = QA_LABELS;
      break;
    case "unread_counts":
      value = {
        total: 132,
        per_account: [
          { account_id: QA_ACCOUNT.id, count: 78 },
          { account_id: QA_ACCOUNT_WORK.id, count: 54 },
        ],
      };
      break;
    case "list_threads":
    case "sync_all":
      value = listThreads(args);
      break;
    case "search_threads":
      value = searchThreads(args);
      break;
    case "get_thread":
      value = getThread(args);
      break;
    case "folder_count":
      value = labelCount(String(args?.label ?? "INBOX"));
      break;
    case "archive_progress":
      value = { total: 36786, with_body: 36786, active: false, full_archive: true };
      break;
    case "load_older":
    case "clear_image_cache":
      value = 0;
      break;
    case "thread_labels":
      value = (QA_THREADS[0]?.labels ?? []).slice();
      break;
    case "list_aliases":
      value = [{ email: "adrdsouza@example.com", name: "Adrian D'Souza", is_default: true }];
      break;
    case "get_signature":
      value = qaSettings.signatures.get(String(args?.accountId ?? "")) ?? "Regards,<br>Adrian";
      break;
    case "find_draft_id":
      value = null;
      break;
    case "save_draft":
      value = "qa-draft-id";
      break;
    case "list_outbox":
    case "list_filters":
    case "list_forwarding_addresses":
    case "inline_image_refs":
      value = [];
      break;
    case "fetch_remote_image":
      value = { mime_type: "image/png", hash: QA_REMOTE_IMAGE_DATA_URL, blocked: false };
      break;
    case "get_raw_message":
      value = "From: qa@example.com\r\nSubject: QA raw message\r\n\r\nRaw QA message body.";
      break;
    case "save_text_download":
      value = `qa://download/${String(args?.filename ?? "export.txt")}`;
      break;
    case "download_eml":
      value = "qa://download/message.eml";
      break;
    case "list_attachments":
      value = listAttachments(args);
      break;
    case "get_vacation":
      value = {
        enableAutoReply: false,
        responseSubject: "",
        responseBodyPlainText: "",
        responseBodyHtml: "",
        restrictToContacts: false,
        restrictToDomain: false,
        startTime: null,
        endTime: null,
      };
      break;
    case "translate_text":
      value = { text: String(args?.text ?? ""), provider: String(args?.provider ?? "qa") };
      break;
    case "ai_run":
      value = "QA AI response";
      break;
    case "trust_check":
      value = {
        level: "info",
        findings: [{ level: "info", message: "QA mock" }],
      };
      break;
    case "remove_account":
    case "start_download_all":
    case "cancel_download_all":
    case "mark_thread_read":
    case "mark_thread_unread":
    case "archive_thread":
    case "trash_thread":
    case "mute_thread":
    case "unmute_thread":
    case "set_account_display":
    case "rk_shell_badge":
    case "rk_shell_set_tray":
    case "rk_shell_notify":
    case "append_diag":
    case "clear_log":
    case "reveal_in_explorer":
    case "rk_shell_idle_note":
    case "rk_shell_window":
    case "rk_shell_set_theme":
    case "finish_quit":
    case "bulk_action":
    case "rotate_db_key":
    case "open_external":
    case "send_email":
    case "snooze_thread":
    case "unsnooze_thread":
    case "schedule_send":
    case "relabel":
    case "block_sender":
    case "delete_filter":
    case "filter_skip_inbox":
    case "gmail_rename_label":
    case "gmail_delete_label":
    case "gmail_set_label_color":
    case "update_vacation":
    case "cancel_outbox":
    case "create_filter_full":
    case "update_event":
    case "delete_event":
    case "truncate_event_series":
    case "rsvp_event":
    case "create_task":
    case "rename_task_list":
    case "delete_task_list":
    case "clear_completed_tasks":
    case "create_task_in":
    case "set_task_status":
    case "delete_task":
    case "set_translate_key":
    case "set_ai_key":
    case "set_ai_config":
      value = undefined;
      break;
    case "create_task_list":
      value = { id: "qa-task-list", title: String(args?.title ?? "QA Tasks") };
      break;
    case "gmail_create_label":
      value = { id: "Label_QA", name: String(args?.name ?? "QA"), label_type: "user" };
      break;
    case "tail_log": {
      const name = String(args?.name ?? "diagnostics");
      // Mirror what the BE would write for a session with one perf stall
      // and one IPC error. Lets the QA-mode Diagnostics tab render real-
      // looking content without booting Tauri.
      value = [
        `12:00:01 PERF ui.stall ms=487 reason=longTask`,
        `12:00:02 IPC  ipc.list_threads ok=true rows=200`,
        `12:00:03 PERF ui.stall ms=519 reason=longTask`,
        `12:00:05 IPC  ipc.search_threads ok=false code=500`,
        `12:00:05 DIAG [delete_drafts] list_drafts failed for account acct_***; dropping local DRAFT only`,
      ].join("\n");
      void name;
      break;
    }
    case "data_dir_path":
      value = "qa://mailright/data";
      break;
    default:
      throw new Error(`No QA mock registered for IPC command: ${command}`);
  }
  return clone(value) as T;
}
