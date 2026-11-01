import mediaTypes from "../generated/media-types.json";
import type { AudioQuality, JobInfo, MediaSpec, VideoQuality } from "../contract";

/** Output choices exposed by the queue command strip. `images` saves the
 * post's image files (single image, carousel slides, profile batch) — #248. */
export type MediaMode = "transcribe" | "audio" | "video" | "images";

/** The frontend's conservative view of what an input can carry. */
type MediaInputKind = "none" | "audio" | "video" | "mixed" | "unknown" | "local";

export interface MediaInputInfo {
  kind: MediaInputKind;
  /** Known audio-only hosts, used for labels and to prevent video requests. */
  audioHost?: string;
}

const AUDIO_HOSTS: ReadonlyArray<[string, string]> = [
  ["soundcloud.com", "SoundCloud"],
  ["mixcloud.com", "Mixcloud"],
  ["bandcamp.com", "Bandcamp"],
  ["podbean.com", "Podbean"],
  ["buzzsprout.com", "Buzzsprout"],
  ["spreaker.com", "Spreaker"],
  ["libsyn.com", "Libsyn"],
  ["megaphone.fm", "Megaphone"],
  ["castbox.fm", "Castbox"],
  ["audioboom.com", "Audioboom"],
];

const VIDEO_HOSTS = [
  "youtube.com",
  "youtu.be",
  "twitter.com",
  "x.com",
  "vimeo.com",
  "twitch.tv",
  "reddit.com",
  "redd.it",
  "linkedin.com",
  "loom.com",
  "dailymotion.com",
  "dai.ly",
  "streamable.com",
  "rumble.com",
  "odysee.com",
  "nebula.tv",
  "bitchute.com",
  "facebook.com",
] as const;

const extensionPattern = (list: readonly string[]) =>
  new RegExp(`\\.(?:${list.join("|")})(?:$|[?#])`, "i");
const AUDIO_EXTENSIONS = extensionPattern(mediaTypes.audio);
const VIDEO_EXTENSIONS = extensionPattern(mediaTypes.video);

function hostForInput(input: string): string | null {
  const trimmed = input.trim();
  if (!trimmed) return null;
  const candidate = /^[a-z][a-z\d+.-]*:\/\//i.test(trimmed) ? trimmed : `https://${trimmed}`;
  try {
    const url = new URL(candidate);
    if (!/^https?:$/i.test(url.protocol)) return null;
    return url.hostname.toLowerCase().replace(/^(?:www|m)\./, "");
  } catch {
    return null;
  }
}

function hostMatches(host: string, domain: string): boolean {
  return host === domain || host.endsWith(`.${domain}`);
}

/** Sources that can return image files for "Save images" (#248): Instagram,
 * TikTok photo posts, and X. Threads returns unsupported from the engine, so
 * the action is not offered there. */
const IMAGE_HOSTS = ["instagram.com", "tiktok.com", "x.com", "twitter.com"] as const;

function knownAudioHost(host: string): string | undefined {
  return AUDIO_HOSTS.find(([domain]) => hostMatches(host, domain))?.[1];
}

/** Return media capability for one pasted input without a network request. */
export function mediaKindForInput(input: string): MediaInputInfo {
  const trimmed = input.trim();
  if (!trimmed) return { kind: "none" };
  if (/^(?:[a-z]:[\\/]|\\\\|\.{0,2}[\\/])/i.test(trimmed)) return { kind: "local" };

  const host = hostForInput(trimmed);
  if (!host) return { kind: "local" };

  const audioHost = knownAudioHost(host);
  if (audioHost) return { kind: "audio", audioHost };
  if (VIDEO_HOSTS.some((domain) => hostMatches(host, domain))) return { kind: "video" };

  // A direct file URL is unambiguous even when it lives on an unlisted host.
  if (AUDIO_EXTENSIONS.test(trimmed)) return { kind: "audio" };
  if (VIDEO_EXTENSIONS.test(trimmed)) return { kind: "video" };

  // Unknown HTTP hosts may still be supported by yt-dlp. Keep download controls
  // usable, while local/non-URL inputs remain transcription-only.
  return { kind: "unknown" };
}

