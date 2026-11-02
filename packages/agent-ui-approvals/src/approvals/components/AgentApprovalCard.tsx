// Approval card: an agent asked to send a drafted message, forward an existing
// one, or move a conversation to Trash. Nothing happens until the user clicks
// Approve here, every time. Everything shown is plain text read from Gmail (a
// move to Trash: from the local database) when the card opened (agent- and
// sender-written text is never rendered as HTML and links are not activated).
// Focus starts on Deny and Enter does not approve; only a click or Enter on the
// focused Approve button does. The heading and the Approve button name the
// action: Send, Forward, Move to Trash or Attach file. An attach card shows the
// exact path and the size in bytes; the file is read only after Approve.

import { useEffect, useId, useRef, useState } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import {
  agentApprovalApprove,
  agentApprovalCard,
  agentApprovalDeny,
  type ApprovalCard,
  type ApprovalDecision,
} from "@rightkit/mail-ui/lib/ipc";
import {
  approveAriaLabel,
  approveLabel,
  decisionMessage,
  denyAriaLabel,
  formatCountdown,
  kindHeading,
  kindNotice,
  nothingUnlessApproved,
  queuePosition,
  requestSentence,
  secondsLeft,
  type QueueAction,
} from "../lib/approvalQueue.js";
import type { PendingApproval } from "@rightkit/mail-ui/lib/ipc/agentApprovals";
import { formatBytes } from "@rightkit/settings-ui/core";
import { bodyStats, displayBody, visibleText } from "@rightkit/mail-ui/lib/visibleText";
import { Button, ModalShell } from "@rightkit/react-shell/react";
import { AgentApprovalThread } from "./AgentApprovalThread.js";

type Props = {
  queue: PendingApproval[];
  now: number;
  refresh: () => Promise<void>;
  dispatch: (a: QueueAction) => void;
};

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function Recipients({ label, list }: { label: string; list: string[] }) {
  if (list.length === 0) return null;
  return (
    <div style={{ marginTop: 4 }}>
      <strong>{label}</strong>
      <ul
        aria-label={`${label} recipients`}
        style={{ margin: 0, paddingLeft: 18, overflowWrap: "anywhere" }}
      >
        {list.map((r, i) => (
          <li key={`${i}-${r}`}>{visibleText(r)}</li>
        ))}
      </ul>
    </div>
  );
}

