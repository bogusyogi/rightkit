import { memo } from "react";
import type { JobInfo } from "../contract.js";
import { invoke } from "../lib/ipc/index.js";
import {
  isActiveStage,
  outputLabelForJob,
  categoryForKind,
  retryTitle,
  labelForJob,
  statusText,
} from "../lib/jobDisplay.js";
import { showsOutputChip } from "../lib/queueView.js";
import { categoryIcon } from "./CategoryIcon.js";
import { buttonClass, errorTextClass, iconActionAccentClass, iconActionDangerClass } from "../lib/ui.js";
import { Badge, IconButton, cx } from "@rightkit/react-shell/react";
import { ArrowRight, LogIn, RefreshCcw, RotateCcw, Square, X } from "lucide-react";

// Secondary row tools stay hidden until the row is hovered or has keyboard focus (issue #227).
const revealOnRowHover =
  "opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100";

type BadgeTone = "neutral" | "accent" | "ok" | "warn" | "bad";

// Nearest RightKit Badge tone per source kind; failures and sign-in needs override it.
const kindTone: Partial<Record<JobInfo["kind"], BadgeTone>> = {
  instagram_post: "accent",
  carousel: "accent",
  reel: "accent",
  threads: "accent",
  facebook: "accent",
  tiktok: "accent",
  youtube: "bad",
};

function badgeTone(job: JobInfo): BadgeTone {
  if (job.stage === "error") return "bad";
  if (job.needs_auth) return "warn";
  return kindTone[job.kind] ?? "neutral";
}

type JobCardProps = {
  job: JobInfo & { cancelling?: boolean };
  highlighted?: boolean;
  selected?: boolean;
  onOpen: (id: string) => void;
  onSelect?: (id: string) => void;
  onRemove?: (id: string) => void;
  onCancel?: (id: string) => void;
  onRerun?: (job: JobInfo) => void;
  onRetryAfterSignin: (job: JobInfo, options?: { skipExport?: boolean }) => void;
  onError: (message: string) => void;
};

