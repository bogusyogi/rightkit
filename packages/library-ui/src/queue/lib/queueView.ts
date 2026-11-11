import type { JobInfo, Kind } from "../contract.js";
import { categoryForKind, isActiveStage } from "./jobDisplay.js";

export type ChipCategory = "media" | "social" | "document" | "web";

/** True while a job is actively working (not queued, finished, failed or cancelled). */
export function isRunningStage(job: Pick<JobInfo, "stage">): boolean {
  return isActiveStage(job.stage);
}

export interface QueueChipCounts {
  running: number;
  ready: number;
  failed: number;
  needs: number;
  categories: Record<ChipCategory, number>;
  kinds: Partial<Record<Kind, number>>;
}

/** Every filter-chip count from one pass over the jobs (issue #224). */
export function countQueueChips(jobs: readonly JobInfo[]): QueueChipCounts {
  const counts: QueueChipCounts = {
    running: 0,
    ready: 0,
    failed: 0,
    needs: 0,
    categories: { media: 0, social: 0, document: 0, web: 0 },
    kinds: {},
  };
  for (const job of jobs) {
    if (isRunningStage(job)) counts.running += 1;
    if (job.stage === "done") counts.ready += 1;
    if (job.stage === "error") counts.failed += 1;
    if (job.needs_auth) counts.needs += 1;
    const category = categoryForKind(job.kind);
    if (category !== "other") counts.categories[category] += 1;
    counts.kinds[job.kind] = (counts.kinds[job.kind] ?? 0) + 1;
  }
  return counts;
}

/**
 * The output chip is shown only for non-default outputs. A plain transcribe
 * request (no media spec, or action "transcribe") drops the chip so the
 * title keeps its width (issue #227).
 */
export function showsOutputChip(job: Pick<JobInfo, "media">): boolean {
  return Boolean(job.media) && job.media?.action !== "transcribe";
}