export function AgentApprovalCard({ queue, now, refresh, dispatch }: Props) {
  const current = queue[0];
  const titleId = useId();
  const timerId = useId();
  const denyRef = useRef<HTMLButtonElement>(null);
  const okRef = useRef<HTMLButtonElement>(null);
  const [card, setCard] = useState<ApprovalCard | null>(null);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<string | null>(null);
  const currentId = current?.id;

  // Fetch the content by id each time a different approval comes to the front.
  useEffect(() => {
    setCard(null);
    setLoadErr(null);
    if (!currentId) return;
    let live = true;
    agentApprovalCard(currentId)
      .then((c) => live && setCard(c))
      .catch((e) => live && setLoadErr(errorText(e)));
    return () => {
      live = false;
    };
  }, [currentId]);

  // Focus starts on Deny for every card (and on OK for a result notice).
  useEffect(() => {
    (result ? okRef : denyRef).current?.focus();
  }, [currentId, result, card?.id]);

  const left = current ? secondsLeft(current.expires_at, now) : 0;
  const finish = async (id: string, d: ApprovalDecision) => {
    dispatch({ type: "decided", id });
    const msg = decisionMessage(d, current?.kind ?? "send");
    if (msg) setResult(msg);
    await refresh();
  };
  const run = async (act: () => Promise<ApprovalDecision>, id: string) => {
    if (busy) return;
    setBusy(true);
    try {
      await finish(id, await act());
    } catch (e) {
      setResult(errorText(e));
      await refresh();
    } finally {
      setBusy(false);
    }
  };

  // Never let a stray Enter reach Approve. ModalShell traps Tab and focus.
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Enter" && !(e.target instanceof HTMLButtonElement)) e.preventDefault();
  };

  // Esc is the safe answer: it dismisses the result notice, or denies the request
  // (nothing happens unless the user approves). It never approves.
  const onEscape = () => {
    if (result) return setResult(null);
    if (current && !busy) void run(() => agentApprovalDeny(current.id), current.id);
  };

  const shell = (children: ReactNode) => (
    <ModalShell
      key={result ? "result" : current?.id}
      label="Agent request"
      role="alertdialog"
      width={560}
      autoFocus={false}
      dismissOnBackdrop={false}
      labelledBy={titleId}
      describedBy={timerId}
      overlayStyle={{ zIndex: 200 }}
      onClose={onEscape}
    >
      <div
        onKeyDown={onKeyDown}
        style={{ maxHeight: "85vh", display: "flex", flexDirection: "column" }}
      >
        {children}
      </div>
    </ModalShell>
  );

  if (result) {
    return shell(
      <>
        <h3 id={titleId}>Agent request</h3>
        <p id={timerId} role="status">
          {result}
        </p>
        <div className="rk-modal__actions">
          <Button ref={okRef} type="button" variant="primary" onClick={() => setResult(null)}>
            OK
          </Button>
        </div>
      </>,
    );
  }

  if (!current) return null;
  // Approve and Deny act only on the card that was loaded for this very item.
  const cardMatches = card?.id === current.id;
  const kind = current.kind;
  const notice = kindNotice(kind);
  const isDelete = kind === "delete";
  const isAttach = kind === "attach";
  // A move to Trash needs the conversation on the card; a delete card has no hash.
  const canApprove =
    cardMatches &&
    (!isDelete || Boolean(card?.thread)) &&
    (!isAttach || Boolean(card?.attach)) &&
    left > 0 &&
    !busy;
  const stats = card && !isDelete && !isAttach ? bodyStats(card.body) : null;
  const boxStyle = {
    marginTop: 8,
    padding: 10,
    maxHeight: 260,
    overflow: "auto",
    whiteSpace: "pre-wrap",
    overflowWrap: "anywhere",
    border: "1px solid var(--line)",
    borderRadius: 8,
    font: "inherit",
  } as const;
  return shell(
    <>
      <h3 id={titleId}>
        {kindHeading(kind)}: {visibleText(current.agent_name) || "An agent"}{" "}
        {requestSentence(kind)}
        {queuePosition(queue.length) && (
          <span className="hint" style={{ marginLeft: 8 }}>
            {queuePosition(queue.length)}
          </span>
        )}
      </h3>
      <p id={timerId} role="timer" style={{ margin: "0 0 8px" }}>
        Expires in {formatCountdown(left)}. {nothingUnlessApproved(kind)}
      </p>
      <div style={{ overflowY: "auto", minHeight: 0 }}>
        <div>
          <strong>From account:</strong> {visibleText(current.account)}
        </div>
        {loadErr && <p role="alert">{loadErr}</p>}
        {!card && !loadErr && (
          <p>
            {isDelete
              ? "Loading the conversation..."
              : isAttach
                ? "Reading the file's size..."
                : "Loading the draft from Gmail..."}
          </p>
        )}
        {card && isAttach && card.attach && (
          <>
            <div style={{ marginTop: 4, overflowWrap: "anywhere" }}>
              <strong>File:</strong> <code>{visibleText(card.attach.path)}</code>
            </div>
            <div style={{ marginTop: 4 }}>
              <strong>Size:</strong> {card.attach.size.toLocaleString("en-US")} bytes (
              {formatBytes(card.attach.size)})
            </div>
            {notice && (
              <p role="note" style={{ margin: "10px 0 0" }}>
                <strong>{notice}</strong>
              </p>
            )}
          </>
        )}
        {card && isDelete && card.thread && (
          <>
            <AgentApprovalThread thread={card.thread} />
            {notice && (
              <p role="note" style={{ margin: "10px 0 0" }}>
                <strong>{notice}</strong>
              </p>
            )}
          </>
        )}
        {card && !isDelete && !isAttach && (
          <>
            {card.edited_since_created && (
              <p className="hint">This draft was changed after the agent created it.</p>
            )}
            {card.reply_to && (
              <div style={{ marginTop: 4, overflowWrap: "anywhere" }}>
                <strong>Reply-To:</strong> {visibleText(card.reply_to)}
              </div>
            )}
            <Recipients label="To" list={card.to} />
            <Recipients label="Cc" list={card.cc} />
            <Recipients label="Bcc" list={card.bcc} />
            {notice && (
              <p role="note" style={{ margin: "10px 0 0" }}>
                <strong>{notice}</strong>
              </p>
            )}
            <div style={{ marginTop: 6, overflowWrap: "anywhere" }}>
              <strong>Subject:</strong> {visibleText(card.subject)}
            </div>
            {card.attachments.length > 0 && (
              <Recipients
                label="Attachments"
                list={card.attachments.map(
                  (a) => `${a.name || "unnamed"} (${a.mime_type || "unknown type"}, ${formatBytes(a.size)})`,
                )}
              />
            )}
            {card.html_only && (
              <p className="hint">This draft is HTML. Its source is shown as plain text.</p>
            )}
            {stats && (
              <p className="hint" style={{ margin: "8px 0 0" }}>
                Body: {stats.chars} characters, {stats.lines} lines
              </p>
            )}
            <pre tabIndex={0} aria-label="Message body" style={boxStyle}>
              {displayBody(card.body)}
            </pre>
            {card.html_differs && (
              <>
                <p className="hint" style={{ margin: "8px 0 0" }}>
                  <strong>Formatted version (what most recipients see)</strong>
                </p>
                <pre tabIndex={0} aria-label="Formatted version" style={boxStyle}>
                  {displayBody(card.html_text)}
                </pre>
              </>
            )}
          </>
        )}
      </div>
      <div className="rk-modal__actions" style={{ marginTop: 14 }}>
        <Button
          ref={denyRef}
          type="button"
          variant="primary"
          disabled={busy || (!cardMatches && !loadErr)}
          aria-label={denyAriaLabel(kind, visibleText(current.agent_name))}
          onClick={() => void run(() => agentApprovalDeny(current.id), current.id)}
        >
          Deny
        </Button>
        <Button variant="primary"
          type="button"
          disabled={!canApprove}
          aria-label={approveAriaLabel(kind, visibleText(current.account))}
          onClick={() =>
            card && cardMatches && void run(() => agentApprovalApprove(current.id, card.content_hash), current.id)
          }
        >
          {approveLabel(kind)}
        </Button>
      </div>
    </>,
  );
}
