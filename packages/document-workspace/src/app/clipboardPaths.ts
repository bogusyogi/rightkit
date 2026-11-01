import { formatFromPath } from "@rightkit/document-core/documents/types";

export function localPathFromClipboardText(text: string): string | null {
  const trimmed = text.trim();
  if (!trimmed || /\r|\n/.test(trimmed)) return null;
  const unquoted = trimmed.replace(/^(['"`])(.+)\1$/, "$2").trim();
  const path = fileUrlToLocalPath(unquoted) ?? unquoted;
  const isWindowsDrive = /^[a-z]:[\\/].+/i.test(path);
  const isUnc = /^\\\\[^\\\/]+[\\\/][^\\\/]+[\\\/].+/.test(path);
  const isPosixAbsolute = /^\/[^/].+/.test(path);
  if (!isWindowsDrive && !isUnc && !isPosixAbsolute) return null;
  return formatFromPath(path) === "unknown" ? null : path;
}

function fileUrlToLocalPath(input: string): string | null {
  let url: URL;
  try {
    url = new URL(input);
  } catch {
    return null;
  }
  if (url.protocol !== "file:") return null;
  const decoded = decodeURIComponent(url.pathname);
  if (/^\/[a-z]:\//i.test(decoded)) return decoded.slice(1).replace(/\//g, "\\");
  if (url.hostname) return `\\\\${url.hostname}${decoded.replace(/\//g, "\\")}`;
  return decoded;
}
