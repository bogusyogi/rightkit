import { engineCall } from "./engine";

/** Timeline ticks per second (cutright-timeline TICKS_PER_SECOND). */
export const TICKS_PER_SECOND = 705_600_000;

export const secondsToTicks = (seconds: number) => Math.round(seconds * TICKS_PER_SECOND);
export const ticksToSeconds = (ticks: number) => ticks / TICKS_PER_SECOND;

export type SourceInfo = {
  id: string;
  duration: number;
  video: { width: number; height: number } | null;
  has_audio: boolean;
};

export type VideoClip = { id: string; source: string; source_in: number; source_out: number };

export type AudioClip = {
  id: string;
  source: string;
  trim_in: number;
  trim_out: number;
  start: number;
  /** MilliDb — thousandths of a dB. */
  gain: number;
  link: string | null;
  /** UnderAudio: what happens to the video audio under this clip. */
  under: { mode: "keep" } | { mode: "duck"; amount: number } | { mode: "mute" };
};

/** `TitleTemplate` serde form: internally tagged by `template`, text inside the template. */
export type TitleTemplate =
  | { template: "lower_third"; title: string; subtitle?: string }
  | { template: "centered"; text: string }
  | { template: "caption"; text: string };

export type OverlayClip = {
  id: string;
  start_frame: number;
  frames: number;
  template: TitleTemplate;
  fade_in_frames?: number;
  fade_out_frames?: number;
};

export const titleLabel = (t: TitleTemplate): string => (t.template === "lower_third" ? t.title : t.text);

export type Timeline = {
  fps: { num: number; den: number };
  sources: SourceInfo[];
  video_tracks: { id: string; clips: VideoClip[] }[];
  audio_tracks: { id: string; role: string; clips: AudioClip[] }[];
  overlay_tracks: { id: string; clips: OverlayClip[] }[];
};

/** `project.show` result. */
export type ProjectShow = {
  project: { id: string; name: string | null; dir: string; fps: { num: number; den: number } };
  revision: { cursor: number; total: number; can_undo: boolean; can_redo: boolean };
  summary: {
    total_frames: number;
    duration_ms: number;
    sources: number;
    video_clips: number;
    audio_clips: number;
  };
  sources: { info: SourceInfo; name: string | null; mode: string | null }[];
  layout: { track: string; clip: string; start_frame: number; frames: number }[];
  timeline: Timeline;
  /** Engine extension ids the editor may open capability gates for. */
  extensions?: string[];
  /** Sources carrying a validated transcript under analysis/ (word-edge evidence). */
  transcripts?: { source: string }[];
};

export const projectShow = (dir: string) => engineCall<ProjectShow>("project.show", { dir });

/**
 * Appends `source` [startMs, endMs) to the end of `track`. The range is clamped to
 * the source's real duration: word timings can run a few ms past the media's end,
 * and the engine rejects out-of-range indices and source ranges.
 */
export async function appendClip(dir: string, source: string, startMs: number, endMs: number, track = "V1") {
  const show = await projectShow(dir);
  const duration = show.timeline.sources.find((s) => s.id === source)?.duration ?? Infinity;
  const index = show.timeline.video_tracks.find((t) => t.id === track)?.clips.length ?? 0;
  return engineCall("edit", {
    dir,
    action: {
      type: "insert_clip",
      track_id: track,
      index,
      clip: {
        id: newClipId(),
        source,
        source_in: Math.max(0, secondsToTicks(startMs / 1000)),
        source_out: Math.min(duration, secondsToTicks(endMs / 1000)),
      },
    },
  });
}

export const projectInit = (dir: string, name?: string) =>
  engineCall<unknown>("project.init", name ? { dir, name } : { dir });

export const fpsValue = (fps: { num: number; den: number }) => fps.num / fps.den;

export function formatSeconds(seconds: number): string {
  if (!Number.isFinite(seconds)) return "-";
  const m = Math.floor(seconds / 60);
  const s = seconds - m * 60;
  return m > 0 ? `${m}:${s.toFixed(1).padStart(4, "0")}` : `${s.toFixed(1)}s`;
}

export function baseName(path: string): string {
  const parts = path.split(/[\\/]+/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

/** Collision-resistant clip id for new timeline clips. */
export function newClipId(): string {
  return `clip-${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

