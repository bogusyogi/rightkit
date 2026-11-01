// Pure mail-domain constants + formatting helpers extracted from App.tsx.
// No React, no app state, no IPC — safe to import anywhere and unit-test.

/** Per-account unread count lookup. Used by the titlebar account switcher,
 *  Settings → Accounts, and the contextual sidebar (which uses the active
 *  account's id as the lookup key). Pulled into one place so the four call
 *  sites can never drift on the "missing account → 0" fallback. */
export function unreadFor(
  counts: { per_account: { account_id: string; count: number }[] },
  accountId: string,
): number {
  return counts.per_account.find((p) => p.account_id === accountId)?.count ?? 0;
}

export type Folder =
  | "all"
  | "inbox"
  | "starred"
  | "snoozed"
  | "sent"
  | "drafts"
  | "allmail"
  | "spam"
  | "trash";

export const FOLDERS: { key: Folder; name: string }[] = [
  { key: "inbox", name: "Inbox" },
  { key: "starred", name: "Starred" },
  { key: "snoozed", name: "Snoozed" },
  { key: "sent", name: "Sent" },
  { key: "drafts", name: "Drafts" },
  { key: "allmail", name: "All Mail" },
  { key: "spam", name: "Spam" },
  { key: "trash", name: "Trash" },
];

// RRULE BYDAY codes (RFC5545) starting Monday, with display labels.
export const WEEKDAYS: ReadonlyArray<readonly [string, string]> = [
  ["MO", "Mon"],
  ["TU", "Tue"],
  ["WE", "Wed"],
  ["TH", "Thu"],
  ["FR", "Fri"],
  ["SA", "Sat"],
  ["SU", "Sun"],
];

/** Maps a folder to the Gmail label the backend filters on. ALL_MAIL is a
 * sentinel (matched in db::list_threads), not a real Gmail label. */
export const FOLDER_LABEL: Record<Folder, string> = {
  all: "INBOX",
  inbox: "INBOX",
  starred: "STARRED",
  snoozed: "SNOOZED",
  sent: "SENT",
  drafts: "DRAFT",
  allmail: "ALL_MAIL",
  spam: "SPAM",
  trash: "TRASH",
};

/** Sidebar folder → Icon name. */
export const FOLDER_ICON: Record<Folder, string> = {
  all: "inboxes",
  inbox: "inbox",
  starred: "star",
  snoozed: "clock",
  sent: "send",
  drafts: "draft",
  allmail: "archive",
  spam: "spam",
  trash: "trash",
};

/** Inbox category tabs (Gmail). Primary = INBOX minus the other categories. */
export const CATEGORY_TABS: { key: string; name: string }[] = [
  { key: "PRIMARY", name: "Primary" },
  { key: "CATEGORY_PROMOTIONS", name: "Promotions" },
  { key: "CATEGORY_SOCIAL", name: "Social" },
  { key: "CATEGORY_UPDATES", name: "Updates" },
];
export const CATEGORY_LABEL_IDS = [
  "CATEGORY_PROMOTIONS",
  "CATEGORY_SOCIAL",
  "CATEGORY_UPDATES",
  "CATEGORY_FORUMS",
];

/** Whether the account has any non-empty Gmail category — gates the inbox
 *  category tabs + the Primary-tab default. Single source of truth so App and
 *  InboxView never diverge on the predicate. */
export function hasInboxCategories<T extends { id: string; total: number }>(labels: T[]): boolean {
  return labels.some((l) => CATEGORY_LABEL_IDS.includes(l.id) && l.total > 0);
}

export const HIDDEN_LABELS = new Set(["UNREAD", "IMPORTANT", "CHAT", "SENT", "DRAFT"]);
export const LABEL_NAMES: Record<string, string> = {
  INBOX: "Inbox",
  STARRED: "Starred",
  SPAM: "Spam",
  TRASH: "Trash",
  SNOOZED: "Snoozed",
};

/** Extract a usable unsubscribe URL from a List-Unsubscribe header value
 * (`<https://…>, <mailto:…>`). Prefer https, then http, then mailto. */
export function unsubUrl(header: string | null): string | null {
  if (!header) return null;
  const urls = [...header.matchAll(/<([^>]+)>/g)].map((m) => m[1].trim());
  return (
    urls.find((u) => /^https:/i.test(u)) ??
    urls.find((u) => /^http:/i.test(u)) ??
    urls.find((u) => /^mailto:/i.test(u)) ??
    null
  );
}

