import type { InvokeArgs } from "./qaMockTypes";
import { QA_LABELS, QA_THREADS } from "./qaMockData";

export function listThreads(args: InvokeArgs) {
  const label = typeof args?.label === "string" ? args.label : "INBOX";
  const category = typeof args?.category === "string" ? args.category : null;
  const accountId =
    typeof args?.accountId === "string" && args.accountId ? args.accountId : null;
  let rows = QA_THREADS.filter((thread) => {
    if (label === "ALL_MAIL") return true;
    return thread.labels.includes(label);
  });
  if (accountId) rows = rows.filter((thread) => thread.account_id === accountId);
  if (category === "PRIMARY") {
    rows = rows.filter(
      (thread) =>
        !thread.labels.some((labelId) => labelId.startsWith("CATEGORY_")),
    );
  } else if (category) {
    rows = rows.filter((thread) => thread.labels.includes(category));
  }
  return rows;
}

export function searchThreads(args: InvokeArgs) {
  const tokens = String(args?.query ?? "")
    .toLowerCase()
    .split(/\s+/)
    .map((token) => token.trim())
    .filter(Boolean);
  if (tokens.length === 0) return [];

  const operators = new Set(tokens.filter((token) => token.includes(":")));
  const textTerms = tokens.filter((token) => !operators.has(token));

  const before = args?.before as
    | { last_message_at: number; account_id: string; thread_id: string }
    | null
    | undefined;
  const afterCursor = (thread: (typeof QA_THREADS)[number]) =>
    !before ||
    thread.last_message_at < before.last_message_at ||
    (thread.last_message_at === before.last_message_at &&
      `${thread.account_id}:${thread.thread_id}` >
        `${before.account_id}:${before.thread_id}`);

  const matches = QA_THREADS.filter((thread) => {
    if (operators.has("has:attachment") && !thread.has_attachments) return false;
    if (operators.has("is:unread") && !thread.unread) return false;
    if (operators.has("is:starred") && !thread.starred) return false;
    if (textTerms.length === 0) return true;

    const haystack = [
      thread.subject,
      thread.snippet,
      thread.from_name,
      thread.attachment_names.join(" "),
    ]
      .join(" ")
      .toLowerCase();
    return textTerms.every((term) => haystack.includes(term));
  });
  return matches
    .filter(afterCursor)
    .sort(
      (a, b) =>
        b.last_message_at - a.last_message_at ||
        `${a.account_id}:${a.thread_id}`.localeCompare(
          `${b.account_id}:${b.thread_id}`,
        ),
    )
    .slice(0, 200);
}

export function getThread(args: InvokeArgs) {
  const threadId = String(args?.threadId ?? args?.thread_id ?? "");
  const thread = QA_THREADS.find((row) => row.thread_id === threadId) ?? QA_THREADS[0];
  return [
    {
      message_id: `${thread.thread_id}-message`,
      from_name: thread.from_name,
      from_addr: `${thread.from_name.toLowerCase().replace(/[^a-z0-9]+/g, ".")}@example.com`,
      to_addrs: "adrdsouza@example.com",
      cc_addrs: "",
      reply_to: null,
      list_unsubscribe: null,
      internal_date: thread.last_message_at,
      subject: thread.subject,
      body:
        `<p>${thread.snippet}</p><p>This is deterministic QA mail content.</p>` +
        (thread.thread_id === "qa-receipt-1"
          ? `<img alt="QA remote image" src="https://example.com/mailright-qa.png">`
          : ""),
      is_html: true,
      unread: thread.unread,
      has_attachments: thread.has_attachments,
      auth: { spf: "pass", dkim: "pass", dmarc: "pass" },
    },
  ];
}

export function listAttachments(args: InvokeArgs) {
  const messageId = String(args?.messageId ?? args?.message_id ?? "");
  if (messageId.startsWith("qa-png-1")) {
    return [
      {
        attachment_id: "qa-weak-mime-png",
        filename: "邮件2.0.png",
        mime_type: "application/octet-stream",
        size: 68,
      },
    ];
  }
  if (!messageId.startsWith("qa-receipt-1")) return [];
  return [
    {
      attachment_id: "qa-invoice-pdf",
      filename: "Invoice-3DE6B3E5.pdf",
      mime_type: "application/pdf",
      size: 18421,
    },
    {
      attachment_id: "qa-orders-xlsx",
      filename: "orders_export_2026-06-02.xlsx",
      mime_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
      size: 1354,
    },
    {
      attachment_id: "qa-receipt-pdf-2",
      filename: "Receipt-2673-1369.pdf",
      mime_type: "application/pdf",
      size: 29892,
    },
  ];
}

export function labelCount(label: string) {
  const row = QA_LABELS.find((item) => item.id === label);
  if (row) return row.total;
  return listThreads({ label }).length;
}

