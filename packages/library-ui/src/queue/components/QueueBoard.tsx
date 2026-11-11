import { memo, type ReactNode, useMemo, useState } from "react";
import type { JobInfo, Kind } from "../contract.js";
import { categoryForKind } from "../lib/jobDisplay.js";
import { countQueueChips, isRunningStage } from "../lib/queueView.js";
import { buttonClass, panelClass } from "../../ui.js";
import { EmptyState, cx } from "@rightkit/react-shell/react";
import { JobCard } from "./JobCard.js";
import { CircleSlash, RotateCcw, Trash2 } from "lucide-react";
import { categoryIcon } from "./CategoryIcon.js";
import { categoryToneClass } from "../lib/categoryDisplay.js";

type FilterKey = "all" | "running" | "ready" | "needs";
type CategoryFilter = "all" | "media" | "social" | "document" | "web";

type QueueBoardProps = {
  jobs: JobInfo[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  onOpen: (id: string) => void;
  onRemove: (id: string) => void;
  onCancel: (id: string) => void;
  onRerun: (job: JobInfo) => void;
  onClearCompleted: () => void;
  onClearAll: () => void;
  onRetryAllFailed: () => void;
  onRetryAfterSignin: (job: JobInfo, options?: { skipExport?: boolean }) => void;
  onError: (message: string) => void;
  preview: ReactNode;
};

export function QueueBoard({
  jobs,
  selectedId,
  onSelect,
  onOpen,
  onRemove,
  onCancel,
  onRerun,
  onClearCompleted,
  onClearAll,
  onRetryAllFailed,
  onRetryAfterSignin,
  onError,
  preview,
}: QueueBoardProps) {
  const [filter, setFilter] = useState<FilterKey>("all");
  const [kindFilter, setKindFilter] = useState<Kind | "all">("all");
  const [catFilter, setCatFilter] = useState<CategoryFilter>("all");

  // One pass over the jobs for every chip count (issue #224).
  const counts = useMemo(() => countQueueChips(jobs), [jobs]);
  const { failed, ready, running, needs } = counts;
  const selectKind = (kind: Kind) => {
    setCatFilter("all");
    setKindFilter((current) => (current === kind ? "all" : kind));
  };

  const visible = useMemo(() => {
    let result = jobs;
    if (filter === "running") result = result.filter(isRunningStage);
    if (filter === "ready") result = result.filter((j) => j.stage === "done");
    if (filter === "needs") result = result.filter((j) => Boolean(j.needs_auth));
    if (catFilter !== "all") result = result.filter((j) => categoryForKind(j.kind) === catFilter);
    if (kindFilter !== "all") result = result.filter((j) => j.kind === kindFilter);
    return result;
  }, [jobs, filter, kindFilter, catFilter]);

  const grouped = useMemo(() => {
    if (kindFilter !== "all" || catFilter !== "all") return null;
    const byCat: Record<string, JobInfo[]> = {};
    for (const job of visible) {
      const cat = categoryForKind(job.kind);
      byCat[cat] ??= [];
      byCat[cat].push(job);
    }
    return Object.entries(byCat);
  }, [visible, kindFilter, catFilter]);

  return (
    <>
      <div className="flex flex-wrap items-center gap-2">
        <FilterChip
          label="All"
          count={jobs.length}
          active={filter === "all"}
          showWhenEmpty
          onClick={() => setFilter("all")}
        />
        <FilterChip
          label="Running"
          count={running}
          active={filter === "running"}
          onClick={() => setFilter("running")}
        />
        <FilterChip
          label="Ready"
          count={ready}
          active={filter === "ready"}
          onClick={() => setFilter("ready")}
        />
        <FilterChip
          label="Needs you"
          count={needs}
          active={filter === "needs"}
          onClick={() => setFilter("needs")}
        />

        <FilterChip
          label="Media"
          count={counts.categories.media}
          active={catFilter === "media"}
          onClick={() => {
            const next = catFilter === "media" ? "all" : "media";
            setCatFilter(next);
            setKindFilter("all");
          }}
        />
        <FilterChip
          label="Local"
          count={(counts.kinds.local_file ?? 0)}
          active={kindFilter === "local_file"}
          onClick={() => selectKind("local_file")}
        />
        <FilterChip
          label="Social"
          count={counts.categories.social}
          active={catFilter === "social"}
          onClick={() => {
            const next = catFilter === "social" ? "all" : "social";
            setCatFilter(next);
            setKindFilter("all");
          }}
        />
        <FilterChip
          label="Document"
          count={counts.categories.document}
          active={catFilter === "document"}
          onClick={() => {
            const next = catFilter === "document" ? "all" : "document";
            setCatFilter(next);
            setKindFilter("all");
          }}
        />
        <FilterChip
          label="Web"
          count={counts.categories.web}
          active={catFilter === "web"}
          onClick={() => {
            const next = catFilter === "web" ? "all" : "web";
            setCatFilter(next);
            setKindFilter("all");
          }}
        />

        {catFilter === "all" && (
          <>
            <FilterChip
              label="Reel"
              count={(counts.kinds.reel ?? 0)}
              active={kindFilter === "reel"}
              onClick={() => selectKind("reel")}
            />
            <FilterChip
              label="Post"
              count={(counts.kinds.instagram_post ?? 0)}
              active={kindFilter === "instagram_post"}
              onClick={() => selectKind("instagram_post")}
            />
            <FilterChip
              label="Carousel"
              count={(counts.kinds.carousel ?? 0)}
              active={kindFilter === "carousel"}
              onClick={() => selectKind("carousel")}
            />
            <FilterChip
              label="YouTube"
              count={(counts.kinds.youtube ?? 0)}
              active={kindFilter === "youtube"}
              onClick={() => selectKind("youtube")}
            />
            <FilterChip
              label="TikTok"
              count={(counts.kinds.tiktok ?? 0)}
              active={kindFilter === "tiktok"}
              onClick={() => selectKind("tiktok")}
            />
            <FilterChip
              label="Article"
              count={(counts.kinds.article ?? 0)}
              active={kindFilter === "article"}
              onClick={() => selectKind("article")}
            />
          </>
        )}
        {(kindFilter !== "all" || catFilter !== "all") && (
          <button
            type="button"
            className={cx(buttonClass, "h-8 px-2.5 text-xs")}
            onClick={() => {
              setKindFilter("all");
              setCatFilter("all");
            }}
          >
            Clear filters
          </button>
        )}

        {jobs.length > 0 && (
          <div className="ml-auto flex flex-wrap items-center gap-1.5">
            {failed > 0 && (
              <button
                type="button"
                className={cx(buttonClass, "h-8 px-2.5 text-xs")}
                onClick={onRetryAllFailed}
                title="Re-run all failed jobs (skips sign-in required)"
                aria-label="Retry all failed"
              >
                <RotateCcw size={13} strokeWidth={1.8} />
                <span>Retry failed</span>
              </button>
            )}
            {ready > 0 && (
              <button
                type="button"
                className={cx(buttonClass, "h-8 px-2.5 text-xs")}
                onClick={onClearCompleted}
                title="Remove finished jobs from the queue"
                aria-label="Clear completed"
              >
                <CircleSlash size={13} strokeWidth={1.8} />
                <span>Clear done</span>
              </button>
            )}
            <button
              type="button"
              className={cx(buttonClass, "h-8 px-2.5 text-xs")}
              onClick={onClearAll}
              title="Remove every job from the queue"
              aria-label="Clear all"
            >
              <Trash2 size={13} strokeWidth={1.8} />
              <span>Clear all</span>
            </button>
          </div>
        )}
      </div>

      <div className="grid min-h-0 grid-cols-[minmax(320px,460px)_minmax(0,1fr)] gap-3 overflow-hidden">
        <section
          className={cx(panelClass, "flex min-h-0 flex-col overflow-hidden")}
          aria-label="Capture queue"
        >
          <ul
            className="m-0 min-h-0 flex-1 list-none overflow-auto p-0"
            aria-label="Jobs"
          >
            {jobs.length === 0 && (
              <li className="p-3">
                <EmptyState title="Paste a YouTube, Instagram, or TikTok link, drop a file, or paste a screenshot." />
              </li>
            )}
            {jobs.length > 0 && visible.length === 0 && (
              <li className="px-3 py-6 text-center font-[family-name:var(--mono)] text-xs text-[var(--muted)]">
                Nothing in this view.
              </li>
            )}
            {grouped
              ? grouped.map(([cat, catJobs]) => {
                  const CatIcon = categoryIcon(cat);
                  return (
                    <li key={cat} className="mb-3 last:mb-0">
                      <div
                        className={cx(
                          "mb-1 flex items-center gap-2 border-b px-3 py-1 text-[11px] font-semibold uppercase tracking-[1px]",
                          categoryToneClass(cat),
                        )}
                      >
                        {CatIcon && <CatIcon size={13} />}
                        <span>{cat}</span>
                        <span className="font-mono text-[10px]">({catJobs.length})</span>
                      </div>
                      <ul className="m-0 list-none p-0">
                        {catJobs.map((job, index) => (
                          <JobCard
                            key={job.id}
                            job={job}
                            highlighted={false}
                            selected={job.id === selectedId}
                            onSelect={onSelect}
                            onOpen={onOpen}
                            onRemove={onRemove}
                            onCancel={onCancel}
                            onRerun={onRerun}
                            onRetryAfterSignin={onRetryAfterSignin}
                            onError={onError}
                          />
                        ))}
                      </ul>
                    </li>
                  );
                })
              : visible.map((job, index) => (
                  <JobCard
                    key={job.id}
                    job={job}
                    highlighted={false}
                    selected={job.id === selectedId}
                    onSelect={onSelect}
                    onOpen={onOpen}
                    onRemove={onRemove}
                    onCancel={onCancel}
                    onRerun={onRerun}
                    onRetryAfterSignin={onRetryAfterSignin}
                    onError={onError}
                  />
                ))}
          </ul>
        </section>
        {preview}
      </div>
    </>
  );
}

const FilterChip = memo(function FilterChip({
  label,
  count,
  active,
  showWhenEmpty,
  onClick,
}: {
  label: string;
  count: number;
  active: boolean;
  showWhenEmpty?: boolean;
  onClick: () => void;
}) {
  if (count === 0 && !active && !showWhenEmpty) return null;

  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      className={cx(
        "inline-flex items-center gap-1.5 rounded-full border px-3 py-1 font-[family-name:var(--mono)] text-[11px] transition-colors",
        active
          ? "border-[var(--line-2)] bg-[var(--panel)] text-[var(--paper)]"
          : "border-[var(--line)] text-[var(--muted)] hover:text-[var(--paper)]",
      )}
    >
      <span>{label}</span>
      <span className="text-[var(--muted-2)]">{count}</span>
    </button>
  );
});