/** Friendly chip text for a Gmail label, or null to hide it. Custom label IDs
 * (Label_NN) are hidden until frontend name resolution lands. */
export function labelChip(l: string): string | null {
  if (HIDDEN_LABELS.has(l)) return null;
  if (/^Label_\d+$/.test(l)) return null;
  if (l.startsWith("CATEGORY_")) {
    const m = l.slice("CATEGORY_".length).toLowerCase();
    return m.charAt(0).toUpperCase() + m.slice(1);
  }
  return LABEL_NAMES[l] ?? l;
}

export const initialOf = (s: string) => (s.trim()[0] || "?").toUpperCase();

const AVATAR_COLORS = [
  "#C46A3D", "#6E8B6B", "#B07A84", "#7E8AA8", "#B59A52", "#8A6BA8", "#5E8C8C", "#A8675B",
];

/** Executable / script extensions that warrant a download warning.
 *  Covers Windows (exe/msi/bat/…), macOS (dmg/pkg/app/command), and
 *  cross-platform script/archive types. */
export const RISKY_EXT =
  /\.(exe|msi|bat|cmd|com|scr|ps1|vbs|js|jar|reg|hta|cpl|msc|lnk|wsf|pif|dmg|pkg|app|command|sh|mpkg|workflow|osx|action)$/i;

/** Gmail-allowed label color pairs (Gmail rejects arbitrary hex). */
export const LABEL_COLORS: { bg: string; text: string }[] = [
  { bg: "#fb4c2f", text: "#ffffff" },
  { bg: "#ffad47", text: "#ffffff" },
  { bg: "#fad165", text: "#000000" },
  { bg: "#16a766", text: "#ffffff" },
  { bg: "#2da2bb", text: "#ffffff" },
  { bg: "#4a86e8", text: "#ffffff" },
  { bg: "#a479e2", text: "#ffffff" },
  { bg: "#f691b3", text: "#000000" },
  { bg: "#999999", text: "#ffffff" },
];

/** Stable per-sender avatar color (Gmail-style colored initials). */
export function colorFor(s: string): string {
  let h = 0;
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) >>> 0;
  return AVATAR_COLORS[h % AVATAR_COLORS.length];
}

/** The custom (user-created) labels from a label catalog. Generic so it stays
 *  free of app/IPC types. */
export function userLabelsOf<T extends { label_type: string }>(labels: T[]): T[] {
  return labels.filter((l) => l.label_type === "user");
}

/** First token of a display name ("Elizabeth Bonano" → "Elizabeth"), Gmail-style. */
function firstWord(s: string): string {
  const t = s.trim();
  const i = t.indexOf(" ");
  return i > 0 ? t.slice(0, i) : t;
}

/** The sender column for a thread row: the full sender name for a single
 *  participant, else deduped first-names (Gmail-style) for a conversation. The
 *  message count is shown separately as a badge, so this is names only. */
export function threadSenders(participants: string[], fromName: string, max = 3): string {
  if (participants.length <= 1) return fromName;
  const names = [...new Set(participants.map(firstWord).filter(Boolean))];
  return names.slice(0, max).join(", ");
}

/** Extract the first Google Meet link from text, or null. */
export function findMeetLink(text: string): string | null {
  const m = text.match(/https?:\/\/meet\.google\.com\/[a-z]{3}-[a-z]{4}-[a-z]{3}/i);
  return m ? m[0] : null;
}

/** The visible thread list for a scope: search-results or folder threads,
 *  optionally unread-only, sorted by the chosen order. Generic (no IPC types). */
/** Stable selection/identity key for a thread (account + thread id). */
export function keyOf(t: { account_id: string; thread_id: string }): string {
  return `${t.account_id}:${t.thread_id}`;
}

export function computeListSource<T extends { unread: boolean; last_message_at: number }>(
  threads: T[],
  results: T[],
  query: string,
  unreadOnly: boolean,
  sortMode: "newest" | "oldest"
): T[] {
  const base = query.trim() ? results : threads;
  const filtered = unreadOnly ? base.filter((t) => t.unread) : base;
  return [...filtered].sort((a, b) =>
    sortMode === "oldest"
      ? a.last_message_at - b.last_message_at
      : b.last_message_at - a.last_message_at
  );
}
