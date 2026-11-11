import { cx } from "@rightkit/react-shell/react";
import { memo, useCallback, useEffect, useMemo } from "react";
import type { JobInfo, TranscriptDetail } from "../contract.js";
import { invoke } from "../lib/ipc/index.js";
import { isActiveStage, statusText } from "../lib/jobDisplay.js";
import { loadTranscriptDetail } from "../lib/transcriptLoad.js";
import { useFlash } from "../lib/flash.js";
import { useTranscript } from "../lib/transcriptHooks.js";
import { savedMediaKind } from "../lib/mediaSelection.js";
import { countWords } from "../../reader/ReaderParts.js";
import { copyTextToClipboard } from "../lib/workbench.js";
import { TranscriptFileActions } from "./TranscriptFileActions.js";
import { buttonClass, errorTextClass, mutedTextClass, panelClass } from "../../ui.js";
import {
  Check,
  Clipboard,
  Copy,
  FileText,
  PanelRightOpen,
  Video,
  X,
} from "lucide-react";
import { JobStatePreview } from "./JobStatePreview.js";

type WorkbenchPreviewProps = {
  job: JobInfo | null;
  onOpen: (id: string) => void;
  onClose: () => void;
  onRetryAfterSignin: (job: JobInfo, options?: { skipExport?: boolean }) => void;
  onError: (message: string) => void;
};

function WorkbenchPreviewImpl({
  job,
  onOpen,
  onClose,
  onRetryAfterSignin,
  onError,
}: WorkbenchPreviewProps) {
  const ready = job?.stage === "done";
  const { detail, loading, error: previewErr } = useTranscript(job?.id, ready, (id) =>
    loadTranscriptDetail(invoke, id),
  );
  const [copied, flashCopied, resetCopied] = useFlash(1400);
  // Re-arm the copy flag whenever the shown job changes. The id and stage are
  // intentional triggers that the body does not read, so they stay in the list.
  // biome-ignore lint/correctness/useExhaustiveDependencies: job identity and stage are the reset trigger
  useEffect(() => {
    resetCopied();
  }, [job?.id, job?.stage, resetCopied]);

  // Memoized so re-renders from progress events never re-scan the transcript (issue #224).
  const wordCount = useMemo(() => countWords(detail?.text), [detail?.text]);


  const copyTranscript = useCallback(() => {
    const text = detail?.text;
    if (!text) return;
    copyTextToClipboard(text)
      .then(flashCopied)
      .catch((e) => onError(String(e)));
  }, [detail, onError, flashCopied]);

  if (!job) {
    return (
      <section
        className={cx(panelClass, "grid min-h-0 place-items-center p-8 text-center")}
        aria-label="Workbench preview"
      >
        <div>
          <Clipboard
            className="mx-auto mb-3 text-[var(--muted)]"
            size={28}
            strokeWidth={1.7}
            aria-hidden="true"
          />
          <h2 className="m-0 text-lg font-semibold text-[var(--paper)]">No source selected</h2>
          <p className="m-0 mt-2 max-w-[360px] text-sm leading-6 text-[var(--muted)]">
            Add a source to start building a transcript preview here.
          </p>
        </div>
      </section>
    );
  }

  const active = isActiveStage(job.stage);
  const progress = Math.max(0, Math.min(100, job.pct));
  const title = job.title ?? job.input;
  const done = job.stage === "done";
  const hasTranscript = Boolean(detail?.text.trim());
  const hasMedia = Boolean(detail?.media_path);
  const mediaKind = savedMediaKind(detail?.media_meta, detail?.media_path);
  const readyLabel = hasMedia
    ? mediaKind === "audio"
      ? "Downloaded audio"
      : "Downloaded video + audio"
    : `${wordCount} words · ${detail?.variants?.some((v) => v.kind === "srt") || detail?.srt ? "timestamped exports" : "plain exports"}`;

  return (
    <section
      className={cx(
        panelClass,
        "grid min-h-0 grid-cols-[minmax(0,1fr)] grid-rows-[auto_minmax(0,1fr)_auto] overflow-hidden",
      )}
      aria-label="Workbench preview"
    >
      <div className="min-w-0 border-b border-[var(--line)] px-4 py-2.5">
        <h2 className="m-0 truncate text-base font-semibold text-[var(--paper)]" title={title}>
          {title}
        </h2>
        <p className="m-0 mt-0.5 font-[family-name:var(--mono)] text-xs text-[var(--muted)]">
          {statusText(job)}
        </p>
      </div>

      <div className="min-h-0 overflow-auto">
        {done ? (
          <TranscriptPreview detail={detail} loading={loading} error={previewErr} />
        ) : (
          <JobStatePreview
            job={job}
            active={active}
            progress={progress}
            onRetryAfterSignin={onRetryAfterSignin}
            onError={onError}
          />
        )}
      </div>

      <div className="flex flex-wrap items-center justify-between gap-2 border-t border-[var(--line)] px-4 py-2">
        <span className="font-[family-name:var(--mono)] text-xs text-[var(--muted)]">
          {done && detail ? readyLabel : statusText(job)}
        </span>
        <div className="flex flex-wrap items-center gap-2">
          {done && (
            <>
              {hasTranscript && (
                <button
                  type="button"
                  className={buttonClass}
                  onClick={copyTranscript}
                  title="Copy transcript text"
                  aria-label="Copy transcript text"
                >
                  {copied ? <Check size={15} strokeWidth={2} /> : <Copy size={15} strokeWidth={1.9} />}
                  <span>{copied ? "Copied" : "Copy"}</span>
                </button>
              )}
              <TranscriptFileActions
                transcriptId={detail?.id ?? null}
                hasTranscript={hasTranscript}
                hasMedia={hasMedia}
                onError={onError}
              />
              <button
                type="button"
                className={buttonClass}
                onClick={() => onOpen(job.id)}
                title="Open full details"
                aria-label="Open details"
              >
                <PanelRightOpen size={15} strokeWidth={1.8} />
                <span>Details</span>
              </button>
            </>
          )}
          <button
            type="button"
            className={buttonClass}
            onClick={onClose}
            title="Close preview"
            aria-label="Close preview"
          >
            <X size={15} strokeWidth={1.9} />
            <span>Close</span>
          </button>
        </div>
      </div>
    </section>
  );
}

