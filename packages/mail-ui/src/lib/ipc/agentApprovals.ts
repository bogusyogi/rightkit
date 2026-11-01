import { invoke } from "./invoke.js";

/** What the agent asked for. All four always wait for the user's click. */
export type ApprovalKind = "send" | "forward" | "delete" | "attach";

/** One approval waiting for the user. No message content. */
export interface PendingApproval {
  id: string;
  kind: ApprovalKind;
  agent_name: string;
  account: string;
  created_at: number;
  expires_at: number;
}

export interface ApprovalAttachment {
  name: string;
  mime_type: string;
  size: number;
}

/** The conversation a delete would move to Trash, read from the local database. */
export interface ApprovalThread {
  subject: string;
  sender: string;
  /** Epoch milliseconds of the newest message. */
  date: number;
  message_count: number;
}

/** An attach request: the exact absolute path the agent named and the file's size in bytes. */
export interface ApprovalAttach {
  path: string;
  size: number;
}

/**
 * What the card shows, read from Gmail when the card opens (a delete: the
 * thread, read locally by id; only `thread` is filled then). Plain text only.
 */
export interface ApprovalCard {
  id: string;
  kind: ApprovalKind;
  thread: ApprovalThread | null;
  agent_name: string;
  account: string;
  expires_at: number;
  reply_to: string;
  to: string[];
  cc: string[];
  bcc: string[];
  subject: string;
  body: string;
  html_only: boolean;
  /** The HTML part reads differently from the plain part; `html_text` is it as text. */
  html_differs: boolean;
  html_text: string;
  attachments: ApprovalAttachment[];
  /** Set for an attach (no draft): the path and size the Approve click binds to. */
  attach: ApprovalAttach | null;
  /** Sent back with Approve: the send is refused if the draft no longer matches. */
  content_hash: string;
  edited_since_created: boolean;
}

export interface ApprovalDecision {
  /** `trashed` is a delete that was carried out; `attached` an attach. */
  status: "sent" | "trashed" | "attached" | "denied" | "failed" | "approved";
  /** user | timeout | draft_changed | unknown_outcome | no_credentials | fetch_failed | not_permitted | thread_missing */
  reason: string | null;
}

/** Name of the Tauri event raised when an agent asks for approval. Carries an id only. */
export const AGENT_APPROVAL_EVENT = "agent-approval-pending";

export function agentApprovalsPending(): Promise<PendingApproval[]> {
  return invoke<PendingApproval[]>("agent_approvals_pending");
}

export function agentApprovalCard(id: string): Promise<ApprovalCard> {
  return invoke<ApprovalCard>("agent_approval_card", { id });
}

/** Only the Approve button calls this, with the hash the card showed ("" for a delete). */
export function agentApprovalApprove(
  id: string,
  contentHash: string,
): Promise<ApprovalDecision> {
  return invoke<ApprovalDecision>("agent_approval_approve", {
    id,
    contentHash,
  });
}

export function agentApprovalDeny(id: string): Promise<ApprovalDecision> {
  return invoke<ApprovalDecision>("agent_approval_deny", { id });
}
