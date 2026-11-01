import { convertFileSrc } from "@tauri-apps/api/core";

/** URL for a local file (thumbnail, source, render) inside the Tauri webview. */
export const assetUrl = (path: string) => convertFileSrc(path);
