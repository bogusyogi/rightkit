import type { MediaAdapters } from "@rightkit/media-ui";
import { defaultToastBus } from "@rightkit/react-shell";
import { videoAppHost, videoHost } from "../../host.js";

/**
 * The app's bindings for @rightkit/media-ui over the installed host. Media the webview cannot decode is
 * remuxed natively (`prepare_registered_media_playback`, `rightkit-playback`'s registry shared with the host's
 * playback host); the player gets the derivative as an asset URL. The `id` is the registry-issued source id from
 * `useMediaPlaybackId`, never a path.
 */
export const mediaAdapters: MediaAdapters = {
  resolveSrc: (source) => (/^[a-z][a-z0-9+.-]*:/i.test(source) ? source : videoHost().assetUrl(source)),
  preparePlayback: async ({ id, requestId }) =>
    videoHost().assetUrl(await videoAppHost().invoke<string>("prepare_registered_media_playback", { source: id, requestId })),
  cancelPlayback: (requestId) => videoAppHost().invoke<void>("cancel_media_playback", { requestId }),
  setFullscreen: (on) => videoAppHost().window.setFullscreen(on),
  notify: (message, severity) => {
    defaultToastBus.push(message, severity === "error" ? "error" : "info");
  },
};