// A single ledger row: [type] [source + status] [contextual action + hover tools].
// The full sign-in / cookie-import / error detail lives in the inspector pane —
// the row stays quiet and scannable.
export const JobCard = memo(function JobCard({
  job,
  highlighted,
  selected,
  onOpen,
  onSelect,
  onRemove,
  onCancel,
  onRerun,
  onRetryAfterSignin,
  onError,
}: JobCardProps) {
  const ready = job.stage === "done";
  const active = isActiveStage(job.stage);
  const cancelling = Boolean(job.cancelling);
  const stoppable = (active || job.stage === "queued") && !cancelling;
  const rerunnable = ["done", "error", "cancelled"].includes(job.stage);
  const progress = Math.max(0, Math.min(100, job.pct));
  const primary = job.title ?? job.input;
  const choose = () => onSelect?.(job.id);

  const dotColor = job.needs_auth
    ? "var(--warn)"
    : ready
      ? "var(--done)"
      : job.stage === "error"
        ? "var(--warn)"
        : active
          ? "var(--accent)"
          : "var(--muted-2)";
  const statusToneClass =
    job.stage === "error" || job.needs_auth
      ? errorTextClass
      : ready
        ? "text-[var(--done)]"
        : "text-[var(--muted)]";

  const CatIcon = categoryIcon(categoryForKind(job.kind));

  return (
    <li
      data-job-card
      className={cx(
        "group grid grid-cols-[minmax(0,1fr)_auto] items-center gap-3 border-b border-[color-mix(in_srgb,var(--line)_70%,transparent)] px-3 py-2.5 transition-colors duration-150 hover:bg-[color-mix(in_srgb,var(--panel-2)_60%,var(--ink))]",
        (highlighted || selected) && "bg-[var(--panel)]",
        selected && "shadow-[inset_3px_0_0_var(--accent)]",
      )}
      title={primary}
    >
      <button
        type="button"
        className="flex min-w-0 cursor-pointer items-center gap-2 rounded-[6px] text-left focus:outline-none focus:ring-2 focus:ring-inset focus:ring-[var(--accent)]/35"
        aria-pressed={Boolean(selected)}
        aria-label={`Select ${primary} — ${outputLabelForJob(job)}`}
        onClick={choose}
      >
        {CatIcon && (
          <span className="shrink-0 text-[var(--muted)]">
            <CatIcon size={14} />
          </span>
        )}
        <Badge tone={badgeTone(job)}>{labelForJob(job)}</Badge>
        {showsOutputChip(job) && (
          <span
            className="inline-flex shrink-0 items-center rounded-[5px] border border-[var(--line)] px-2 py-[3px] font-[family-name:var(--mono)] text-[10px] text-[var(--muted)]"
            data-media-action={job.media?.action ?? "transcribe"}
            title={`Output: ${outputLabelForJob(job)}`}
          >
            {outputLabelForJob(job)}
          </span>
        )}

        <div className="min-w-0 flex-1">
          <span className="block truncate text-[13px] text-[var(--paper)]" title={primary}>
            {primary}
          </span>
          <span className="mt-1 flex min-w-0 items-center gap-2 font-[family-name:var(--mono)] text-[11px]">
            <span
              className="h-1.5 w-1.5 shrink-0 rounded-full"
              style={{ background: dotColor }}
              aria-hidden="true"
            />
            {cancelling ? (
              <span className="truncate text-[var(--muted)]">Cancelling…</span>
            ) : active && !job.needs_auth ? (
              <>
                <span
                  className="progress-shine h-1 w-12 shrink-0 overflow-hidden rounded-full bg-[color-mix(in_srgb,var(--line)_58%,transparent)]"
                  role="progressbar"
                  aria-label={`${statusText(job)} progress`}
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-valuenow={progress}
                >
                  <span
                    className="block h-full rounded-full bg-[var(--accent)] transition-[width]"
                    style={{ width: `${progress}%` }}
                  />
                </span>
                <span className="truncate text-[var(--muted)]">{statusText(job)}</span>
              </>
            ) : (
              <span className={cx("truncate", statusToneClass)}>{statusText(job)}</span>
            )}
          </span>
        </div>
      </button>

      <div className="flex shrink-0 items-center gap-1 justify-self-end">
        {job.needs_auth ? (
          <button
            type="button"
            className={cx(buttonClass, "h-7 px-2 text-xs")}
            title={`Sign in to ${job.needs_auth} — the download continues automatically`}
            aria-label={`Sign in to ${job.needs_auth}`}
            onClick={(e) => {
              e.stopPropagation();
              invoke("sign_in", { platform: job.needs_auth, url: job.input }).catch((x) =>
                onError(String(x)),
              );
            }}
          >
            <LogIn size={14} strokeWidth={2} />
            <span>Sign in</span>
          </button>
        ) : ready ? (
          <button
            type="button"
            className={cx(buttonClass, "h-7 px-2 text-xs")}
            title="Open transcript detail"
            aria-label="Open transcript detail"
            onClick={(e) => {
              e.stopPropagation();
              onOpen(job.id);
            }}
          >
            <span>View</span>
            <ArrowRight size={14} strokeWidth={1.9} />
          </button>
        ) : job.stage === "error" ? (
          <IconButton
            className={iconActionAccentClass}
            title={retryTitle(job)}
            label="Retry job"
            onClick={(e) => {
              e.stopPropagation();
              onRetryAfterSignin(job);
            }}
          >
            <RotateCcw size={14} strokeWidth={1.8} aria-hidden="true" />
          </IconButton>
        ) : null}

        {onCancel && stoppable && !job.needs_auth && (
          <IconButton
            className={iconActionDangerClass}
            title="Cancel job"
            label={`Cancel ${primary}`}
            onClick={(e) => {
              e.stopPropagation();
              onCancel(job.id);
            }}
          >
            <Square size={13} strokeWidth={2} aria-hidden="true" />
          </IconButton>
        )}
        {onRerun && rerunnable && !job.needs_auth && (
          <IconButton
            className={cx(iconActionAccentClass, revealOnRowHover)}
            title="Re-run job"
            label={`Re-run ${primary}`}
            onClick={(e) => {
              e.stopPropagation();
              onRerun(job);
            }}
          >
            <RefreshCcw size={14} strokeWidth={1.9} aria-hidden="true" />
          </IconButton>
        )}
        {onRemove && (
          <IconButton
            className={cx(iconActionDangerClass, revealOnRowHover)}
            title="Remove from queue"
            label={`Remove ${primary} from queue`}
            onClick={(e) => {
              e.stopPropagation();
              onRemove(job.id);
            }}
          >
            <X size={15} strokeWidth={1.9} aria-hidden="true" />
          </IconButton>
        )}
      </div>
    </li>
  );
});
