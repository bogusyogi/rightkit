import type { LoadedDocument, WebDocument } from "../types.js";

const LOOPBACK_HOSTS = new Set(["localhost", "127.0.0.1", "::1", "[::1]"]);

function hasScheme(input: string): boolean {
  return /^[a-z][a-z0-9+.-]*:/i.test(input);
}

function looksLikeBareLoopback(input: string): boolean {
  return /^(localhost|127\.0\.0\.1|\[::1\])(?::|\/|$)/i.test(input);
}

function isLoopbackHost(hostname: string): boolean {
  return LOOPBACK_HOSTS.has(hostname.toLowerCase());
}

export function isLoopbackWebUrl(input: string): boolean {
  try {
    const url = new URL(input);
    return (url.protocol === "http:" || url.protocol === "https:") && isLoopbackHost(url.hostname);
  } catch {
    return false;
  }
}

function looksLikeBareWebHost(input: string): boolean {
  if (input.includes("/") || input.includes(":")) return true;
  return /^[a-z0-9-]+(\.[a-z0-9-]+)+$/i.test(input);
}

export function normalizeLivePreviewUrl(input: string): string | null {
  const raw = input.trim();
  if (!raw || /\s/.test(raw)) return null;
  const withScheme =
    hasScheme(raw) && !looksLikeBareLoopback(raw)
      ? raw
      : looksLikeBareLoopback(raw)
        ? `http://${raw}`
        : `https://${raw}`;
  if (!hasScheme(raw) && !looksLikeBareWebHost(raw)) return null;
  let url: URL;
  try {
    url = new URL(withScheme);
  } catch {
    return null;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;
  return url.toString();
}

export function isHostAppOrigin(
  url: string,
  appOrigin = typeof window !== "undefined" ? window.location.origin : "",
): boolean {
  if (!appOrigin) return false;
  try {
    return new URL(url).origin === new URL(appOrigin).origin;
  } catch {
    return false;
  }
}

export function webTabPath(url: string): string {
  return `web:${url}`;
}

export function isWebTabKey(path: string): boolean {
  return path.startsWith("web:");
}

export function isWebDocument(
  doc: LoadedDocument | null | undefined,
): doc is LoadedDocument & { format: "web"; web: WebDocument } {
  return doc?.format === "web" && !!doc.web;
}

export function webDisplayTitle(web: WebDocument): string {
  try {
    const u = new URL(web.url);
    const path = u.pathname && u.pathname !== "/" ? u.pathname : "";
    return `${isLoopbackWebUrl(web.url) ? "Live" : "Web"}: ${u.host}${path}`;
  } catch {
    return "Web preview";
  }
}
