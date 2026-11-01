import { IS_WINDOWS } from "./platform";

export const EMPTY_IMG_SRC =
  "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==";
export const MAX_DATA_IMAGE_BYTES = 8 * 1024 * 1024;
export const MAX_DATA_IMAGE_B64_CHARS = Math.ceil((MAX_DATA_IMAGE_BYTES * 4) / 3) + 128;
export const MAX_REMOTE_IMAGES_PER_MESSAGE = 30;

// The cached-image custom protocol. Windows WebView2 serves custom URI schemes
// at http://<scheme>.localhost/; macOS/Linux use the <scheme>://localhost/ form.
// Emit the right one for the running platform so cached remote images load
// (the iframe CSP whitelists both forms).
export const RIGHTKIT_IMG_BASE = IS_WINDOWS
  ? "http://mailright-img.localhost/"
  : "mailright-img://localhost/";

/** True for our local cached-image protocol in either platform URL form. */
export function isLocalCachedImg(src: string): boolean {
  return /^mailright-img:/i.test(src) || /^https?:\/\/mailright-img\.localhost\//i.test(src);
}

/** Resolve a fetched remote-image token into something the renderer can load. */
export function remoteImageSrc(hash: string): string {
  return /^data:image\//i.test(hash) ? hash : `${RIGHTKIT_IMG_BASE}${hash}`;
}

export type ImageRule = "show" | "block";

export function dataImageMagicMatches(mime: string, head: string): boolean {
  if (mime === "image/png") return head.startsWith("\x89PNG\r\n\x1a\n");
  if (mime === "image/jpeg") return head.startsWith("\xff\xd8\xff");
  if (mime === "image/gif") return head.startsWith("GIF87a") || head.startsWith("GIF89a");
  if (mime === "image/webp") {
    return head.length >= 12 && head.startsWith("RIFF") && head.slice(8, 12) === "WEBP";
  }
  return false;
}

export function isSafeDataImageSrc(src: string): boolean {
  const m = src.match(/^data:(image\/(?:png|jpeg|gif|webp));base64,([a-z0-9+/=\s]+)$/i);
  if (!m) return false;
  const mime = m[1].toLowerCase();
  const b64 = m[2].replace(/\s/g, "");
  if (b64.length === 0 || b64.length > MAX_DATA_IMAGE_B64_CHARS) return false;
  try {
    return dataImageMagicMatches(mime, atob(b64.slice(0, 64)));
  } catch {
    return false;
  }
}

export function emailDomain(addr: string): string {
  const cleaned = addr.trim().toLowerCase().replace(/[),.;]+$/, "");
  const m = cleaned.match(/@([^>\s]+)>?$/);
  return m ? m[1].replace(/[),.;]+$/, "") : "";
}

/** Whether ReaderMessage should render the "N trackers blocked" note, and its
 * text, given the current showImages toggle and this message's blocked count.
 * Extracted as a pure helper so the render-gating logic is unit-testable
 * without a DOM/component-rendering harness. */
export function trackerNoteFor(showImages: boolean, blockedCount: number | undefined): string | null {
  const count = blockedCount ?? 0;
  if (!showImages || count <= 0) return null;
  return `${count} tracker${count === 1 ? "" : "s"} blocked`;
}

export function resolveImageRule(
  addr: string,
  senderRules: Record<string, ImageRule> = {},
  domainRules: Record<string, ImageRule> = {}
): ImageRule | null {
  const key = addr.trim().toLowerCase();
  const domain = emailDomain(addr);
  return senderRules[addr] ?? senderRules[key] ?? (domain ? domainRules[domain] : undefined) ?? null;
}
