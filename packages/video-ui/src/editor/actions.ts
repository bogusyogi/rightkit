/**
 * Wire builders for `cutright_timeline::Action` — the serde shape the engine's
 * `edit` method consumes (`{ "type": <snake_case>, ... }`). Field names and
 * units match rightkit-timeline src/action.rs exactly:
 * times are `Ticks` (705_600_000 per second), gains are `MilliDb`
 * (thousandths of a dB), title spans are integer frames.
 */
import { secondsToTicks, type TitleTemplate } from "../lib/project";

export type EditAction = Record<string, unknown> & { type: string };

export const insertClip = (
  trackId: string,
  index: number,
  clip: { id: string; source: string; source_in: number; source_out: number },
): EditAction => ({ type: "insert_clip", track_id: trackId, index, clip });

export const removeClip = (clipId: string): EditAction => ({
  type: "remove_clip",
  clip_id: clipId,
});

export const trimClip = (clipId: string, sourceInS: number, sourceOutS: number): EditAction => ({
  type: "trim_clip",
  clip_id: clipId,
  source_in: secondsToTicks(sourceInS),
  source_out: secondsToTicks(sourceOutS),
});

export const moveClip = (clipId: string, toIndex: number): EditAction => ({
  type: "move_clip",
  clip_id: clipId,
  to_index: toIndex,
});

export const moveAudioClip = (clipId: string, startS: number): EditAction => ({
  type: "move_audio_clip",
  clip_id: clipId,
  start: secondsToTicks(startS),
});

export const trimAudioClip = (clipId: string, trimInS: number, trimOutS: number): EditAction => ({
  type: "trim_audio_clip",
  clip_id: clipId,
  trim_in: secondsToTicks(trimInS),
  trim_out: secondsToTicks(trimOutS),
});

export const removeAudioClip = (clipId: string): EditAction => ({
  type: "remove_audio_clip",
  clip_id: clipId,
});

/** `gainDb` is linear-reading decibels; the model stores thousandths (MilliDb). */
export const setGain = (clipId: string, gainDb: number): EditAction => ({
  type: "set_gain",
  clip_id: clipId,
  gain: Math.round(gainDb * 1000),
});

export const linkNarration = (audioId: string, videoId: string): EditAction => ({
  type: "link_narration",
  audio_id: audioId,
  video_id: videoId,
});

export const unlinkNarration = (audioId: string): EditAction => ({
  type: "unlink_narration",
  audio_id: audioId,
});

export type UnderMode = { mode: "keep" } | { mode: "duck"; amount: number } | { mode: "mute" };

export const setUnder = (audioId: string, under: UnderMode): EditAction => ({
  type: "set_under",
  audio_id: audioId,
  under,
});

export const addTitle = (
  trackId: string,
  clip: {
    id: string;
    start_frame: number;
    frames: number;
    template: TitleTemplate;
    fade_in_frames: number;
    fade_out_frames: number;
  },
): EditAction => ({ type: "add_title", track_id: trackId, clip });

export const removeTitle = (clipId: string): EditAction => ({ type: "remove_title", clip_id: clipId });

export const moveTitle = (clipId: string, startFrame: number): EditAction => ({
  type: "move_title",
  clip_id: clipId,
  start_frame: Math.round(startFrame),
});

export const trimTitle = (clipId: string, startFrame: number, frames: number): EditAction => ({
  type: "trim_title",
  clip_id: clipId,
  start_frame: Math.round(startFrame),
  frames: Math.round(frames),
});

export const editTitle = (
  clipId: string,
  template: TitleTemplate,
  fadeInFrames: number,
  fadeOutFrames: number,
): EditAction => ({
  type: "edit_title",
  clip_id: clipId,
  template,
  fade_in_frames: Math.round(fadeInFrames),
  fade_out_frames: Math.round(fadeOutFrames),
});

/** `atS` is seconds INTO the clip's own source range (not output time). */
export const splitClip = (clipId: string, atS: number, newClipId: string): EditAction => ({
  type: "split_clip",
  clip_id: clipId,
  at: secondsToTicks(atS),
  new_clip_id: newClipId,
});

/** `atS` is seconds into the audio clip's trimmed duration. */
export const splitAudioClip = (clipId: string, atS: number, newClipId: string): EditAction => ({
  type: "split_audio_clip",
  clip_id: clipId,
  at: secondsToTicks(atS),
  new_clip_id: newClipId,
});

/** Shifts a video clip's source window by `deltaS`; output span unchanged. */
export const slipClip = (clipId: string, deltaS: number): EditAction => ({
  type: "slip_clip",
  clip_id: clipId,
  delta: secondsToTicks(deltaS),
});

/** Shifts an audio clip's trim window by `deltaS`; start/duration unchanged. */
export const slipAudioClip = (clipId: string, deltaS: number): EditAction => ({
  type: "slip_audio_clip",
  clip_id: clipId,
  delta: secondsToTicks(deltaS),
});

/** Excises the output range [startS, endS) on every video track it covers. */
export const rippleDeleteRange = (startS: number, endS: number): EditAction => ({
  type: "ripple_delete_range",
  start: secondsToTicks(startS),
  end: secondsToTicks(endS),
});

export const setMute = (clipId: string, muted: boolean): EditAction => ({ type: "set_mute", clip_id: clipId, muted });
export const positionTitle = (clipId: string, x: number, y: number): EditAction => ({
  type: "position_title", clip_id: clipId, position: { x: Math.round(x), y: Math.round(y) },
});
export const swapTake = (clipId: string, source: string, inS: number, outS: number): EditAction => ({
  type: "swap_take", clip_id: clipId, source, source_in: secondsToTicks(inS), source_out: secondsToTicks(outS),
});
export const editCaption = (cueId: string, correction: import("./model").CaptionCorrection): EditAction => ({
  type: "edit_caption", cue_id: cueId, correction: {
    text: correction.text, start_frame: correction.start_frame, frames: correction.frames, position: correction.position,
  },
});
export const removeCaption = (cueId: string): EditAction => ({ type: "remove_caption", cue_id: cueId });
