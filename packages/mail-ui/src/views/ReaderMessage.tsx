// A single message in the reader — collapsed preview or expanded card with head,
// sender card, details, per-message actions, body (sandboxed iframe), translation
// panel, Meet banner, and attachments. Prop-light: reads handlers/state from the
// store + useThreadView; takes the message, its thread index, and the find-bar
// callbacks (those are owned by ReaderView, which hosts the FindBar).
import { type ComponentProps, type CSSProperties, useRef, useState } from "react";
import { Button, IconButton, Popover } from "@rightkit/react-shell/react";
import Icon from "../components/Icons.js";
import { EmailFrame } from "../components/EmailFrame.js";
import {
  AttachmentPreviewModal,
  type AttachmentPreviewPayload,
} from "../components/AttachmentPreviewModal.js";
import { authBadge } from "../lib/auth.js";
import { emailDomain, trackerNoteFor } from "../lib/imageTrust.js";
import { relativeTimeFrom } from "@rightkit/platform-ui";
import { colorFor, initialOf, findMeetLink } from "../lib/mail.js";
import { htmlToText, rewriteCids, rewriteRemoteImgs } from "../lib/html.js";
import { useThreadView } from "../hooks/useThreadView.js";
import { openExternal, previewAttachment, type AttachmentMeta, type ThreadMessage } from "../lib/ipc.js";
import { useAppStore } from "../store/index.js";
import { ReaderMoreMenu } from "./ReaderMoreMenu.js";

type EmailFrameProps = ComponentProps<typeof EmailFrame>;

type Props = {
  m: ThreadMessage;
  i: number;
  registerFind: EmailFrameProps["registerFind"];
};

const COLLAPSIBLE_HEAD_STYLE: CSSProperties = { cursor: "pointer" };

