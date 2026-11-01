import { useEffect, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { MediaAdapters } from "@rightkit/media-ui";
import { defaultToastBus } from "@rightkit/react-shell";

/**
 * the host app bindings for @rightkit/media-ui. Media the webview cannot decode is
 * remuxed by `rightkit-playback` (`prepare_registered_media_playback`, registry shared with
 * src-tauri/src/playback_host.rs); the player gets the derivative as an asset URL.
 * The `id` is the registry-issued source id from `useMediaPlaybackId`, never a path.
 */
export const mediaAdapters: MediaAdapters = {
  resolveSrc: (source) => (/^[a-z][a-z0-9+.-]*:/i.test(source) ? source : convertFileSrc(source)),
  preparePlayback: async ({ id, requestId }) =>
    convertFileSrc(await invoke<string>("prepare_registered_media_playback", { source: id, requestId })),
  cancelPlayback: (requestId) => invoke<void>("cancel_media_playback", { requestId }),
  setFullscreen: (on) => getCurrentWindow().setFullscreen(on),
  notify: (message, severity) => {
    defaultToastBus.push(message, severity === "error" ? "error" : "info");
  },
};

/** Registry-issued id; RightKit's authoriser checks the host's scope on every platform. */
export const mediaSourceId = (path: string) => invoke<string>("authorised_media_source_id", { path });

/**
 * The `playbackId` for `path`, once the host has registered it. Undefined until then, and
 * stays undefined when the file is outside the media scope (no compatibility copy, as before).
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
