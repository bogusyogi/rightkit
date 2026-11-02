/**
 * Source↔output time mapping for transcript-driven editing. Mirrors
 * `clip_ranges` in crates/cutright-engine/src/media_cmd.rs: every placed clip
 * range (video gapless accumulation, audio explicit starts) maps a source-ms
 * window onto output seconds. Word-edge evidence stays in the engine's
 * validated `transcript.show` payload — this module only maps numbers.
 */
import type { ProjectShow } from "../lib/project.js";
import { fpsValue, ticksToSeconds } from "../lib/project.js";

/** One placed source range: source [inS,outS) lands at outOffS + (t - inS). */
export type ClipRange = { inS: number; outS: number; outOffS: number };

/** source id → placed ranges, in output order (same walk as the engine). */
export function clipRanges(show: ProjectShow): Map<string, ClipRange[]> {
  const tl = show.timeline;
  const fps = fpsValue(tl.fps);
  const ranges = new Map<string, ClipRange[]>();
  const push = (source: string, r: ClipRange) => {
    const list = ranges.get(source) ?? [];
    list.push(r);
    ranges.set(source, list);
  };
  const placements = new Map(show.layout.map((p) => [p.clip, p]));
  for (const track of tl.video_tracks) {
    for (const clip of track.clips) {
      const p = placements.get(clip.id);
      const inS = ticksToSeconds(clip.source_in);
      const outS = ticksToSeconds(clip.source_out);
      if (!p || outS <= inS) continue;
      push(clip.source, { inS, outS, outOffS: p.start_frame / fps });
    }
  }
  for (const track of tl.audio_tracks) {
    for (const clip of track.clips) {
      const inS = ticksToSeconds(clip.trim_in);
      const outS = ticksToSeconds(clip.trim_out);
      if (outS <= inS) continue;
      push(clip.source, { inS, outS, outOffS: ticksToSeconds(clip.start) });
    }
  }
  for (const list of ranges.values()) list.sort((a, b) => a.outOffS - b.outOffS);
  return ranges;
}

/** Output seconds for a source timestamp in ms, or null when unplaced. */
export function sourceToOutput(ranges: readonly ClipRange[], sourceMs: number): number | null {
  const s = sourceMs / 1000;
  const r = ranges.find((r) => s >= r.inS && s < r.outS);
  return r ? r.outOffS + (s - r.inS) : null;
}

/**
 * Output ranges (seconds) covered by a source window [startMs, endMs). A word
 * or span can be placed more than once; every covered piece is returned so a
 * ripple-delete excises all of them.
 */
export function coveredOutputRanges(
  ranges: readonly ClipRange[],
  startMs: number,
  endMs: number,
): { startS: number; endS: number }[] {
  const out: { startS: number; endS: number }[] = [];
  const a = startMs / 1000;
  const b = endMs / 1000;
  for (const r of ranges) {
    const lo = Math.max(a, r.inS);
    const hi = Math.min(b, r.outS);
    if (hi <= lo) continue;
    out.push({ startS: r.outOffS + (lo - r.inS), endS: r.outOffS + (hi - r.inS) });
  }
  return out;
}
