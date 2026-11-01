import { useRef } from "react";
import { MediaPlayer } from "@rightkit/media-ui";
import { mediaKind, useMediaPlaybackId } from "../lib/mediaAdapters";

/** @rightkit/media-ui player confined to [startMs, endMs] of a real file: starts
 *  at the range start and either loops it (shorts) or stops at its end (phrases). */
export function RangePreview({
  path,
  startMs,
  endMs,
  loop,
  title,
}: {
  path: string;
  startMs: number;
  endMs: number;
  loop?: boolean;
  title?: string;
}) {
  const el = useRef<HTMLMediaElement | null>(null);
  const playbackId = useMediaPlaybackId(path);
  return (
    <MediaPlayer
      key={`${path}:${startMs}:${playbackId ?? ""}`}
      src={path}
      playbackId={playbackId}
      kind={mediaKind(path)}
      title={title}
      autoPlay
      compact
      mediaRef={el}
      onReady={() => {
        if (el.current) el.current.currentTime = startMs / 1000;
      }}
      onTimeChange={(s) => {
        const m = el.current;
        if (!m || s * 1000 < endMs) return;
        if (loop) m.currentTime = startMs / 1000;
        else m.pause();
      }}
    />
  );
}
