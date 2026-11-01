import { invoke } from "./invoke.js";
import type { ThreadRef } from "./mail.js";

export interface OutAttachment {
  filename: string;
  mimeType: string;
  dataB64: string;
}

export interface SendReq {
  accountId: string;
  from: string;
  to: string;
  cc: string;
  bcc: string;
  subject: string;
  body: string;
  isHtml: boolean;
  threadId: string | null;
  inReplyTo: string | null;
  attachments?: OutAttachment[];
}

/** Send a message (explicit user action — outbox-tracked, guarded). */
export function sendEmail(req: SendReq): Promise<void> {
  return invoke<void>("send_email", { req });
}

// ---- Backlog: attachments / drafts / relabel / signature / block / workspace write ----
/** Save the compose as a Gmail draft (create, or update with a draft id). */
export function saveDraft(
  req: SendReq,
  gmailDraftId: string | null,
): Promise<string> {
  return invoke<string>("save_draft", { req, gmailDraftId });
}

/** The Gmail draft id for a synced DRAFT message (for Edit draft / PUT-by-id). */
export function findDraftId(
  accountId: string,
  messageId: string,
): Promise<string | null> {
  return invoke<string | null>("find_draft_id", { accountId, messageId });
}

/** Snooze a thread until untilMs (epoch ms) — hidden from all views but Snoozed. */
export function snoozeThread(
  accountId: string,
  threadId: string,
  untilMs: number,
): Promise<void> {
  return invoke<void>("snooze_thread", { accountId, threadId, untilMs });
}

/** Wake a snoozed thread now. */
export function unsnoozeThread(
  accountId: string,
  threadId: string,
): Promise<void> {
  return invoke<void>("unsnooze_thread", { accountId, threadId });
}

/** Schedule a message to send at sendAt (epoch ms); fires while the app runs. */
export function scheduleSend(req: SendReq, sendAt: number): Promise<void> {
  return invoke<void>("schedule_send", { req, sendAt });
}

/** Move-to / Label-as: add/remove arbitrary labels on threads. */
export function relabel(
  refs: ThreadRef[],
  addLabels: string[],
  removeLabels: string[],
): Promise<void> {
  return invoke<void>("relabel", { refs, addLabels, removeLabels });
}

export function getSignature(accountId: string): Promise<string> {
  return invoke<string>("get_signature", { accountId });
}
export function setSignature(accountId: string, text: string): Promise<void> {
  return invoke<void>("set_signature", { accountId, text });
}

/** Block a sender (creates a Gmail filter that trashes future mail from them). */
export function blockSender(
  accountId: string,
  fromAddr: string,
): Promise<void> {
  return invoke<void>("block_sender", { accountId, fromAddr });
}

export interface FilterInfo {
  id: string;
  from: string;
  to: string;
  subject: string;
  add_labels: string[];
  remove_labels: string[];
}

/** The account's Gmail filters (for the Settings list). */
export function listFilters(accountId: string): Promise<FilterInfo[]> {
  return invoke<FilterInfo[]>("list_filters", { accountId });
}

/** Delete a Gmail filter by id. */
export function deleteFilter(
  accountId: string,
  filterId: string,
): Promise<void> {
  return invoke<void>("delete_filter", { accountId, filterId });
}

/** "Filter like this": archive future mail from a sender (skip Inbox). */
export function filterSkipInbox(
  accountId: string,
  fromAddr: string,
): Promise<void> {
  return invoke<void>("filter_skip_inbox", { accountId, fromAddr });
}

export interface LabelResult {
  id: string;
  name: string;
  label_type: string;
}

/** Create a custom Gmail label (name may use "Parent/Child" for nesting). */
export function gmailCreateLabel(
  accountId: string,
  name: string,
): Promise<LabelResult> {
  return invoke<LabelResult>("gmail_create_label", { accountId, name });
}

/** Rename a custom Gmail label. */
export function gmailRenameLabel(
  accountId: string,
  labelId: string,
  newName: string,
): Promise<void> {
  return invoke<void>("gmail_rename_label", { accountId, labelId, newName });
}

/** Delete a custom Gmail label (the label only — messages are untouched). */
export function gmailDeleteLabel(
  accountId: string,
  labelId: string,
): Promise<void> {
  return invoke<void>("gmail_delete_label", { accountId, labelId });
}

/** Set a label's color (Gmail palette hex pair). */
export function gmailSetLabelColor(
  accountId: string,
  labelId: string,
  background: string,
  text: string,
): Promise<void> {
  return invoke<void>("gmail_set_label_color", {
    accountId,
    labelId,
    background,
    text,
  });
}

/** Gmail vacation-responder settings (keys match Gmail's JSON exactly). */
export interface Vacation {
  enableAutoReply: boolean;
  responseSubject: string;
  responseBodyPlainText: string;
  responseBodyHtml: string;
  restrictToContacts: boolean;
  restrictToDomain: boolean;
  startTime?: string | null;
  endTime?: string | null;
}

export function getVacation(accountId: string): Promise<Vacation> {
  return invoke<Vacation>("get_vacation", { accountId });
}

/** Update the vacation responder (explicit Save — changes a live auto-reply). */
export function updateVacation(
  accountId: string,
  vacation: Vacation,
): Promise<void> {
  return invoke<void>("update_vacation", { accountId, vacation });
}

/** The account's verified forwarding addresses (read-only). */
export function listForwardingAddresses(accountId: string): Promise<string[]> {
  return invoke<string[]>("list_forwarding_addresses", { accountId });
}

// ── Outbox view ─────────────────────────────────────────────────────────────
export interface OutboxRow {
  id: string;
  account_id: string;
  status: string; // queued | scheduled | failed
  send_at: number | null;
  last_error: string | null;
  created_at: number;
}
/** Scheduled/failed outbox rows (the Outbox view). */
export function listOutbox(): Promise<OutboxRow[]> {
  return invoke<OutboxRow[]>("list_outbox");
}
/** Cancel a scheduled/failed outbox row before it sends. */
export function cancelOutbox(id: string): Promise<void> {
  return invoke<void>("cancel_outbox", { id });
}

// ── Filter builder ──────────────────────────────────────────────────────────
/** Create a Gmail filter from full criteria + label actions (Settings builder). */
export function createFilterFull(
  accountId: string,
  from: string,
  to: string,
  subject: string,
  query: string,
  hasAttachment: boolean,
  addLabels: string[],
  removeLabels: string[],
): Promise<void> {
  return invoke<void>("create_filter_full", {
    accountId,
    from,
    to,
    subject,
    query,
    hasAttachment,
    addLabels,
    removeLabels,
  });
}
