// Send/draft/schedule lifecycle, extracted from App. This machinery must outlive
// the compose drawer (the undo-send hold timer and debounced autosave fire after
// the drawer closes), so it lives here at the App level rather than in useCompose
// (which is the in-drawer form behavior). Owns the two timers as refs.
import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { useAppStore, composeEditorRef } from "../store/index.js";
import { sanitizeHtml } from "../lib/html.js";
import {
  type SendReq,
  sendEmail,
  saveDraft,
  scheduleSend,
  archiveThread,
  finishQuit,
} from "../lib/ipc.js";

export function useSend() {
  const compose = useAppStore((s) => s.compose);
  const activeAccount = useAppStore((s) => s.activeAccount);
  const aliases = useAppStore((s) => s.aliases);
  const undoDelay = useAppStore((s) => s.undoDelay);
  const pendingSend = useAppStore((s) => s.pendingSend);
  const setCompose = useAppStore((s) => s.setCompose);
  const setErr = useAppStore((s) => s.setErr);
  const setPresend = useAppStore((s) => s.setPresend);
  const setPendingSend = useAppStore((s) => s.setPendingSend);
  const setSending = useAppStore((s) => s.setSending);
  const setScheduleOpen = useAppStore((s) => s.setScheduleOpen);
  const setScheduleAt = useAppStore((s) => s.setScheduleAt);
  const setReloadKey = useAppStore((s) => s.setReloadKey);
  const showUndo = useAppStore((s) => s.showUndo);

  const pendingSendTimer = useRef<number | null>(null); // undo-send hold timer
  const autosaveRef = useRef<number | null>(null); // debounced draft autosave timer
  const pendingSendRef = useRef(pendingSend);
  const inFlightSendRef = useRef<Promise<void> | null>(null);
  const fireSendRef = useRef<(req: SendReq) => Promise<void>>(async () => {});
  const quittingRef = useRef(false);

  useEffect(() => {
    pendingSendRef.current = pendingSend;
  }, [pendingSend]);

  /** Read the live body out of the TipTap editor, DOMPurify-sanitized, for
   *  send/draft. Falls back to the plain seed if the editor isn't mounted. */
  function readComposeBody(): { body: string; isHtml: boolean } {
    const html = composeEditorRef.current?.getHTML();
    if (html == null) return { body: compose?.body ?? "", isHtml: false };
    const body = sanitizeHtml(html);
    return { body, isHtml: true };
  }

  /** Close the compose drawer, cancelling any pending autosave. */
  function closeCompose() {
    if (autosaveRef.current) {
      window.clearTimeout(autosaveRef.current);
      autosaveRef.current = null;
    }
    setScheduleOpen(false);
    setScheduleAt("");
    setCompose(null);
  }

  /** Save the compose as a Gmail draft. `close` = the manual button (closes the
   *  drawer + resyncs); otherwise it's a silent autosave that reuses the stored
   *  gmailDraftId so the draft UPDATES in place instead of duplicating. */
  async function doSaveDraft(close: boolean) {
    if (!compose) return;
    const accountId = compose.accountId || activeAccount;
    if (!accountId) return;
    const al = aliases.find((a) => a.email === compose.from);
    const fromHeader = al && al.name ? `${al.name} <${al.email}>` : compose.from;
    const { body, isHtml } = readComposeBody();
    if (!close) {
      // Skip empty autosaves; skip when attachments are present (re-uploading
      // the bytes on every typing pause is wasteful — manual save still keeps them).
      const hasText =
        compose.to.trim() || compose.subject.trim() || body.replace(/<[^>]*>/g, "").trim();
      if (!hasText || compose.attachments.length > 0) return;
    }
    const nonce = compose.nonce;
    const payload = {
      accountId,
      from: fromHeader,
      to: compose.to,
      cc: compose.cc,
      bcc: compose.bcc,
      subject: compose.subject,
      body,
      isHtml,
      threadId: compose.threadId,
      inReplyTo: compose.inReplyTo,
      attachments: compose.attachments,
    };
    const draftId = compose.gmailDraftId;
    if (close) {
      // Close the drawer immediately — don't block on the Gmail round-trip. The
      // save + resync run in the background; the draft lands in the Drafts list
      // once the resync completes.
      closeCompose();
      showUndo("Saving draft…", [], "");
      saveDraft(payload, draftId)
        .then(() => {
          showUndo("Draft saved", [], "");
          // The backend wrote the draft into the local DB, so re-querying shows
          // it in Drafts immediately — no full syncAll (which was the lag).
          setReloadKey((k) => k + 1);
        })
        .catch((e) => setErr(String(e)));
      return;
    }
    // Autosave: await + stamp the draft id back so the next save updates in place.
    try {
      const id = await saveDraft(payload, draftId);
      setCompose((c) => (c && c.nonce === nonce ? { ...c, gmailDraftId: id } : c));
    } catch {
      /* autosave failures are silent */
    }
  }

  /** Debounced autosave — fires ~2.5s after the user stops typing. */
  function scheduleAutosave() {
    if (autosaveRef.current) window.clearTimeout(autosaveRef.current);
    autosaveRef.current = window.setTimeout(() => doSaveDraft(false), 2500);
  }

  /** Build a SendReq from the current compose (null if not ready). */
  function currentSendReq(): SendReq | null {
    if (!compose) return null;
    const accountId = compose.accountId || activeAccount;
    if (!accountId) return null;
    const al = aliases.find((a) => a.email === compose.from);
    const fromHeader = al && al.name ? `${al.name} <${al.email}>` : compose.from;
    const { body, isHtml } = readComposeBody();
    return {
      accountId,
      from: fromHeader,
      to: compose.to,
      cc: compose.cc,
      bcc: compose.bcc,
      subject: compose.subject,
      body,
      isHtml,
      threadId: compose.threadId,
      inReplyTo: compose.inReplyTo,
      attachments: compose.attachments,
    };
  }

  /** Schedule the current compose to send at `whenMs`. Fires while the app runs. */
  function scheduleCompose(whenMs: number) {
    const req = currentSendReq();
    if (!req) return;
    if (!req.to.trim()) {
      setErr("Add a recipient before scheduling.");
      return;
    }
    if (whenMs <= Date.now()) {
      setErr("Pick a future time to schedule.");
      return;
    }
    closeCompose();
    scheduleSend(req, whenMs)
      .then(() => showUndo("Send scheduled", [], ""))
      .catch((e) => setErr(String(e)));
  }

  /** Gmail-style pre-send warnings: no subject, "attached" with no file, a large
   *  recipient set, or external recipients on a custom-domain account. */
  function presendWarnings(req: SendReq): string[] {
    const w: string[] = [];
    if (!req.subject.trim()) w.push("This message has no subject.");
    const bodyText = (req.isHtml ? req.body.replace(/<[^>]+>/g, " ") : req.body).toLowerCase();
    if (/\battach(ed|ment|ments|ing)?\b/.test(bodyText) && (req.attachments?.length ?? 0) === 0)
      w.push("You mention an attachment, but nothing is attached.");
    const recips = [req.to, req.cc, req.bcc]
      .join(",")
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    if (recips.length > 8) w.push(`This message goes to ${recips.length} recipients.`);
    const dom = req.accountId.split("@")[1]?.toLowerCase();
    if (dom && dom !== "gmail.com" && dom !== "googlemail.com") {
      const ext = recips.filter((r) => {
        const d = r.split("@")[1]?.toLowerCase();
        return d && d !== dom;
      });
      if (ext.length)
        w.push(`Sending outside ${dom} — ${ext.length} external recipient${ext.length > 1 ? "s" : ""}.`);
    }
    return w;
  }

  /** Validate + (if clean) dispatch a send. `archive` = Send & Archive (the reply
   *  thread is archived). Trips the pre-send confirm if any warning fires. */
  function beginSend(archive: boolean) {
    if (!compose) return;
    if (!compose.to.trim()) {
      setErr("Add a recipient before sending.");
      return;
    }
    const req = currentSendReq();
    if (!req) return;
    const draftId = compose.gmailDraftId;
    const archiveRef =
      archive && compose.threadId
        ? { account_id: compose.accountId, thread_id: compose.threadId }
        : null;
    const warnings = presendWarnings(req);
    if (warnings.length) {
      setPresend({ req, draftId, archiveRef, warnings });
      return;
    }
    dispatchSend(req, draftId, archiveRef);
  }

  function dispatchSend(
    req: SendReq,
    draftId: string | null,
    archiveRef: { account_id: string; thread_id: string } | null
  ) {
    // Archive the replied-to thread up front (independent of the send) so the
    // list updates immediately and there's no race with the post-send resync.
    if (archiveRef) archiveThread(archiveRef.account_id, archiveRef.thread_id).catch(() => {});
    closeCompose();
    // Hold the send behind an Undo toast for the configured delay (default 5s).
    setPendingSend({ req, draftId });
    if (pendingSendTimer.current) window.clearTimeout(pendingSendTimer.current);
    pendingSendTimer.current = window.setTimeout(
      () => fireSend(req),
      Math.max(0, undoDelay) * 1000
    );
  }

  function sendCompose() {
    beginSend(false);
  }

  function sendComposeArchive() {
    beginSend(true);
  }

  /** Dispatch a held send once the undo window elapses. */
  async function fireSend(req: SendReq) {
    pendingSendTimer.current = null;
    setPendingSend(null);
    setSending(true);
    const work = (async () => {
      await sendEmail(req);
      showUndo("Message sent", [], "");
      // The backend mirrors the sent message locally (fetch_metadata_batch +
      // apply_search_messages, compose_assets.rs) before send_email resolves, so
      // re-querying local data is enough — no full syncAll (which was the lag).
      setReloadKey((k) => k + 1);
    })();
    inFlightSendRef.current = work;
    try {
      await work;
    } catch (e) {
      setErr(String(e));
    } finally {
      if (inFlightSendRef.current === work) inFlightSendRef.current = null;
      setSending(false);
    }
  }

  useEffect(() => {
    fireSendRef.current = fireSend;
  });

  useEffect(() => {
    let active = true;
    // The shell's quit gate emits this instead of exiting (tray Quit, Cmd+Q, last window).
    listen("rightkit-shell://quit-requested", async () => {
      if (!active || quittingRef.current) return;
      quittingRef.current = true;
      const held = pendingSendRef.current;
      if (pendingSendTimer.current && held) {
        window.clearTimeout(pendingSendTimer.current);
        pendingSendTimer.current = null;
        await fireSendRef.current(held.req);
      } else if (inFlightSendRef.current) {
        try {
          await inFlightSendRef.current;
        } catch {
          /* send failure is already surfaced and persisted in outbox */
        }
      }
      await finishQuit();
    }).catch(() => {});
    return () => {
      active = false;
    };
  }, []);

  /** Cancel a held send and reopen the compose with its content restored. */
  function undoSend() {
    if (pendingSendTimer.current) window.clearTimeout(pendingSendTimer.current);
    pendingSendTimer.current = null;
    const ps = pendingSend;
    setPendingSend(null);
    if (!ps) return;
    const fromMatch = ps.req.from.match(/<([^>]+)>/);
    setCompose({
      accountId: ps.req.accountId,
      from: fromMatch ? fromMatch[1] : ps.req.from,
      to: ps.req.to,
      cc: ps.req.cc,
      bcc: ps.req.bcc,
      subject: ps.req.subject,
      body: ps.req.isHtml ? "" : ps.req.body,
      bodyHtml: ps.req.isHtml ? ps.req.body : undefined,
      threadId: ps.req.threadId,
      inReplyTo: ps.req.inReplyTo,
      showCc: !!(ps.req.cc || ps.req.bcc),
      nonce: Date.now(),
      attachments: ps.req.attachments ?? [],
      gmailDraftId: ps.draftId,
    });
  }

  return {
    closeCompose,
    doSaveDraft,
    scheduleAutosave,
    currentSendReq,
    scheduleCompose,
    beginSend,
    dispatchSend,
    sendCompose,
    sendComposeArchive,
    undoSend,
  };
}
