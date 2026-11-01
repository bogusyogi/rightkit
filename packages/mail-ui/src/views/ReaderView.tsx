// ReaderView — the open-thread shell: find bar, toolbar, snooze picker, subject,
// label chips, trust/AI/draft banners, and the message list. Each message renders
// via ReaderMessage (which owns the ⋯ menu via ReaderMoreMenu). Prop-less: reads
// state from the store and handlers from useThreadView.
import { memo } from "react";
import { FindBar } from "../components/FindBar.js";
import { unsubUrl, labelChip } from "../lib/mail.js";
import { useThreadView } from "../hooks/useThreadView.js";
import { openExternal } from "../lib/ipc.js";
import { useAppStore } from "../store/index.js";
import { useReaderFindProvider } from "../hooks/useReaderFindProvider.js";
import { ReaderToolbar } from "./ReaderToolbar.js";
import { ReaderMessage } from "./ReaderMessage.js";
import { Button, SkeletonRows, Input } from "@rightkit/react-shell/react";

function ReaderViewInner() {
  const findOpen = useAppStore((s) => s.findOpen);
  const setFindOpen = useAppStore((s) => s.setFindOpen);

  const { findProvider, registerFind } = useReaderFindProvider(findOpen);

  const { snoozeOpen, openDraftForEdit } = useThreadView();
  const aiSummary = useAppStore((s) => s.aiSummary);
  const customSnoozeAt = useAppStore((s) => s.customSnoozeAt);
  const customSnoozeOpen = useAppStore((s) => s.customSnoozeOpen);
  const folder = useAppStore((s) => s.folder);
  const labels = useAppStore((s) => s.labels);
  const openLabels = useAppStore((s) => s.openLabels);
  const openThread = useAppStore((s) => s.openThread);
  const setAiSummary = useAppStore((s) => s.setAiSummary);
  const setCustomSnoozeAt = useAppStore((s) => s.setCustomSnoozeAt);
  const setCustomSnoozeOpen = useAppStore((s) => s.setCustomSnoozeOpen);
  const setErr = useAppStore((s) => s.setErr);
  const threadMsgs = useAppStore((s) => s.threadMsgs);
  const trustReport = useAppStore((s) => s.trustReport);

  // Rendered only when a thread is open (App gates on `openThread`); narrow here.
  if (!openThread) return null;

  const unsubLink =
    threadMsgs.map((m) => unsubUrl(m.list_unsubscribe)).find(Boolean) ?? null;

  // Chip text resolving custom Label_NN ids to their real names via the catalog.
  function chipText(l: string): string | null {
    if (/^Label_\d+$/.test(l)) {
      const n = labels.find((x) => x.id === l)?.name ?? l;
      return n === l ? null : n; // unresolved id → hide
    }
    return labelChip(l);
  }

  return (
    <div className="reader">
      {findOpen && (
        <FindBar provider={findProvider} onClose={() => setFindOpen(false)} />
      )}
      <ReaderToolbar />
      {customSnoozeOpen && (
        <div className="custom-snooze">
          <span>Snooze until</span>
          <Input
            type="datetime-local"
            value={customSnoozeAt}
            onChange={(e) => setCustomSnoozeAt(e.target.value)}
          />
          <Button
            variant="primary" size="sm"
            onClick={() => {
              if (!customSnoozeAt) {
                setErr("Pick a date and time to snooze until.");
                return;
              }
              const ts = new Date(customSnoozeAt).getTime();
              if (Number.isNaN(ts)) {
                setErr("Invalid date format.");
                return;
              }
              if (ts <= Date.now()) {
                setErr("Snooze time must be in the future.");
                return;
              }
              snoozeOpen(ts);
              setCustomSnoozeOpen(false);
            }}
          >
            Snooze
          </Button>
          <Button
            variant="ghost" size="sm"
            onClick={() => setCustomSnoozeOpen(false)}
          >
            Cancel
          </Button>
        </div>
      )}
      <h1 className="reader-subject">{openThread.subject || "(no subject)"}</h1>
      {(openLabels.map(chipText).filter(Boolean).length > 0 || unsubLink) && (
        <div className="reader-chips">
          {openLabels.map((l) => {
            const text = chipText(l);
            return text ? (
              <span className="label-chip" key={l}>
                {text}
              </span>
            ) : null;
          })}
          {unsubLink && (
            <Button variant="ghost"
              className="unsub-btn"
              title={unsubLink}
              onClick={() =>
                openExternal(unsubLink).catch((e) => setErr(String(e)))
              }
            >
              Unsubscribe
            </Button>
          )}
        </div>
      )}
      {trustReport && trustReport.level !== "info" && (
        <div className={`trust-banner ${trustReport.level}`}>
          {trustReport.findings
            .filter((f) => f.level !== "info")
            .map((f, i) => (
              <span key={i}>{f.message}</span>
            ))}
        </div>
      )}
      {aiSummary && (
        <div className="ai-summary">
          <div className="ai-summary-head">
            <span>AI summary</span>
            <Button variant="ghost" size="sm" onClick={() => setAiSummary(null)}>
              Dismiss
            </Button>
          </div>
          <div className="ai-summary-body">{aiSummary}</div>
        </div>
      )}
      {folder === "drafts" && threadMsgs.length > 0 && (
        <div className="draft-banner">
          <span>This is a draft.</span>
          <Button
            variant="primary" size="sm"
            onClick={() => openDraftForEdit(threadMsgs[threadMsgs.length - 1])}
          >
            Edit draft
          </Button>
        </div>
      )}
      {threadMsgs.length === 0 && <SkeletonRows rows={5} />}
      {/* Newest message on top (Kiwi-style). Iterate indices in reverse but keep
          the ORIGINAL index `i`, so expand/details/more-menu state (all keyed by
          i) stays correct. */}
      {[...threadMsgs.keys()].reverse().map((i) => (
        <ReaderMessage
          key={`${openThread.account_id}:${openThread.thread_id}:${threadMsgs[i].message_id}`}
          m={threadMsgs[i]}
          i={i}
          registerFind={registerFind}
        />
      ))}
    </div>
  );
}

export const ReaderView = memo(ReaderViewInner);
