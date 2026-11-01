// The ⋯ per-message actions dropdown in the reader. Prop-light: reads handlers
// from useThreadView / useImageTrust and state from the store, like the rest of
// the reader. Rendered by ReaderMessage while its more-menu is open; the
// published ContextMenu owns dismissal, keyboard navigation and focus restore.
import { ContextMenu, type MenuItem } from "@rightkit/react-shell/react";
import Icon from "../components/Icons";
import { emailDomain } from "../lib/imageTrust";
import { useThreadView } from "../hooks/useThreadView";
import { useImageTrust } from "../hooks/useImageTrust";
import { useAppStore } from "../store";
import type { ThreadMessage } from "../lib/ipc";

// Snooze presets (epoch-ms factories evaluated at click time). Module-scope so
// the array isn't reallocated on every render.
const SNOOZE_PRESETS: { label: string; at: () => number }[] = [
  { label: "1 hour", at: () => Date.now() + 3600_000 },
  {
    label: "tomorrow",
    at: () => {
      const d = new Date();
      d.setDate(d.getDate() + 1);
      d.setHours(8, 0, 0, 0);
      return d.getTime();
    },
  },
  {
    label: "next week",
    at: () => {
      const d = new Date();
      d.setDate(d.getDate() + 7);
      d.setHours(8, 0, 0, 0);
      return d.getTime();
    },
  },
];

