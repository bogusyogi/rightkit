import { useState, type ReactNode, type SyntheticEvent } from "react";
import { useMediaAdapters, type MediaAdapters } from "./adapters.js";

export interface MediaThumbnailLabels {
  audio: string;
  video: string;
  unavailable: string;
}

export const DEFAULT_MEDIA_THUMBNAIL_LABELS: MediaThumbnailLabels = {
  audio: "Audio",
  video: "Video",
  unavailable: "Preview unavailable",
};

export interface MediaThumbnailProps {
  /** Media source, resolved through `adapters.resolveSrc` as MediaPlayer resolves its own. */
  src: string;
  kind: "audio" | "video";
  /** Still image shown instead of a decoded frame (video) or as audio artwork. */
  poster?: string;
  /** Seconds into a video whose frame is shown. Default 0. Ignored when `poster` is set. */
  frameAt?: number;
  /** Duration text on the tile, for example "3:24". Omitted when unset. */
  durationLabel?: string;
  /** Accessible name of the tile, usually the title. */
  label?: string;
  adapters?: MediaAdapters;
  labels?: Partial<MediaThumbnailLabels>;
  className?: string;
}

const AUDIO_GLYPH = (
  <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true">
    <path d="M9 18V5l11-2v13" strokeLinecap="round" strokeLinejoin="round" />
    <circle cx="6" cy="18" r="3" />
    <circle cx="17" cy="16" r="3" />
  </svg>
);

/**
 * Non-interactive tile preview of a video frame or an audio item, for galleries and lists. Renders no controls and
 * takes no focus: it is a picture of the media, and the tile around it is the interactive element. A video shows
 * its own frame (muted, metadata only, no playback); an audio item shows its poster or a glyph.
 */
export function MediaThumbnail(props: MediaThumbnailProps) {
  const { src, kind, poster, frameAt = 0, durationLabel, label, className } = props;
  const adapters = useMediaAdapters(props.adapters);
  const labels: MediaThumbnailLabels = { ...DEFAULT_MEDIA_THUMBNAIL_LABELS, ...props.labels };
  const [failed, setFailed] = useState(false);
  const resolve = (path: string) => adapters.resolveSrc?.(path) ?? path;
  const name = label ?? (kind === "audio" ? labels.audio : labels.video);

  const seekToFrame = (event: SyntheticEvent<HTMLVideoElement>): void => {
    const video = event.currentTarget;
    if (!Number.isFinite(video.duration) || video.duration <= 0 || frameAt <= 0) return;
    video.currentTime = Math.min(frameAt, Math.max(0, video.duration - 0.05));
  };

  let media: ReactNode;
  if (failed) {
    media = <span className="rk-media-thumb-fallback">{labels.unavailable}</span>;
  } else if (kind === "video" && !poster) {
    media = (
      <video
        className="rk-media-thumb-media"
        src={resolve(src)}
        muted
        playsInline
        preload="metadata"
        tabIndex={-1}
        aria-hidden="true"
        onLoadedMetadata={seekToFrame}
        onError={() => setFailed(true)}
      />
    );
  } else if (poster) {
    media = (
      <img
        className="rk-media-thumb-media"
        src={resolve(poster)}
        alt=""
        draggable={false}
        onError={() => setFailed(true)}
      />
    );
  } else {
    media = (
      <span className="rk-media-thumb-fallback" aria-hidden="true">
        {AUDIO_GLYPH}
      </span>
    );
  }

  return (
    <div
      className={["rk-media-thumb", `rk-media-thumb-${kind}`, className].filter(Boolean).join(" ")}
      role="img"
      aria-label={name}
    >
      {media}
      {durationLabel ? <span className="rk-media-thumb-duration">{durationLabel}</span> : null}
    </div>
  );
}