export function ReaderMessage({ m, i, registerFind }: Props) {
  const expandedMsgs = useAppStore((s) => s.expandedMsgs);
  const threadMsgs = useAppStore((s) => s.threadMsgs);
  const openThread = useAppStore((s) => s.openThread);
  const senderCard = useAppStore((s) => s.senderCard);
  const detailMsgs = useAppStore((s) => s.detailMsgs);
  const moreOpenIdx = useAppStore((s) => s.moreOpenIdx);
  const setMoreOpenIdx = useAppStore((s) => s.setMoreOpenIdx);
  const openStarred = useAppStore((s) => s.openStarred);
  const translated = useAppStore((s) => s.translated);
  const showImages = useAppStore((s) => s.showImages);
  const inlineMap = useAppStore((s) => s.inlineMap);
  const remoteImgMap = useAppStore((s) => s.remoteImgMap);
  const remoteImgBlockedCount = useAppStore((s) => s.remoteImgBlockedCount);
  const msgAttach = useAppStore((s) => s.msgAttach);
  const setErr = useAppStore((s) => s.setErr);
  const openReply = useAppStore((s) => s.openReply);
  const openForward = useAppStore((s) => s.openForward);
  const toggleStar = useAppStore((s) => s.toggleStar);
  const moreBtnRef = useRef<HTMLButtonElement>(null);
  const detailsBtnRef = useRef<HTMLButtonElement>(null);
  const { toggleExpand, toggleDetails, openSenderCard, downloadAtt, saveAttToDrive, clearTranslation } =
    useThreadView();
  const [attachmentPreview, setAttachmentPreview] = useState<
    (AttachmentPreviewPayload & { attachment: AttachmentMeta }) | null
  >(null);

  function openAttachmentPreview(a: AttachmentMeta) {
    if (!openThread) return;
    previewAttachment(
      openThread.account_id,
      m.message_id,
      a.attachment_id,
      a.filename,
      a.mime_type,
    )
      .then((preview) =>
        setAttachmentPreview({
          filename: preview.filename,
          mimeType: preview.mime_type,
          dataB64: preview.data_b64,
          attachment: a,
        }),
      )
      .catch((e) => setErr(String(e)));
  }

  function recipientLine(): string {
    const to = (m.to_addrs || "").trim();
    if (!to) return "";
    const acct = openThread?.account_id ?? "";
    if (acct && to.toLowerCase().includes(acct.toLowerCase())) {
      return to.includes(",") ? "to me, others" : "to me";
    }
    return `to ${to}`;
  }

  const open = expandedMsgs.has(i);
  if (!open) {
    const preview = (m.is_html ? htmlToText(m.body) : m.body)
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 140);
    return (
      <Button variant="ghost"
        type="button"
        className={`msg collapsed ${m.unread ? "unread" : ""}`}
        onClick={() => toggleExpand(i)}
      >
        <span
          className="msg-avatar"
          style={{ background: colorFor(m.from_addr || m.from_name) }}
        >
          {initialOf(m.from_name || m.from_addr)}
        </span>
        <span className="msg-from">{m.from_name}</span>
        <span className="msg-collapsed-preview">{preview}</span>
        <span className="msg-time">{relativeTimeFrom(m.internal_date)}</span>
      </Button>
    );
  }

  const meetLink = findMeetLink(m.body);
  const collapsible = threadMsgs.length > 1;
  return (
    <div className="msg">
      <div
        className="msg-head"
        style={collapsible ? COLLAPSIBLE_HEAD_STYLE : undefined}
        onClick={(e) => {
          if (!collapsible) return;
          if (
            (e.target as HTMLElement).closest(
              "button,[role=button],a,input,select,.sender-card,.msg-details"
            )
          )
            return;
          toggleExpand(i);
        }}
      >
        <div className="msg-sender-wrap">
          <span
            className="msg-avatar msg-avatar-btn"
            style={{ background: colorFor(m.from_addr || m.from_name) }}
            role="button"
            tabIndex={0}
            title="Contact info"
            onClick={() => openSenderCard(m.from_addr)}
          >
            {initialOf(m.from_name || m.from_addr)}
          </span>
          {senderCard && senderCard.email === m.from_addr && (
            <div className="sender-card">
              {senderCard.loading ? (
                <span className="hint">Looking up…</span>
              ) : senderCard.card ? (
                <>
                  {senderCard.card.photo_url ? (
                    <img
                      className="sender-card-photo"
                      src={senderCard.card.photo_url}
                      alt=""
                    />
                  ) : (
                    <span
                      className="sender-card-photo sender-card-initial"
                      style={{ background: colorFor(m.from_addr) }}
                    >
                      {initialOf(senderCard.card.display_name || m.from_addr)}
                    </span>
                  )}
                  <div className="sender-card-meta">
                    <span className="sender-card-name">
                      {senderCard.card.display_name || m.from_name || m.from_addr}
                    </span>
                    <span className="sender-card-email">
                      {senderCard.card.email || m.from_addr}
                    </span>
                    {senderCard.card.org_title && (
                      <span className="sender-card-org">{senderCard.card.org_title}</span>
                    )}
                  </div>
                </>
              ) : (
                <span className="hint">No saved contact for this sender.</span>
              )}
            </div>
          )}
        </div>
        <div className="msg-meta">
          <span className="msg-line1">
            <span className="msg-from">{m.from_name}</span>
            <span className="msg-addr">&lt;{m.from_addr}&gt;</span>
            {(() => {
              const b = authBadge(m.auth);
              return b ? (
                <span className={`auth-badge ${b.level}`} title={b.title}>
                  {b.label}
                </span>
              ) : null;
            })()}
          </span>
          <div className="msg-details-wrap">
            <Button variant="ghost"
              ref={detailsBtnRef}
              className="msg-to msg-details-toggle"
              onClick={() => toggleDetails(i)}
              title="Message details"
            >
              {recipientLine() || "Details"}{" "}
              <span className="caret">{detailMsgs.has(i) ? "▴" : "▾"}</span>
            </Button>
            {detailMsgs.has(i) && detailsBtnRef.current && (
              // Portal events bubble through the React tree; keep them off .msg-head.
              <span style={{ display: "contents" }} onClick={(e) => e.stopPropagation()}>
                <Popover
                  anchor={detailsBtnRef.current}
                  onClose={() => toggleDetails(i)}
                  label="Message details"
                  width={420}
                  className="msg-details"
                >
                  <div>
                    <span className="md-label">From:</span> {m.from_name} &lt;{m.from_addr}&gt;
                  </div>
                  {emailDomain(m.from_addr) && (
                    <div>
                      <span className="md-label">Mailed-by:</span> {emailDomain(m.from_addr)}
                    </div>
                  )}
                  {m.to_addrs && (
                    <div>
                      <span className="md-label">To:</span> {m.to_addrs}
                    </div>
                  )}
                  {m.cc_addrs && (
                    <div>
                      <span className="md-label">Cc:</span> {m.cc_addrs}
                    </div>
                  )}
                  {m.reply_to && (
                    <div>
                      <span className="md-label">Reply-To:</span> {m.reply_to}
                    </div>
                  )}
                  <div>
                    <span className="md-label">Date:</span>{" "}
                    {new Date(m.internal_date).toLocaleString()}
                  </div>
                  {m.auth && (
                    <div>
                      <span className="md-label">Authentication:</span> SPF {m.auth.spf} · DKIM{" "}
                      {m.auth.dkim} · DMARC {m.auth.dmarc}
                    </div>
                  )}
                </Popover>
              </span>
            )}
          </div>
        </div>
        <div className="msg-head-actions">
          <span className="msg-time" title={new Date(m.internal_date).toLocaleString()}>
            {relativeTimeFrom(m.internal_date)}
          </span>
          {threadMsgs.length > 1 && (
            <IconButton label="Collapse" onClick={() => toggleExpand(i)}>
              ▴
            </IconButton>
          )}
          <IconButton
            label={openStarred ? "Unstar" : "Star"} pressed={openStarred}
            onClick={toggleStar}
          >
            <Icon name="star" size={16} filled={openStarred} />
          </IconButton>
          <IconButton label="Reply" onClick={() => openReply(m, false)}>
            <Icon name="reply" size={16} />
          </IconButton>
          <IconButton label="Forward" onClick={() => openForward(m)}>
            <Icon name="forward" size={16} />
          </IconButton>
          <IconButton label="Print" onClick={() => window.print()}>
            <Icon name="print" size={16} />
          </IconButton>
          <div className="more-wrap">
            <IconButton
              ref={moreBtnRef}
              label="More"
              onClick={() => setMoreOpenIdx(moreOpenIdx === i ? null : i)}
            >
              <Icon name="more" size={16} />
            </IconButton>
            {moreOpenIdx === i && moreBtnRef.current && <ReaderMoreMenu m={m} anchor={moreBtnRef.current} />}
          </div>
        </div>
      </div>
      {translated[m.message_id] && (
        <div className="translated-panel">
          <div className="translated-head">
            <span>
              Translated
              {translated[m.message_id].source
                ? ` from ${translated[m.message_id].source.toUpperCase()}`
                : ""}
            </span>
            <Button variant="ghost" size="sm" onClick={() => clearTranslation(m.message_id)}>
              Show original
            </Button>
          </div>
          <div className="translated-body">{translated[m.message_id].text}</div>
        </div>
      )}
      {meetLink && (
        <div className="meet-banner">
          <Icon name="video" size={16} />
          <span>Google Meet</span>
          <Button
            variant="primary" size="sm"
            onClick={() => openExternal(meetLink).catch((e) => setErr(String(e)))}
          >
            Join
          </Button>
        </div>
      )}
      {m.is_html ? (
        <EmailFrame
          html={rewriteRemoteImgs(
            rewriteCids(m.body, inlineMap[m.message_id]),
            showImages ? remoteImgMap[m.message_id] : undefined
          )}
          title={`message-${i}`}
          loadImages={showImages}
          findId={m.message_id}
          registerFind={registerFind}
        />
      ) : (
        <div className="msg-body">{m.body}</div>
      )}
      {trackerNoteFor(showImages, remoteImgBlockedCount[m.message_id]) && (
        <div className="tracker-note">
          <Icon name="lock" size={13} />
          <span>{trackerNoteFor(showImages, remoteImgBlockedCount[m.message_id])}</span>
        </div>
      )}
      {(msgAttach[m.message_id]?.length ?? 0) > 0 && (
        <div className="att-strip">
          {msgAttach[m.message_id].map((a) => (
            <span className="att-group" key={a.attachment_id}>
              <Button variant="ghost"
                className="att-chip"
                title={`Preview ${a.filename}`}
                onClick={() => openAttachmentPreview(a)}
              >
                <Icon name="folder" size={14} />
                <span className="att-name">{a.filename}</span>
              </Button>
              <IconButton label={`Download ${a.filename}`}
                className="att-chip att-chip-icon"
                title={`Download ${a.filename}`}
                aria-label={`Download ${a.filename}`}
                onClick={() => downloadAtt(m.message_id, a)}
              >
                <Icon name="download" size={14} />
              </IconButton>
              <IconButton label={`Save ${a.filename} to Drive`}
                className="att-chip att-chip-icon"
                title={`Save ${a.filename} to Drive`}
                aria-label={`Save ${a.filename} to Drive`}
                onClick={() => saveAttToDrive(m.message_id, a)}
              >
                <Icon name="drive" size={14} />
              </IconButton>
            </span>
          ))}
        </div>
      )}
      {attachmentPreview && (
        <AttachmentPreviewModal
          preview={attachmentPreview}
          onClose={() => setAttachmentPreview(null)}
          onDownload={() => downloadAtt(m.message_id, attachmentPreview.attachment)}
        />
      )}
    </div>
  );
}
