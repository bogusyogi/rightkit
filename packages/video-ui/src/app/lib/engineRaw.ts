import { videoAppHost, type VideoEngineRawEvent } from "../../host.js";

export type { VideoEngineRawEvent };

/** Every engine event payload (`{call, id, tag, event}`), lifecycle frames included, from the installed host. */
export const onEngineRaw = (handler: (payload: VideoEngineRawEvent) => void): Promise<() => void> =>
  Promise.resolve().then(() => videoAppHost().onEngineRaw(handler));
