// Pure logic for the agent approval card: the queue of pending approvals, the
// countdown, and the wording of a decision. No React, no IPC.

import type { ApprovalDecision, ApprovalKind, PendingApproval } from "@rightkit/mail-ui/lib/ipc/agentApprovals";
import { hostAppName } from "../../appName";

export const APPROVAL_TIMEOUT_MS = 15 * 60 * 1000;

export interface QueueState {
  /** Oldest first. The card shows items[0]. */
  items: PendingApproval[];
}

export type QueueAction =
  | { type: "replace"; items: PendingApproval[] }
  | { type: "decided"; id: string }
  | { type: "expire"; now: number };

export const emptyQueue: QueueState = { items: [] };

function sorted(items: PendingApproval[]): PendingApproval[] {
  const seen = new Set<string>();
  const unique = items.filter((a) => {
    if (seen.has(a.id)) return false;
    seen.add(a.id);
    return true;
  });
  return unique.sort(
    (a, b) => a.created_at - b.created_at || a.id.localeCompare(b.id),
  );
}

/** `replace` is the authoritative list from the app; the others only remove. */
export function queueReducer(state: QueueState, action: QueueAction): QueueState {
  switch (action.type) {
    case "replace":
      return { items: sorted(action.items) };
    case "decided":
      return { items: state.items.filter((a) => a.id !== action.id) };
    case "expire":
      return {
        items: state.items.filter((a) => !isExpired(a.expires_at, action.now)),
      };
  }
}

/** The app treats the exact expiry moment as expired. */
export function isExpired(expiresAt: number, now: number): boolean {
  return now >= expiresAt;
}

/** Whole seconds left, rounded up, never below zero. */
export function secondsLeft(expiresAt: number, now: number): number {
  return Math.max(0, Math.ceil((expiresAt - now) / 1000));
}

