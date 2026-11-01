import { resolveImageRule, RIGHTKIT_IMG_BASE } from "../../../lib/imageTrust";
import {
  getThread,
  inlineImageRefs,
  listAttachments,
  markThreadRead,
  threadLabels,
  trustCheck,
  unreadCounts,
  type Thread,
} from "../../../lib/ipc";
import type { StoreGet } from "../../mk";

export async function openThreadAction(get: StoreGet, t: Thread) {
  const s = get();
  const isCurrentThread = () => {
    const current = get().openThread;
    return (
      current?.account_id === t.account_id &&
      current?.thread_id === t.thread_id
    );
  };
  s.setShowImages(s.imagePolicy);
  s.setMoreOpenIdx(null);
  s.setLabelPickerOpen(false);
  s.setMsgAttach({});
  s.setInlineMap({});
  s.setRemoteImgMap({});
  s.setThreadMsgs([]);
  s.setExpandedMsgs(new Set());
  s.setDetailMsgs(new Set());
  s.setOpenLabels([]);
  s.setOpenStarred(false);
  s.setCustomSnoozeOpen(false);
  s.setAiSummary(null);
  s.setTrustReport(null);
  s.setOpenThread({
    account_id: t.account_id,
    thread_id: t.thread_id,
    subject: t.subject,
  });
  try {
    const msgs = await getThread(t.account_id, t.thread_id);
    if (!isCurrentThread()) return;
    s.setThreadMsgs(msgs);
    const unreadIdx = msgs.flatMap((m, i) => (m.unread ? [i] : []));
    s.setExpandedMsgs(
      new Set(
        s.expandAll
          ? msgs.map((_, i) => i)
          : unreadIdx.length
            ? unreadIdx
            : msgs.length
              ? [msgs.length - 1]
              : [],
      ),
    );
    msgs
      .filter((m) => m.is_html && m.body.includes("cid:"))
      .forEach((m) => {
        inlineImageRefs(t.account_id, m.message_id)
          .then((refs) => {
            if (!isCurrentThread()) return;
            if (refs.length === 0) return;
            const map: Record<string, string> = {};
            for (const r of refs) {
              map[r.cid] = r.hash
                ? `${RIGHTKIT_IMG_BASE}${r.hash}`
                : `data:${r.mime_type};base64,${r.data_b64}`;
            }
            s.setInlineMap((prev) => ({ ...prev, [m.message_id]: map }));
          })
          .catch(() => {});
      });
    const rule = msgs[0]?.from_addr
      ? resolveImageRule(
          msgs[0].from_addr,
          s.senderImageTrust,
          s.domainImageTrust,
        )
      : null;
    if (rule === "show") s.setShowImages(true);
    else if (rule === "block") s.setShowImages(false);
    const m0 = msgs[0];
    if (m0) {
      trustCheck(m0.from_addr, m0.from_name, m0.reply_to, m0.body)
        .then((report) => {
          if (isCurrentThread()) s.setTrustReport(report);
        })
        .catch(() => {});
    }
    msgs
      .filter((m) => m.has_attachments)
      .forEach((m) => {
        listAttachments(t.account_id, m.message_id)
          .then((atts) => {
            if (isCurrentThread()) {
              s.setMsgAttach((prev) => ({ ...prev, [m.message_id]: atts }));
            }
          })
          .catch(() => {});
      });
  } catch (e) {
    if (!isCurrentThread()) return;
    s.setErr(String(e));
  }
  threadLabels(t.account_id, t.thread_id)
    .then((ls) => {
      if (!isCurrentThread()) return;
      s.setOpenLabels(ls);
      s.setOpenStarred(ls.includes("STARRED"));
    })
    .catch(() => {
      if (!isCurrentThread()) return;
      s.setOpenLabels([]);
      s.setOpenStarred(false);
    });
  if (t.unread) {
    const same = (x: Thread) =>
      x.account_id === t.account_id && x.thread_id === t.thread_id;
    s.setThreads((prev) =>
      prev.map((x) => (same(x) ? { ...x, unread: false } : x)),
    );
    s.setResults((prev) =>
      prev.map((x) => (same(x) ? { ...x, unread: false } : x)),
    );
    markThreadRead(t.account_id, t.thread_id)
      .then(() =>
        unreadCounts()
          .then(s.setCounts)
          .catch(() => {}),
      )
      .catch(() => {});
  }
}