function TranscriptPreview({
  detail,
  loading,
  error,
}: {
  detail: TranscriptDetail | null;
  loading: boolean;
  error: string | null;
}) {
  if (loading) {
    return <div className={cx("p-5 text-sm", mutedTextClass)}>Loading transcript preview...</div>;
  }
  if (error) {
    return (
      <div className={cx("p-5 font-[family-name:var(--mono)] text-xs", errorTextClass)}>
        Preview failed: {error}
      </div>
    );
  }
  if (!detail) {
    return (
      <div className={cx("p-5 text-sm", mutedTextClass)}>Transcript preview is not available.</div>
    );
  }

  if (!detail.text.trim() && detail.media_path) {
    const mediaKind = savedMediaKind(detail.media_meta, detail.media_path);
    const mediaName = detail.media_path.split(/[\\/]/).pop();
    return (
      <div className="p-6">
        <div className={cx(panelClass, "flex items-center gap-3 p-4")}>
          <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-[8px] bg-[color-mix(in_srgb,var(--accent)_14%,transparent)] text-[var(--accent)]">
            {mediaKind === "audio" ? <FileText size={19} /> : <Video size={19} />}
          </span>
          <span className="min-w-0">
            <span className="block text-sm font-medium text-[var(--paper)]">Download ready</span>
            <span className="mt-1 block truncate font-[family-name:var(--mono)] text-xs text-[var(--muted)]">
              {mediaName}
            </span>
          </span>
        </div>
      </div>
    );
  }

  return (
    <div className="p-6">
      <p className="m-0 max-w-[80ch] whitespace-pre-wrap break-words text-[15px] leading-7 text-[var(--paper)]">
        {detail.text || "Output is ready."}
      </p>
    </div>
  );
}

export const WorkbenchPreview = memo(WorkbenchPreviewImpl);
