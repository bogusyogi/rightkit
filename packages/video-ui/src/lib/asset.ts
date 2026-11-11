import { videoHost } from "../host.js";

/** URL for a local file (thumbnail, source, render), from the host. */
export const assetUrl = (path: string) => videoHost().assetUrl(path);