export function formatCountdown(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, "0")}`;
}

/** "1 of 3" when several are waiting; empty for a single one. */
export function queuePosition(count: number): string {
  return count > 1 ? `1 of ${count}` : "";
}

/** Heading of the card, and the text of its Approve button. Distinct per kind. */
export function kindHeading(kind: ApprovalKind): string {
  switch (kind) {
    case "forward":
      return "Forward";
    case "delete":
      return "Move to Trash";
    case "attach":
      return "Attach file";
    default:
      return "Send";
  }
}

/** The Approve button names the action it carries out. */
export function approveLabel(kind: ApprovalKind): string {
  return kindHeading(kind);
}

/** What the agent is asking, after its name: "wants to forward a message". */
export function requestSentence(kind: ApprovalKind): string {
  switch (kind) {
    case "forward":
      return "wants to forward a message";
    case "delete":
      return "wants to move a conversation to Trash";
    case "attach":
      return "wants to attach a file from this computer";
    default:
      return "wants to send a message";
  }
}

/** "Nothing is forwarded unless you approve." */
export function nothingUnlessApproved(kind: ApprovalKind): string {
  switch (kind) {
    case "forward":
      return "Nothing is forwarded unless you approve.";
    case "delete":
      return "Nothing is moved to Trash unless you approve.";
    case "attach":
      return "The file is not read unless you approve.";
    default:
      return "Nothing is sent unless you approve.";
  }
}

export const FORWARD_NOTICE =
  "This forwards an existing message, including its attachments, to the recipients above.";
export const ATTACH_NOTICE =
  "This copies the file into the agent's folder so it can attach it to a draft. The agent can then read its contents.";
export const DELETE_NOTICE =
  "This moves the conversation to Trash. You can restore it from Trash for 30 days.";

/** The distinct line a kind shows on its card. Null: none (a plain send). */
export function kindNotice(kind: ApprovalKind): string | null {
  if (kind === "forward") return FORWARD_NOTICE;
  if (kind === "delete") return DELETE_NOTICE;
  if (kind === "attach") return ATTACH_NOTICE;
  return null;
}

/** Spoken text for the Deny and Approve buttons. */
export function denyAriaLabel(kind: ApprovalKind, agent: string): string {
  switch (kind) {
    case "forward":
      return `Deny: do not forward this message for ${agent}`;
    case "delete":
      return `Deny: do not move this conversation to Trash for ${agent}`;
    case "attach":
      return `Deny: do not let ${agent} attach this file`;
    default:
      return `Deny: do not send this message from ${agent}`;
  }
}

export function approveAriaLabel(kind: ApprovalKind, account: string): string {
  switch (kind) {
    case "forward":
      return `Approve and forward this message from ${account}`;
    case "delete":
      return `Approve and move this conversation in ${account} to Trash`;
    case "attach":
      return `Approve and attach this file for ${account}`;
    default:
      return `Approve and send this message from ${account}`;
  }
}

/** What to tell the user after a decision. Null: nothing to say. */
export function decisionMessage(
  d: ApprovalDecision,
  kind: ApprovalKind = "send",
): string | null {
  if (kind === "delete") return deleteMessage(d);
  if (kind === "attach") return attachMessage(d);
  const done = kind === "forward" ? "forwarded" : "sent";
  const noun = kind === "forward" ? "forward" : "send";
  if (d.status === "sent") {
    return kind === "forward" ? "The message was forwarded." : "The message was sent.";
  }
  if (d.status === "approved") return `The ${noun} is still in progress.`;
  if (d.status === "denied") {
    return d.reason === "timeout"
      ? `This request expired. Nothing was ${done}.`
      : null;
  }
  switch (d.reason) {
    case "draft_changed":
      return kind === "forward"
        ? "The forward changed after you opened this request. Nothing was forwarded."
        : "The draft changed after you opened this request. Nothing was sent.";
    case "unknown_outcome":
      return `${hostAppName()} could not confirm the ${noun}. Check the Sent folder in Gmail before trying again.`;
    case "no_credentials":
      return `This account is not signed in. Nothing was ${done}.`;
    case "not_permitted":
      return `This agent is no longer allowed to ${noun}. Nothing was ${done}.`;
    default:
      return `The draft could not be read from Gmail. Nothing was ${done}.`;
  }
}

function attachMessage(d: ApprovalDecision): string | null {
  if (d.status === "attached") return "The file was copied to the agent's folder.";
  if (d.status === "approved") return "The attach is still in progress.";
  if (d.status === "denied") {
    return d.reason === "timeout" ? "This request expired. The file was not read." : null;
  }
  switch (d.reason) {
    case "file_changed":
      return "The file changed after you opened this request. It was not attached.";
    case "file_missing":
      return "The file no longer exists. It was not attached.";
    case "file_too_large":
      return "The file is too large to attach. It was not attached.";
    case "type_blocked":
      return "That file type cannot be attached. It was not attached.";
    case "not_permitted":
      return "This agent is no longer allowed to attach files. Nothing was read.";
    default:
      return "The file could not be read or copied. It was not attached.";
  }
}

function deleteMessage(d: ApprovalDecision): string | null {
  if (d.status === "trashed") return "The conversation was moved to Trash.";
  if (d.status === "approved") return "The move to Trash is still in progress.";
  if (d.status === "denied") {
    return d.reason === "timeout"
      ? "This request expired. Nothing was moved to Trash."
      : null;
  }
  switch (d.reason) {
    case "unknown_outcome":
      return `${hostAppName()} could not confirm the move to Trash. Check Trash in Gmail before trying again.`;
    case "no_credentials":
      return "This account is not signed in. Nothing was moved to Trash.";
    case "not_permitted":
      return "This agent is no longer allowed to delete. Nothing was moved to Trash.";
    case "thread_missing":
      return `The conversation is no longer in ${hostAppName()}. Nothing was moved to Trash.`;
    case "draft_changed":
      return "This conversation changed after you opened this request. Nothing was moved.";
    default:
      return "The conversation could not be moved to Trash. Nothing was moved.";
  }
}
