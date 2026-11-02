/**
 * Read model: folds `project.show` (`ProjectShow`) into flat editor lanes with
 * second-based geometry. Rust owns canonical ticks/frames — this model only
 * translates for display and gesture maths, and converts back on commit.
 */
import {
  type AudioClip,
  type OverlayClip,
  type ProjectShow,
  type VideoClip,
  fpsValue,
  ticksToSeconds,
  titleLabel,
} from "../lib/project.js";

export type LaneKind = "video" | "audio" | "overlay";

export type EditorClipRef =
  | { kind: "video"; clip: VideoClip; trackId: string; index: number }
  | { kind: "audio"; clip: AudioClip; trackId: string; role: string }
  | { kind: "title"; clip: OverlayClip; trackId: string };

export type EditorClip = {
  id: string;
  kind: LaneKind;
  /** Timeline start in seconds. */
  start: number;
  /** Timeline duration in seconds. */
  duration: number;
  label: string;
  ref: EditorClipRef;
  /** Audio clip linked narration target (video clip id). */
  link?: string | null;
};

export type EditorLane = {
  id: string;
  kind: LaneKind;
  /** e.g. "V1", "Narr", "Music", "Titles". */
  label: string;
  clips: EditorClip[];
};

export type TextPosition = { x: number; y: number };
export type CaptionCorrection = { text: string; start_frame: number; frames: number; position: TextPosition };
export type EditorCaption = CaptionCorrection & { id: string; original_text: string; corrected?: boolean };
export type EditorCorrections = {
  captions: Record<string, CaptionCorrection>;
  positions: Record<string, TextPosition>;
  audio: Record<string, { gain: number; muted: boolean }>;
};
export type CorrectionShow = ProjectShow & {
  editor_captions?: EditorCaption[];
  timeline: ProjectShow["timeline"] & { corrections?: EditorCorrections };
};

export type EditorModel = {
  corrections: EditorCorrections;
  captions: EditorCaption[];
  sources: ProjectShow["sources"];
  fps: number;
  durationS: number;
  revisionCursor: number;
  revisionTotal: number;
  canUndo: boolean;
  canRedo: boolean;
  lanes: EditorLane[];
  clipCount: number;
  /** Source id → duration in seconds (from registered source info ticks). */
  sourceDurationS: ReadonlyMap<string, number>;
};

export function buildEditorModel(show: ProjectShow): EditorModel {
  const corrected = show as CorrectionShow;
  const tl = corrected.timeline;
  const fps = fpsValue(tl.fps);
  const sourceName = (id: string) =>
    show.sources.find((s) => s.info.id === id)?.name ?? id.slice(0, 8);
  const placements = new Map(show.layout.map((p) => [p.clip, p]));

  const lanes: EditorLane[] = [];

  for (const track of tl.video_tracks) {
    lanes.push({
      id: track.id,
      kind: "video",
      label: track.id,
      clips: track.clips.map((c, index) => {
        const p = placements.get(c.id);
        return {
          id: c.id,
          kind: "video" as const,
          start: (p?.start_frame ?? 0) / fps,
          duration: (p?.frames ?? 0) / fps,
          label: sourceName(c.source),
          ref: { kind: "video" as const, clip: c, trackId: track.id, index },
        };
      }),
    });
  }

  for (const track of tl.audio_tracks) {
    lanes.push({
      id: track.id,
      kind: "audio",
      label: track.role === "narration" ? "Narr" : track.role,
      clips: track.clips.map((c) => ({
        id: c.id,
        kind: "audio" as const,
        start: ticksToSeconds(c.start),
        duration: ticksToSeconds(c.trim_out - c.trim_in),
        label: sourceName(c.source),
        link: c.link,
        ref: { kind: "audio" as const, clip: c, trackId: track.id, role: track.role },
      })),
    });
  }

  for (const track of tl.overlay_tracks) {
    lanes.push({
      id: track.id,
      kind: "overlay",
      label: track.id,
      clips: track.clips.map((c) => ({
        id: c.id,
        kind: "overlay" as const,
        start: c.start_frame / fps,
        duration: c.frames / fps,
        label: titleLabel(c.template),
        ref: { kind: "title" as const, clip: c, trackId: track.id },
      })),
    });
  }

  return {
    corrections: tl.corrections ?? { captions: {}, positions: {}, audio: {} },
    captions: corrected.editor_captions ?? [],
    sources: show.sources,
    fps,
    durationS: Math.max(show.summary.duration_ms / 1000, 0),
    revisionCursor: show.revision.cursor,
    revisionTotal: show.revision.total,
    canUndo: show.revision.can_undo,
    canRedo: show.revision.can_redo,
    lanes,
    clipCount: show.summary.video_clips + show.summary.audio_clips,
    sourceDurationS: new Map(
      show.sources.map((s) => [s.info.id, ticksToSeconds(s.info.duration)]),
    ),
  };
}

export function findClip(model: EditorModel, id: string): EditorClip | null {
  for (const lane of model.lanes) {
    const hit = lane.clips.find((c) => c.id === id);
    if (hit) return hit;
  }
  return null;
}

// Compatibility alias for consumers importing the shorter name.
export const buildModel = buildEditorModel;
