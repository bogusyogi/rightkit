// The body of a "Move to Trash" approval card: the conversation an agent asked
// to move, as the local database holds it. Everything is plain text (a subject
// or sender written by a third party is never rendered as HTML, and hidden
// characters are made visible).

import type { ApprovalThread } from "@rightkit/mail-ui/lib/ipc/agentApprovals";
import { visibleText } from "@rightkit/mail-ui/lib/visibleText";

function when(ms: number): string {
  const d = new Date(ms);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleString();
}

export function AgentApprovalThread({ thread }: { thread: ApprovalThread }) {
  const date = when(thread.date);
  return (
    <div aria-label="Conversation to move to Trash">
      <div style={{ marginTop: 6, overflowWrap: "anywhere" }}>
        <strong>Subject:</strong> {visibleText(thread.subject) || "(no subject)"}
      </div>
      <div style={{ marginTop: 4, overflowWrap: "anywhere" }}>
        <strong>From:</strong> {visibleText(thread.sender) || "(unknown sender)"}
      </div>
      {date && (
        <div style={{ marginTop: 4 }}>
          <strong>Date:</strong> {date}
        </div>
      )}
      <div style={{ marginTop: 4 }}>
        <strong>Messages:</strong> {thread.message_count}
      </div>
    </div>
  );
}
