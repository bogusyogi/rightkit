import { convertFileSrc } from "@tauri-apps/api/core";
import type { LibraryDetail } from "../../contract.js";
import type { TranscriptDetail } from "../contract.js";

// Only real URLs pass through; a POSIX-absolute path (`/Users/...` on macOS) must go through
// convertFileSrc like a Windows path does.
function fileUrl(src: string): string {
  try {
    return /^[a-z][a-z0-9+.-]*:\/\//i.test(src) ? src : convertFileSrc(src);
  } catch {
    return src;
  }
}

/** The queue's transcript record in the library reader's shape (paths resolved to webview URLs). */
export function toLibraryDetail(detail: TranscriptDetail): LibraryDetail {
  return {
    id: detail.id,
    title: detail.title ?? detail.url,
    text: detail.text,
    timedText: detail.timed_text,
    markdown: detail.md_text,
    timedMarkdown: detail.timed_md_text,
    srt: detail.srt,
    images: detail.slides.map(fileUrl),
    mediaUrl: detail.media_path ? fileUrl(detail.media_path) : null,
    sourceUrl: detail.url,
  };
}
