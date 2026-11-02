// Browser-QA mock for the agent approval card: one pending request of each kind
// (send, forward, delete) once `qa_seed_approvals` is called. Nothing real is
// sent, forwarded or trashed; decisions only update this list.

import type {
  ApprovalCard,
  ApprovalDecision,
  ApprovalKind,
  PendingApproval,
} from "./agentApprovals.js";
import type { InvokeArgs } from "./qaMockTypes.js";

const TIMEOUT_MS = 15 * 60 * 1000;
const ACCOUNT = "qa@example.com";

let pending: PendingApproval[] = [];

function seed(): void {
  const now = Date.now();
  const kinds: ApprovalKind[] = ["send", "forward", "delete"];
  pending = kinds.map((kind, i) => ({
    id: `qa-${kind}`,
    kind,
    agent_name: "QA agent",
    account: ACCOUNT,
    created_at: now + i,
    expires_at: now + TIMEOUT_MS,
  }));
}

function card(id: string): ApprovalCard {
  const item = pending.find((p) => p.id === id);
  if (!item) throw new Error("This request is no longer pending.");
  const base: ApprovalCard = {
    id,
    kind: item.kind,
    thread: null,
    agent_name: item.agent_name,
    account: item.account,
    expires_at: item.expires_at,
    reply_to: "",
    to: [],
    cc: [],
    bcc: [],
    subject: "",
    body: "",
    html_only: false,
    html_differs: false,
    html_text: "",
    attachments: [],
    attach: null,
    content_hash: "",
    edited_since_created: false,
  };
  if (item.kind === "delete") {
    return {
      ...base,
      thread: {
        subject: "Quarterly report",
        sender: "Pat Example <pat@example.com>",
        date: Date.now() - 3600_000,
        message_count: 3,
      },
    };
  }
  const forwarded = item.kind === "forward";
  return {
    ...base,
    to: ["friend@example.com"],
    bcc: ["archive@example.com"],
    subject: forwarded ? "Fwd: Quarterly report" : "Lunch on Friday",
    body: forwarded
      ? "Please see below.\n\n---------- Forwarded message ---------\nFrom: Pat Example <pat@example.com>\nSubject: Quarterly report\n\nNumbers attached."
      : "Are you free for lunch on Friday?",
    attachments: forwarded
      ? [{ name: "report.pdf", mime_type: "application/pdf", size: 48213 }]
      : [],
    content_hash: `qa-hash-${item.kind}`,
  };
}

function decide(id: string, approve: boolean): ApprovalDecision {
  const item = pending.find((p) => p.id === id);
  pending = pending.filter((p) => p.id !== id);
  if (!approve) return { status: "denied", reason: "user" };
  return { status: item?.kind === "delete" ? "trashed" : "sent", reason: null };
}

export function qaApprovalsInvoke(
  command: string,
  args?: InvokeArgs,
): { handled: boolean; value: unknown } {
  switch (command) {
    case "qa_seed_approvals":
      seed();
      return { handled: true, value: undefined };
    case "agent_approvals_pending":
      return { handled: true, value: pending };
    case "agent_approval_card":
      return { handled: true, value: card(String(args?.id ?? "")) };
    case "agent_approval_approve":
      return { handled: true, value: decide(String(args?.id ?? ""), true) };
    case "agent_approval_deny":
      return { handled: true, value: decide(String(args?.id ?? ""), false) };
    default:
      return { handled: false, value: undefined };
  }
}