/** Summarise a multiline paste for the output control's safety checks. */
export function mediaKindForInputs(inputs: string): MediaInputInfo {
  const values = inputs
    .split(/\r?\n/)
    .map((value) => value.trim())
    .filter(Boolean);
  if (!values.length) return { kind: "none" };

  const infos = values.map(mediaKindForInput);
  if (infos.some((info) => info.kind === "local")) return { kind: "local" };

  const hasAudio = infos.some((info) => info.kind === "audio");
  const hasVideo = infos.some((info) => info.kind === "video");
  if (hasAudio && hasVideo) return { kind: "mixed" };
  if (hasAudio) {
    return {
      kind: "audio",
      audioHost: infos.find((info) => info.audioHost)?.audioHost,
    };
  }
  if (hasVideo) return { kind: "video" };
  if (infos.some((info) => info.kind === "unknown")) return { kind: "unknown" };
  return { kind: "none" };
}

/** Host-aware label for queue/library badges (SoundCloud must not read Video). */
export function sourceLabelForInput(input: string): string | undefined {
  const host = hostForInput(input);
  return host ? knownAudioHost(host) : undefined;
}

/** True when one pasted input can offer "Save images" (#248). */
export function supportsImageAction(input: string): boolean {
  const host = hostForInput(input);
  return Boolean(host && IMAGE_HOSTS.some((domain) => hostMatches(host, domain)));
}

/** Path segments that name a post, feed or tool page rather than a profile. */
const NON_PROFILE_SEGMENTS = new Set([
  "p",
  "reel",
  "reels",
  "tv",
  "explore",
  "stories",
  "status",
  "photo",
  "video",
  "i",
  "accounts",
  "home",
  "search",
  "hashtag",
  "share",
  "login",
]);

/** True for a single-segment profile URL on an image-capable source
 * (instagram.com/name, tiktok.com/@name, x.com/name). Such a URL is batch-saved
 * through the engine's profile listing rather than as one post (#248). */
export function isProfileUrl(input: string): boolean {
  const trimmed = input.trim();
  const host = hostForInput(trimmed);
  if (!host || !IMAGE_HOSTS.some((domain) => hostMatches(host, domain))) return false;
  const candidate = /^[a-z][a-z\d+.-]*:\/\//i.test(trimmed) ? trimmed : `https://${trimmed}`;
  let segments: string[];
  try {
    segments = new URL(candidate).pathname.split("/").filter(Boolean);
  } catch {
    return false;
  }
  if (segments.length !== 1) return false;
  const name = segments[0].replace(/^@/, "");
  return name.length > 0 && !NON_PROFILE_SEGMENTS.has(name.toLowerCase());
}

/** "Save images" is available only when the draft has inputs and every one of
 * them is image-capable; a blank draft leaves the mode selectable. */
export function canSaveImages(inputs: string): boolean {
  const values = inputs
    .split(/\r?\n/)
    .map((value) => value.trim())
    .filter(Boolean);
  return values.length > 0 && values.every(supportsImageAction);
}

/** Split a "Save images" draft per line: image-capable links save images, the
 * rest fall back to transcription (the default output never blocks a link). */
export function splitImageLines(inputs: string): { image: string[]; other: string[] } {
  const values = inputs
    .split(/\r?\n/)
    .map((value) => value.trim())
    .filter(Boolean);
  return {
    image: values.filter(supportsImageAction),
    other: values.filter((value) => !supportsImageAction(value)),
  };
}

/** Platform named by a `[needs_auth] <platform>` error, or null. */
export function needsAuthPlatform(message: string): string | null {
  const match = /^\[needs_auth\] (.+)$/.exec(message.trim());
  return match?.[1]?.trim() || null;
}

/** Wire spec for a "Save images" submit, or undefined when any input is not
 * image-capable (the caller keeps the action disabled in that case). */
export function imageSpecForInputs(inputs: string): MediaSpec | undefined {
  return canSaveImages(inputs) ? { action: "images" } : undefined;
}

/** Label for the image kind on Library rows and queue/sidebar badges. */
export const IMAGE_KIND_LABEL = "Images";

/** Count text for saved image files: "1 image", "3 images", "0 images". */
export function imageCountLabel(count: number): string {
  return count === 1 ? "1 image" : `${count} images`;
}

