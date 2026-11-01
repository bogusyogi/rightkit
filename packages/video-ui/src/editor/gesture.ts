/**
 * Pure gesture → action planning. Pointer handlers report transient deltas
 * here; on release the planned actions go through `EditSession.apply` as one
 * transaction. Nothing here mutates the canonical timeline — the returned
 * `EditAction`s are proposals the engine validates.
 */
import type { EditAction } from "./actions.js";
import {
  moveAudioClip,
  moveClip,
  moveTitle,
  trimAudioClip,
  trimClip,
  trimTitle,
} from "./actions.js";
import type { EditorClip, EditorModel } from "./model.js";

export type Edge = "in" | "out";

/** Snap `t` to the nearest grid step, or to a clip edge within `threshold` s. */
export function snapTime(t: number, gridStep: number, edges: readonly number[], threshold: number): number {
  let best = gridStep > 0 ? Math.round(t / gridStep) * gridStep : t;
  let bestDist = Math.abs(best - t);
  for (const e of edges) {
    const d = Math.abs(e - t);
    if (d <= threshold && d < bestDist) {
      best = e;
      bestDist = d;
    }
  }
  return Math.max(0, best);
}

export function clipEdges(model: EditorModel, exclude: ReadonlySet<string>): number[] {
  const out: number[] = [0];
  for (const lane of model.lanes) {
    for (const c of lane.clips) {
      if (exclude.has(c.id)) continue;
      out.push(c.start, c.start + c.duration);
    }
  }
  return out;
}

/**
 * Video tracks are gapless: a horizontal drag is a reorder, never a free
 * position. Returns the index the dragged clip occupies once removed, from the
 * lane position of `dropTime`.
 */
export function videoDropIndex(lane: { clips: EditorClip[] }, clipId: string, dropTime: number): number {
  const clips = lane.clips.filter((c) => c.id !== clipId);
  const idx = clips.findIndex((c) => dropTime < c.start + c.duration / 2);
  return idx < 0 ? clips.length : idx;
}

/** Plan a completed move gesture for one clip. */
export function planMove(model: EditorModel, clip: EditorClip, target: { time?: number; index?: number }): EditAction[] {
  const lane = model.lanes.find((l) => l.id === clip.ref.trackId);
  switch (clip.ref.kind) {
    case "video": {
      const index =
        target.index ??
        (lane && target.time != null ? videoDropIndex(lane, clip.id, target.time) : clip.ref.index);
      if (index === clip.ref.index) return [];
      return [moveClip(clip.id, index)];
    }
    case "audio": {
      if (target.time == null) return [];
      return [moveAudioClip(clip.id, target.time)];
    }
    case "title": {
      if (target.time == null) return [];
      return [moveTitle(clip.id, target.time * model.fps)];
    }
  }
}

/** Plan a completed multi-clip move: one action per clip (grouped transaction). */
export function planMoveMany(
  model: EditorModel,
  clips: readonly EditorClip[],
  deltaS: number,
): EditAction[] {
  const out: EditAction[] = [];
  for (const c of clips) {
    if (c.ref.kind === "audio") {
      out.push(...planMove(model, c, { time: Math.max(0, c.start + deltaS) }));
    } else if (c.ref.kind === "title") {
      out.push(...planMove(model, c, { time: Math.max(0, c.start + deltaS) }));
    }
    // Video clips ignore pure time deltas — gapless track; reorder via drop index.
  }
  return out;
}

/** Plan a completed edge trim. Returns [] when the trim is a no-op. */
export function planTrim(model: EditorModel, clip: EditorClip, edge: Edge, deltaS: number): EditAction[] {
  if (deltaS === 0 || clip.duration <= 0) return [];
  switch (clip.ref.kind) {
    case "video": {
      const inS = clip.ref.clip.source_in / TICKS;
      const outS = clip.ref.clip.source_out / TICKS;
      const sourceDur = sourceDuration(model, clip.ref.clip.source);
      const [lo, hi] =
        edge === "in"
          ? [Math.max(0, Math.min(inS + deltaS, outS - MIN_CLIP_S)), outS]
          : [inS, Math.min(sourceDur ?? Infinity, Math.max(outS + deltaS, inS + MIN_CLIP_S))];
      if (lo === inS && hi === outS) return [];
      return [trimClip(clip.id, lo, hi)];
    }
    case "audio": {
      const inS = clip.ref.clip.trim_in / TICKS;
      const outS = clip.ref.clip.trim_out / TICKS;
      const sourceDur = sourceDuration(model, clip.ref.clip.source);
      const [lo, hi] =
        edge === "in"
          ? [Math.max(0, Math.min(inS + deltaS, outS - MIN_CLIP_S)), outS]
          : [inS, Math.min(sourceDur ?? Infinity, Math.max(outS + deltaS, inS + MIN_CLIP_S))];
      if (lo === inS && hi === outS) return [];
      return [trimAudioClip(clip.id, lo, hi)];
    }
    case "title": {
      const startF = clip.ref.clip.start_frame;
      const frames = clip.ref.clip.frames;
      const d = Math.round(deltaS * model.fps);
      const [sf, fr] =
        edge === "in"
          ? [startF + d, frames - d]
          : [startF, frames + d];
      if (fr < 1) return [];
      if (sf === startF && fr === frames) return [];
      return [trimTitle(clip.id, sf, fr)];
    }
  }
}

const TICKS = 705_600_000;
const MIN_CLIP_S = 1 / 60;

function sourceDuration(model: EditorModel, sourceId: string): number | null {
  return model.sourceDurationS.get(sourceId) ?? null;
}
