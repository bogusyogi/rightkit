// Per-folder action visibility + routing. Single source of truth for "which
// Gmail actions make sense in which folder" — mirrors Gmail web's toolbar so
// the host app doesn't surface buttons that silently no-op (the 2026-07-03 bug
// class: bulk_action assumed INBOX semantics and dropped a label the thread
// didn't carry).
//
// Every toolbar / reader / more-menu that renders a button reads from here.
// If Gmail ships a new action, add it here once. If the action needs a
// folder-specific IPC (drafts.delete instead of bulk_action trash), the
// `handler` field on the action object points at the right call.

import type { Folder } from "./mail.js";

/** The verbs the host app UI exposes for thread manipulation. Each maps to
 *  exactly one BE handler. `kind: "bulk"` runs through `bulk_action`;
 *  `kind: "delete_drafts"` runs through `delete_drafts` (Gmail drafts.delete
 *  + local DRAFT label drop); `kind: "untrash"` / `kind: "unspam"` /
 *  `kind: "unsnooze"` / `kind: "unarchive"` are bulk_action variants. */
export type ThreadAction =
  | "archive"
  | "unarchive"
  | "trash"
  | "untrash"
  | "spam"
  | "unspam"
  | "snooze"
  | "unsnooze"
  | "mute"
  | "unmute"
  | "mark_read"
  | "mark_unread"
  | "star"
  | "unstar"
  | "delete_drafts"
  | "block_sender"
  | "filter_like_this"
  | "translate"
  | "summarize"
  | "important";

/** Which toolbar buttons to render for the thread list at the top of the
 *  inbox view, per Gmail folder. The order here is the render order. */
export function getListToolbarActions(folder: Folder): ThreadAction[] {
  switch (folder) {
    case "drafts":
      // Gmail Drafts: only Discard. No archive, no spam, no mute — drafts
      // have no INBOX to drop and Gmail doesn't expose those verbs.
      return ["delete_drafts", "mark_unread", "snooze", "star"];
    case "sent":
      // Gmail Sent: only Delete (moves to Trash; SENT label preserved).
      // No archive, no spam, no mute.
      return ["trash", "mark_unread", "star"];
    case "trash":
      // Gmail Trash: Move to Inbox + Delete forever (disabled in MR).
      return ["untrash", "mark_unread", "star"];
    case "spam":
      // Gmail Spam: Not spam + Delete forever (disabled in MR).
      return ["unspam", "mark_unread", "star"];
    case "snoozed":
      // Gmail Snoozed: Move to Inbox only.
      return ["unsnooze", "mark_unread", "star"];
    case "allmail":
      // Gmail All Mail: no Archive (everything here is already archived),
      // no Spam. Move to Inbox is "unarchive".
      return ["unarchive", "trash", "mark_read", "mark_unread", "star"];
    case "inbox":
    case "starred":
    case "all":
    default:
      return [
        "archive",
        "spam",
        "trash",
        "mark_read",
        "mark_unread",
        "star",
        "snooze",
        "mute",
      ];
  }
}

/** Which reader-toolbar buttons to render when viewing a single open
 *  thread, per Gmail folder. */
export function getReaderActions(folder: Folder): ThreadAction[] {
  switch (folder) {
    case "drafts":
      return ["delete_drafts", "important"]; // + Edit draft (rendered separately as banner button)
    case "sent":
      return ["trash", "important"];
    case "trash":
      return ["untrash", "important"];
    case "spam":
      return ["unspam", "important"];
    case "snoozed":
      return ["unsnooze", "important"];
    case "allmail":
      return ["unarchive", "trash", "important"];
    case "inbox":
    case "starred":
    case "all":
    default:
      return ["archive", "spam", "trash", "mark_unread", "mute", "important"];
  }
}

/** Map an action to the BE handler it should invoke. Keeps the call sites
 *  declarative — `<button onClick={() => invokeAction("trash", refs)}>` reads
 *  like Gmail. */
export function invokeAction(
  action: ThreadAction,
  _refs: { account_id: string; thread_id: string }[],
  _opts?: { onDrafts?: () => void; onDraftsCheck?: (openLabels: string[]) => boolean }
): { kind: "bulk"; action: string } | { kind: "delete_drafts" } | { kind: "noop" } {
  switch (action) {
    case "delete_drafts":
      return { kind: "delete_drafts" };
    case "archive":
      return { kind: "bulk", action: "archive" };
    case "unarchive":
      return { kind: "bulk", action: "unarchive" };
    case "trash":
      return { kind: "bulk", action: "trash" };
    case "untrash":
      return { kind: "bulk", action: "untrash" };
    case "spam":
      return { kind: "bulk", action: "spam" };
    case "unspam":
      return { kind: "bulk", action: "unspam" };
    case "mute":
      return { kind: "bulk", action: "mute" };
    case "mark_read":
      return { kind: "bulk", action: "read" };
    case "mark_unread":
      return { kind: "bulk", action: "unread" };
    case "star":
      return { kind: "bulk", action: "star" };
    case "unstar":
      return { kind: "bulk", action: "unstar" };
    default:
      // read-toolbar verbs handled outside this helper: snooze, unsnooze,
      // block_sender, filter_like_this, translate, summarize, important,
      // edit, reply. They have their own paths and aren't routed through
      // `bulk_action`/`delete_drafts`.
      return { kind: "noop" };
  }
}

/** Convenience: is a thread a draft? */
export function isDraftThread(openLabels: readonly string[]): boolean {
  return openLabels.includes("DRAFT");
}