export function ReaderMoreMenu({ m, anchor }: { m: ThreadMessage; anchor: Element }) {
  const actOnOpen = useAppStore((s) => s.actOnOpen);
  const openReply = useAppStore((s) => s.openReply);
  const folder = useAppStore((s) => s.folder);
  const openLabels = useAppStore((s) => s.openLabels);
  const showPro = useAppStore((s) => s.showPro);
  const senderImageTrust = useAppStore((s) => s.senderImageTrust);
  const domainImageTrust = useAppStore((s) => s.domainImageTrust);
  const setMoreOpenIdx = useAppStore((s) => s.setMoreOpenIdx);
  const setCustomSnoozeOpen = useAppStore((s) => s.setCustomSnoozeOpen);
  const {
    blockSenderOf,
    filterLikeThis,
    showOriginal,
    downloadEmlOf,
    toggleImportant,
    summarizeThread,
    translateMessage,
    snoozeOpen,
    unsnoozeOpen,
    addOpenToTasks,
    createEventFromMsg,
  } = useThreadView();
  const { setSenderImageRule, setDomainImageRule } = useImageTrust();

  const senderKey = m.from_addr.trim().toLowerCase();
  const senderDomain = emailDomain(m.from_addr);
  const senderRule = senderImageTrust[m.from_addr] ?? senderImageTrust[senderKey];
  const domainRule = senderDomain ? domainImageTrust[senderDomain] : undefined;

  // Per-folder gating for Gmail 1:1 action visibility. The full toolbar (top of
  // the reader) is already driven by getReaderActions(folder) which hides the
  // wrong actions entirely; this is the secondary ⋯ dropdown on each message,
  // which still needs the same gates because the toolbar above is per-thread
  // but the more menu is per-message.
  const isDraft = folder === "drafts";
  const isSent = folder === "sent";
  const isTrash = folder === "trash";
  const isSpam = folder === "spam";
  const isSnoozed = folder === "snoozed";
  // Show "Mark important" only where Gmail does. Drafts + Sent + Trash + Spam
  // don't surface it; Snoozed + Inbox/Starred/Important/All Mail/Custom do.
  const showImportant = !isDraft && !isSent && !isTrash && !isSpam;
  // Show "Snooze" only on actively-readable folders. Drafts/Sent/Trash/Spam
  // can't be snoozed in Gmail web.
  const showSnooze = !isDraft && !isSent && !isTrash && !isSpam;
  // Show "Mark unread" on folders where Gmail does. Trash gets it (restoring
  // shouldn't lose unread state), Spam doesn't (always starts unread), Sent
  // doesn't (you can't unread something you sent), Drafts don't.
  const showMarkUnread = !isDraft && !isSent && !isSpam;
  // "Report spam" is inbox-side only. Drafts/Sent/Trash/Spam hide it.
  const showReportSpam = !isDraft && !isSent && !isTrash && !isSpam;
  // "Block sender" / "Filter like this" — only on inbound personal mail.
  const showSenderRules = !isDraft && !isSent && !isTrash && !isSpam;
  // Image trust rules — Gmail scopes these per-sender; safe on most folders,
  // including Drafts (the address is still a real one) but skip on Sent
  // (you don't trust yourself) and Spam (don't train on spam senders).
  const showImageRules = !isSent && !isSpam;

  const ic = (name: string) => <Icon name={name} size={15} />;
  const items: MenuItem[] = [];
  const add = (id: string, icon: string, label: string, run: () => void) =>
    items.push({ id, icon: ic(icon), label, run });

  add("reply-all", "replyAll", "Reply all", () => openReply(m, true));
  if (showMarkUnread) add("unread", "mailClosed", "Mark unread", () => actOnOpen("unread"));
  if (showReportSpam) add("spam", "spam", "Report spam", () => actOnOpen("spam"));
  if (showSenderRules) {
    add("block", "spam", "Block sender", () => blockSenderOf(m.from_addr));
    add("filter", "archive", "Filter like this (skip Inbox)", () => filterLikeThis(m.from_addr));
  }
  if (showImageRules) {
    add("sender-show", "folder", senderRule === "show" ? "Stop always showing images" : "Always show images from sender", () =>
      setSenderImageRule(m.from_addr, senderRule === "show" ? null : "show"));
    add("sender-block", "spam", senderRule === "block" ? "Stop blocking sender images" : "Always block images from sender", () =>
      setSenderImageRule(m.from_addr, senderRule === "block" ? null : "block"));
    if (senderDomain) {
      add("domain-show", "folder", domainRule === "show" ? `Stop showing images from ${senderDomain}` : `Always show images from ${senderDomain}`, () =>
        setDomainImageRule(m.from_addr, domainRule === "show" ? null : "show"));
      add("domain-block", "spam", domainRule === "block" ? `Stop blocking images from ${senderDomain}` : `Always block images from ${senderDomain}`, () =>
        setDomainImageRule(m.from_addr, domainRule === "block" ? null : "block"));
    }
  }
  if (showPro) {
    add("translate", "draft", "Translate message", () => translateMessage(m));
    add("summarize", "draft", "Summarize thread (AI)", () => summarizeThread());
  }
  add("trash", "trash", isTrash ? "Delete forever" : "Delete", () => actOnOpen("trash"));
  add("print", "print", "Print", () => window.print());
  add("original", "draft", "Show original", () => showOriginal(m));
  add("eml", "folder", "Download .eml", () => downloadEmlOf(m));
  if (showImportant) {
    add("important", "tag", openLabels.includes("IMPORTANT") ? "Mark not important" : "Mark important", () => toggleImportant());
  }
  add("tasks", "check", "Add to Tasks", () => addOpenToTasks(m));
  add("event", "calendar", "Create event", () => createEventFromMsg());
  if (showSnooze) {
    for (const p of SNOOZE_PRESETS) add(`snooze-${p.label}`, "clock", `Snooze · ${p.label}`, () => snoozeOpen(p.at()));
    add("snooze-custom", "clock", "Snooze · Pick date/time…", () => setCustomSnoozeOpen(true));
  }
  if (isSnoozed) add("unsnooze", "clock", "Unsnooze", () => unsnoozeOpen());

  return (
    // Portal events bubble through the React tree; keep them off .msg-head (which collapses the thread).
    <span style={{ display: "contents" }} onClick={(e) => e.stopPropagation()}>
      <ContextMenu
        anchor={anchor}
        placement="bottom-end"
        items={items}
        onClose={() => setMoreOpenIdx(null)}
        label="Message actions"
        width={260}
      />
    </span>
  );
}
