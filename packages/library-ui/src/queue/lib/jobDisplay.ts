import type { DisplayJob, ErrorKind, JobInfo, Kind, Stage } from "../contract";
import { outputLabelForJob, sourceLabelForInput } from "./mediaSelection";

const KIND_LABEL: Record<Kind, string> = {
  reel: "Reel",
  instagram_post: "Post",
  carousel: "Carousel",
  youtube: "YouTube",
  tiktok: "TikTok",
  threads: "Threads",
  facebook: "Facebook",
  video_url: "Video",
  media_url: "Audio",
  article: "Article",
  local_file: "File",
  unknown: "Unknown",
};

export function labelForKind(kind: Kind): string {
  return KIND_LABEL[kind];
}

const STATUS: Record<Stage, string> = {
  queued: "Queued",
  detecting: "Detecting",
  downloading: "Downloading",
  fetched: "Fetched",
  transcribing: "Transcribing",
  writing: "Saving",
  done: "Ready",
  error: "Failed",
  cancelled: "Cancelled",
};

const ERROR_STATUS: Record<ErrorKind, string> = {
  needs_auth: "Needs sign-in",
  blocked: "Blocked",
  rate_limited: "Rate limited",
  no_media: "No media",
  no_text_layer: "No text layer",
  unsupported: "Unsupported",
  timeout: "Timed out",
  cancelled: "Cancelled",
  other: "Failed",
};

export function statusText(job: DisplayJob): string {
  if (job.stage === "error") {
    return job.error_kind ? ERROR_STATUS[job.error_kind] : STATUS.error;
  }
  if (job.stage === "done" || job.stage === "cancelled") {
    return STATUS[job.stage];
  }

  // Keep media work legible even when the engine only reports a terse phase
  // message. The UI names user outcomes, not the downloader implementation.
  if (job.stage === "downloading" && job.media?.video) {
    return withProgress("Downloading video + audio", job.pct);
  }
  if (job.stage === "downloading" && job.media?.audio) {
    return withProgress("Downloading audio", job.pct);
  }
  if (job.stage === "transcribing") return withProgress(STATUS.transcribing, job.pct);
  if (job.stage === "writing") return withProgress(STATUS.writing, job.pct);
  if (job.msg) return userFacingMessage(job.msg);
  return withProgress(STATUS[job.stage], job.pct);
}

function withProgress(label: string, pct: number): string {
  return pct > 0 ? `${label} ${pct}%` : label;
}

function userFacingMessage(message: string): string {
  const cleaned = message.replace(/\byt-dlp\b/gi, "media downloader").trim();
  if (/^download\s+\d/i.test(cleaned)) {
    return `Downloading ${cleaned.slice("download ".length)}`;
  }
  switch (cleaned.toLowerCase()) {
    case "downloading":
      return "Downloading";
    case "merging":
      return "Merging";
    case "transcribing":
      return "Transcribing";
    case "saving":
      return "Saving";
    default:
      return cleaned;
  }
}

const IMG_RE = /\.(png|jpe?g|webp|gif|bmp|tiff?)$/i;

export function labelForSource(kind: Kind, input: string): string {
  if (kind === "local_file" && IMG_RE.test(input)) return "Image";
  if (kind === "local_file" && /\.pdf$/i.test(input)) return "PDF";
  const sourceLabel = sourceLabelForInput(input);
  if (sourceLabel) return sourceLabel;
  return labelForKind(kind);
}

export function labelForJob(job: JobInfo): string {
  return labelForSource(job.kind, job.input);
}

export { outputLabelForJob };

/** Hover text for a failed job's retry control: sign-in only when the job needs it. */
export function retryTitle(job: { needs_auth?: string | null }): string {
  return job.needs_auth ? "Retry after sign-in" : "Retry job";
}

export type JobCategory = "media" | "social" | "document" | "web" | "other";

// Stages where a job is in flight. Settled stages are done, error and cancelled;
// `queued` waits for the engine and is active only when the caller opts in.
export function isActiveStage(stage: Stage, options: { includeQueued?: boolean } = {}): boolean {
  const settled = options.includeQueued ? ["done", "error", "cancelled"] : ["done", "error", "cancelled", "queued"];
  return !settled.includes(stage);
}

export function categoryForKind(kind: Kind): "media" | "social" | "document" | "web" | "other" {
  switch (kind) {
    case "youtube":
    case "tiktok":
    case "reel":
    case "video_url":
    case "media_url":
    case "facebook":
      return "media";
    case "instagram_post":
    case "carousel":
    case "threads":
      return "social";
    case "local_file":
      return "document";
    case "article":
      return "web";
    default:
      return "other";
  }
}