/** Shape of the image-job facts the engine writes into `media_meta` (#248). */
export interface ImageJobMeta {
  count: number;
  files: string[];
}

/** Parse `media_meta` defensively. Returns null for any value that is not an
 * image-job record, so transcript and media rows never read as image jobs. */
export function imageJobMeta(meta: string | null | undefined): ImageJobMeta | null {
  if (!meta) return null;
  try {
    const parsed: unknown = JSON.parse(meta);
    if (!parsed || typeof parsed !== "object") return null;
    const record = parsed as { kind?: unknown; count?: unknown; files?: unknown };
    if (record.kind !== "images") return null;
    const files = Array.isArray(record.files)
      ? record.files.filter((f): f is string => typeof f === "string")
      : [];
    const count = typeof record.count === "number" ? record.count : files.length;
    return { count, files };
  } catch {
    return null;
  }
}

/** A history row is an image job when the engine recorded the image action
 * or its media metadata says so. */
export function isImageRow(row: {
  media_action?: string | null;
  media_meta?: string | null;
}): boolean {
  return row.media_action === "download_images" || imageJobMeta(row.media_meta) !== null;
}

/** Image count for a history row: from media_meta, else 0. */
export function imageCountForRow(row: { media_meta?: string | null }): number {
  return imageJobMeta(row.media_meta)?.count ?? 0;
}

export function mediaModeLabel(mode: MediaMode): string {
  switch (mode) {
    case "audio":
      return "Audio";
    case "video":
      return "Video + audio";
    case "images":
      return "Save images";
    default:
      return "Transcribe";
  }
}

/** Build the wire spec for a selected output. Transcription stays absent for
 * compatibility with older queue entries and non-media inputs. `withTranscript`
 * (WS-5's "+ Transcript" pill) upgrades a download request to `action: "both"`
 * so one queue job downloads the media AND writes the transcript. */
export function mediaSpecForMode(
  mode: MediaMode,
  videoQuality: VideoQuality,
  source: MediaInputInfo,
  audioQuality: AudioQuality = "best",
  withTranscript = false,
): MediaSpec | undefined {
  // "images" is built by imageSpecForInputs (it needs the raw inputs, not a
  // media-kind summary), so it never reaches this function.
  if (mode === "transcribe" || mode === "images") return undefined;
  if (source.kind === "none" || source.kind === "local") return undefined;
  if (mode === "video" && source.kind === "audio") return undefined;
  const action: MediaSpec["action"] = withTranscript ? "both" : "download";
  if (mode === "audio") return { action, audio: audioQuality };
  return { action, video: videoQuality };
}

/** Human-readable output badge for a submitted queue row. */
export function outputLabelForJob(job: Pick<JobInfo, "media">): string {
  const media = job.media;
  if (media?.action === "images") return IMAGE_KIND_LABEL;
  if (!media || media.action === "transcribe") return "Transcribe";
  if (media.video) {
    return media.action === "both" ? "Video + audio + transcript" : "Video + audio";
  }
  return media.action === "both" ? "Audio + transcript" : "Audio";
}

// Containers that always carry a video stream when the host app writes them. An
// audio request that fell back to a combined YouTube format (builds before
// `--extract-audio`) was saved as .mp4 with an "audio" label, so the file wins.
const VIDEO_CONTAINERS = new Set(mediaTypes.videoContainers);

export function savedMediaKind(
  meta: string | null | undefined,
  path: string | null | undefined,
): "audio" | "video" {
  const ext = path?.match(/\.([a-z0-9]+)$/i)?.[1]?.toLowerCase();
  if (ext && VIDEO_CONTAINERS.has(ext)) return "video";
  return savedMetaKind(meta) === "audio" ? "audio" : "video";
}

/** Top-level `kind` of a saved-media `media_meta` JSON object; undefined when absent or malformed. */
function savedMetaKind(meta: string | null | undefined): unknown {
  if (!meta) return undefined;
  try {
    const parsed: unknown = JSON.parse(meta);
    return parsed && typeof parsed === "object" ? (parsed as { kind?: unknown }).kind : undefined;
  } catch {
    return undefined;
  }
}
