// Reader thread-action handlers (mute/snooze/star/label/download/translate/etc).
// Extracted from App (M5) — reads its state + store actions via the store so
// ReaderView drops the prop bag. Handlers are verbatim from App.
import { useAppStore } from "../store/index.js";
import { showDialog } from "./dialog.js";
import { htmlToText, sanitizeHtml } from "../lib/html.js";
import {
  muteThread,
  unmuteThread,
  listTaskLists,
  createTaskIn,
  createTask,
  blockSender,
  filterSkipInbox,
  getRawMessage,
  downloadAttachment,
  saveAttachmentToDrive,
  downloadEml,
  lookupContact,
  aiRun,
  translateText,
  findDraftId,
  relabel,
  snoozeThread,
  unsnoozeThread,
  type ThreadMessage,
  type AttachmentMeta,
} from "../lib/ipc.js";

export function useThreadView() {
  const activeAccount = useAppStore((s) => s.activeAccount);
  const loadAliases = useAppStore((s) => s.loadAliases);
  const loadThreads = useAppStore((s) => s.loadThreads);
  const openLabels = useAppStore((s) => s.openLabels);
  const openThread = useAppStore((s) => s.openThread);
  const saveAndScan = useAppStore((s) => s.saveAndScan);
  const senderCard = useAppStore((s) => s.senderCard);
  const setAiBusy = useAppStore((s) => s.setAiBusy);
  const setAiSummary = useAppStore((s) => s.setAiSummary);
  const setCompose = useAppStore((s) => s.setCompose);
  const setDetailMsgs = useAppStore((s) => s.setDetailMsgs);
  const setErr = useAppStore((s) => s.setErr);
  const setEventSeed = useAppStore((s) => s.setEventSeed);
  const setExpandedMsgs = useAppStore((s) => s.setExpandedMsgs);
  const setMoveFilter = useAppStore((s) => s.setMoveFilter);
  const setMovePickerOpen = useAppStore((s) => s.setMovePickerOpen);
  const setOpenLabels = useAppStore((s) => s.setOpenLabels);
  const setOpenThread = useAppStore((s) => s.setOpenThread);
  const setRawView = useAppStore((s) => s.setRawView);
  const setReloadKey = useAppStore((s) => s.setReloadKey);
  const setSenderCard = useAppStore((s) => s.setSenderCard);
  const setTranslated = useAppStore((s) => s.setTranslated);
  const setView = useAppStore((s) => s.setView);
  const showUndo = useAppStore((s) => s.showUndo);
  const taskLists = useAppStore((s) => s.taskLists);
  const threadMsgs = useAppStore((s) => s.threadMsgs);
  const translateProvider = useAppStore((s) => s.translateProvider);

  /** First Google Meet link in a string, or null. */
  function toggleExpand(i: number) {
    setExpandedMsgs((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  }

  function toggleDetails(i: number) {
    setDetailMsgs((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  }

  /** Mute the open thread (archive + local MUTED marker), or unmute. */
  function muteOpen() {
    if (!openThread) return;
    muteThread(openThread.account_id, openThread.thread_id)
      .then(() => {
        setOpenLabels((prev) => [...prev.filter((l) => l !== "MUTED"), "MUTED"]);
        showUndo("Conversation muted", [], "");
        setOpenThread(null);
        loadThreads();
      })
      .catch((e) => setErr(String(e)));
  }

  function unmuteOpen() {
    if (!openThread) return;
    unmuteThread(openThread.account_id, openThread.thread_id)
      .then(() => {
        setOpenLabels((prev) => prev.filter((l) => l !== "MUTED"));
        showUndo("Conversation unmuted", [], "");
      })
      .catch((e) => setErr(String(e)));
  }

  /** Create a Google Task from the open message (title = subject, notes = a
   * backlink hint + snippet). Uses the account's default (first) task list. */
  async function addOpenToTasks(m: ThreadMessage) {
    if (!openThread) return;
    try {
      const lists = taskLists.length ? taskLists : await listTaskLists(openThread.account_id);
      const listId = lists[0]?.id;
      if (!listId) {
        setErr("No Google Tasks list found for this account.");
        return;
      }
      const notes = `From: ${m.from_name} <${m.from_addr}>\n${m.body
        .replace(/<[^>]+>/g, " ")
        .replace(/\s+/g, " ")
        .trim()
        .slice(0, 300)}`;
      await createTaskIn(openThread.account_id, listId, openThread.subject || "(no subject)", notes, "");
      showUndo("Added to Tasks", [], "");
    } catch (e) {
      setErr(String(e));
    }
  }

  /**
   * Create a Google Task from the open thread: title = subject, notes carry the
   * sender, a snippet, a Gmail backlink and the source ids so the task points
   * back at the conversation. Created in the thread's account default list.
   */
  function addThreadToTask() {
    if (!openThread) return;
    const acct = openThread.account_id;
    const first = threadMsgs[0];
    const from = first?.from_name || first?.from_addr || "";
    const snippet = first ? htmlToText(first.body).trim().slice(0, 200) : "";
    const link = `https://mail.google.com/mail/?authuser=${encodeURIComponent(
      acct
    )}#all/${openThread.thread_id}`;
    const notes = [
      from && `From: ${from}`,
      snippet,
      `Open in Gmail: ${link}`,
      `[mailright thread:${openThread.thread_id}${first ? ` msg:${first.message_id}` : ""}]`,
    ]
      .filter(Boolean)
      .join("\n");
    const title = (openThread.subject || "(no subject)").slice(0, 1000);
    createTask(acct, title, notes)
      .then(() => {
        // TasksView reloads its data on open, so no live refresh needed here.
        showUndo("Added to Tasks", [], "");
      })
      .catch((e) => setErr(String(e)));
  }

  /** Block the sender of a message (creates a Gmail filter → trash). */
  async function blockSenderOf(addr: string) {
    if (!openThread || !addr) return;
    if (
      (await showDialog({
        kind: "confirm",
        message: `Block ${addr}? Future mail from them will be sent to Trash.`,
      })) !== "ok"
    )
      return;
    blockSender(openThread.account_id, addr).catch((e) => setErr(String(e)));
  }

  /** "Filter like this": archive future mail from a sender (skip Inbox). */
  function filterLikeThis(addr: string) {
    if (!openThread || !addr) return;
    filterSkipInbox(openThread.account_id, addr)
      .then(() => showUndo(`Future mail from ${addr} will skip the Inbox`, [], ""))
      .catch((e) => setErr(String(e)));
  }

  /** Download an attachment: warn on risky types, then best-effort AV-scan it. */
  function downloadAtt(messageId: string, a: AttachmentMeta) {
    if (!openThread) return;
    saveAndScan(a.filename, () =>
      downloadAttachment(openThread.account_id, messageId, a.attachment_id, a.filename)
    );
  }

  /** Save an attachment directly into Drive root. */
  function saveAttToDrive(messageId: string, a: AttachmentMeta) {
    if (!openThread) return;
    saveAttachmentToDrive(
      openThread.account_id,
      messageId,
      a.attachment_id,
      a.filename,
      a.mime_type,
    )
      .then(() => showUndo(`Saved ${a.filename} to Drive`, [], ""))
      .catch((e) => setErr(String(e)));
  }

  /** Fetch + show the full RFC 822 source of a message ("Show original"). */
  function showOriginal(m: ThreadMessage) {
    if (!openThread) return;
    getRawMessage(openThread.account_id, m.message_id)
      .then(setRawView)
      .catch((e) => setErr(String(e)));
  }

  /** Download a message as a .eml file to the Downloads folder. */
  function downloadEmlOf(m: ThreadMessage) {
    if (!openThread) return;
    const name = (m.subject || openThread.subject || "message").slice(0, 80);
    saveAndScan(`${name}.eml`, () => downloadEml(openThread.account_id, m.message_id, name));
  }

  /** Toggle a label on the open thread (Label as / un-label). */
  function toggleThreadLabel(labelId: string) {
    if (!openThread) return;
    const refs = [{ account_id: openThread.account_id, thread_id: openThread.thread_id }];
    const has = openLabels.includes(labelId);
    setOpenLabels((prev) => (has ? prev.filter((l) => l !== labelId) : [...prev, labelId]));
    relabel(refs, has ? [] : [labelId], has ? [labelId] : []).catch((e) => setErr(String(e)));
  }

  /** Move the open thread to a label and out of the Inbox (apply label + drop INBOX). */
  function moveToLabel(labelId: string) {
    if (!openThread) return;
    const refs = [{ account_id: openThread.account_id, thread_id: openThread.thread_id }];
    setMovePickerOpen(false);
    setMoveFilter("");
    relabel(refs, [labelId], ["INBOX"])
      .then(() => {
        showUndo("Moved", refs, "unarchive");
        setOpenThread(null);
        loadThreads();
      })
      .catch((e) => setErr(String(e)));
  }

  /** Toggle the IMPORTANT marker on the open thread. */
  function toggleImportant() {
    if (!openThread) return;
    const refs = [{ account_id: openThread.account_id, thread_id: openThread.thread_id }];
    const has = openLabels.includes("IMPORTANT");
    setOpenLabels((prev) => (has ? prev.filter((l) => l !== "IMPORTANT") : [...prev, "IMPORTANT"]));
    relabel(refs, has ? [] : ["IMPORTANT"], has ? ["IMPORTANT"] : []).catch((e) =>
      setErr(String(e))
    );
  }

  /** Toggle a People-API contact card for a sender address. Best-effort. */
  function openSenderCard(email: string) {
    const accountId = openThread?.account_id || activeAccount;
    if (!accountId || !email) return;
    if (senderCard?.email === email) {
      setSenderCard(null);
      return;
    }
    setSenderCard({ email, card: null, loading: true });
    lookupContact(accountId, email)
      .then((card) => setSenderCard({ email, card, loading: false }))
      .catch(() => setSenderCard({ email, card: null, loading: false }));
  }

  function clearTranslation(id: string) {
    setTranslated((prev) => {
      const n = { ...prev };
      delete n[id];
      return n;
    });
  }

  /** Summarize the open thread (AI) into a banner. */
  function summarizeThread() {
    if (!threadMsgs.length) return;
    const text = threadMsgs
      .map((m) => `${m.from_name}: ${m.is_html ? htmlToText(m.body) : m.body}`)
      .join("\n\n---\n\n");
    setAiBusy(true);
    aiRun("summarize_thread", text)
      .then(setAiSummary)
      .catch((e) => setErr(String(e)))
      .finally(() => setAiBusy(false));
  }

  /** Translate one message into the app/system language via the chosen provider. */
  function translateMessage(m: ThreadMessage) {
    const text = m.is_html ? htmlToText(m.body) : m.body;
    if (!text.trim()) return;
    const target = (navigator.language || "en").split("-")[0];
    translateText(translateProvider, text, target)
      .then((r) =>
        setTranslated((prev) => ({
          ...prev,
          [m.message_id]: { text: r.translated_text, source: r.detected_source },
        }))
      )
      .catch((e) => setErr(String(e)));
  }

  /** Snooze the open thread until `untilMs`, then return to the list. */
  function snoozeOpen(untilMs: number) {
    if (!openThread) return;
    snoozeThread(openThread.account_id, openThread.thread_id, untilMs)
      .then(() => {
        setOpenThread(null);
        showUndo("Snoozed", [], "");
        setReloadKey((k) => k + 1);
      })
      .catch((e) => setErr(String(e)));
  }

  /** Wake the open snoozed thread now. */
  function unsnoozeOpen() {
    if (!openThread) return;
    unsnoozeThread(openThread.account_id, openThread.thread_id)
      .then(() => {
        setOpenThread(null);
        setReloadKey((k) => k + 1);
      })
      .catch((e) => setErr(String(e)));
  }

  /** Jump to the calendar with the new-event form prefilled from a message. */
  function createEventFromMsg() {
    if (!openThread) return;
    setEventSeed({ summary: openThread.subject || "", withMeet: false });
    setView("calendar");
  }

  /** Open an existing synced draft for editing — reuses its Gmail draft id so
   *  saving updates the same draft instead of creating a new one. */
  function openDraftForEdit(m: ThreadMessage) {
    if (!openThread) return;
    loadAliases(openThread.account_id);
    const seedHtml = m.is_html ? sanitizeHtml(m.body) : undefined;
    findDraftId(openThread.account_id, m.message_id)
      .then((id) => {
        setCompose({
          accountId: openThread.account_id,
          from: openThread.account_id,
          to: m.to_addrs || "",
          cc: m.cc_addrs || "",
          bcc: "",
          subject: openThread.subject,
          body: m.is_html ? "" : m.body,
          bodyHtml: seedHtml,
          threadId: openThread.thread_id,
          inReplyTo: null,
          showCc: !!(m.cc_addrs && m.cc_addrs.trim()),
          nonce: Date.now(),
          attachments: [],
          gmailDraftId: id,
        });
      })
      .catch((e) => setErr(String(e)));
  }


  return { toggleExpand, toggleDetails, muteOpen, unmuteOpen, addOpenToTasks, addThreadToTask, blockSenderOf, filterLikeThis, downloadAtt, saveAttToDrive, showOriginal, downloadEmlOf, toggleThreadLabel, moveToLabel, toggleImportant, openSenderCard, clearTranslation, summarizeThread, translateMessage, snoozeOpen, unsnoozeOpen, createEventFromMsg, openDraftForEdit };
}
