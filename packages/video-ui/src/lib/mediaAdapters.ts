import { useEffect, useState } from "react";
import { videoHost } from "../host.js";

/** The host's registered id for `path`; the host checks its media scope. */
export const mediaSourceId = (path: string) => videoHost().mediaSourceId(path);

/**
 * The `playbackId` for `path`, once the host has registered it. Undefined until then, and
 * stays undefined when the file is outside the media scope (no compatibility copy).
 * Call sites key the player on this value so a late id remounts it cleanly.
 */
export function useMediaPlaybackId(path: string | null | undefined): string | undefined {
  const [known, setKnown] = useState<{ path: string; id: string } | null>(null);
  useEffect(() => {
    if (!path) return;
    let live = true;
    mediaSourceId(path).then(
      (id) => {
        if (live) setKnown({ path, id });
      },
      () => undefined,
    );
    return () => {
      live = false;
    };
  }, [path]);
  return path && known?.path === path ? known.id : undefined;
}

const AUDIO_EXT = /\.(wav|mp3|m4a|flac|aac|ogg|opus)$/i;
const MEDIA_EXT = /\.(mp4|mov|mkv|webm|m4v|wav|mp3|m4a|flac|aac|ogg|opus)$/i;

export const isPlayable = (path: string) => MEDIA_EXT.test(path);
export const mediaKind = (path: string): "audio" | "video" => (AUDIO_EXT.test(path) ? "audio" : "video");
