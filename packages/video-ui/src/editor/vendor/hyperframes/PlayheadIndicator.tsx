/*
 * Adapted from heygen-com/hyperframes (Apache-2.0),
 * packages/studio @ 1d9b3673fade7511dd13cde5c3776a0e45e47ca3.
 * Upstream file: packages/studio/src/player/components/PlayheadIndicator.tsx.
 * Modifications: inlined PLAYHEAD_HEAD_W (from ./timelineLayout); retargeted
 * the colour props at the shell tokens; removed the radial-gradient glow (the
 * shell is flat, with no gradients).
 * See src/editor/NOTICES.md for the licence text and provenance.
 */

const PLAYHEAD_HEAD_W = 14;
const HEAD_H = 9;

/**
 * Flat playhead: 1px accent line over the track height plus a small rotated
 * grab-handle head pinned sticky to the ruler row.
 */
export function PlayheadIndicator({
  color = "var(--rk-accent)",
  scrubbing = false,
  stickyHead = true,
}: {
  color?: string;
  scrubbing?: boolean;
  stickyHead?: boolean;
}) {
  return (
    <>
      <div
        aria-hidden="true"
        className="absolute bottom-0"
        style={{
          left: "50%",
          top: 1 + HEAD_H,
          width: 1,
          marginLeft: -0.5,
          background: color,
        }}
      />
      <div
        className={stickyHead ? "sticky" : "absolute"}
        style={{
          left: 0,
          top: stickyHead ? 0 : 1,
          height: stickyHead ? 0 : undefined,
        }}
      >
        <div
          style={{
            width: PLAYHEAD_HEAD_W,
            height: HEAD_H,
            borderRadius: 2,
            marginTop: 1,
            background: scrubbing ? color : "transparent",
            border: `1.5px solid ${color}`,
            boxSizing: "border-box",
            transform: "rotate(45deg)",
          }}
        />
      </div>
    </>
  );
}
