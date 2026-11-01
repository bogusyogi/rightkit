// PostMessage wire protocol between the app (parent) and the sandboxed MDX preview iframe.
// The parent can't read the frame's DOM (opaque origin: `sandbox="allow-scripts"`, never
// `allow-same-origin`), so every cross-boundary signal is a typed message here. The frame side
// of this protocol is the injected bridge script (mdxBridgeScript.ts). Mirrors the shape of
// htmlInteractiveProtocol.ts so reviewers can map them at a glance.

import type { SearchOptions } from "@rightkit/document-core/documents/types";
import type { CompiledMdxModule } from "./mdxCompiler";

export const MDX_LINK = "viewright:mdx-link"; // iframe → parent
export const MDX_COMPILED = "viewright:mdx-compiled"; // parent → iframe
export const MDX_COMPILE_ERROR = "viewright:mdx-compile-error"; // iframe → parent (post-eval)
export const MDX_RUNTIME_ERROR = "viewright:mdx-runtime-error"; // iframe → parent (render throw)
export const MDX_HEIGHT = "viewright:mdx-height"; // iframe → parent (ResizeObserver)
export const MDX_FIND_OPEN = "viewright:mdx-find-open"; // iframe → parent (in-frame Ctrl+F)
export const MDX_FIND = "viewright:mdx-find"; // parent → iframe
export const MDX_FIND_STEP = "viewright:mdx-find-step"; // parent → iframe
export const MDX_FIND_CLEAR = "viewright:mdx-find-clear"; // parent → iframe
export const MDX_FIND_RESULT = "viewright:mdx-find-result"; // iframe → parent
export const MDX_PRINT = "viewright:mdx-print"; // parent → iframe
// The frame replies with its own rendered markup rather than calling window.print() on itself: this
// iframe is pinned to the on-screen content-pane box, so printing it directly paginates against that
// fixed viewport, not the document's full height (see htmlInteractiveProtocol.ts for the identical
// HTML-side rationale). documentPrinting.ts prints the returned markup in-flow instead.
export const MDX_PRINT_CONTENT = "viewright:mdx-print-content"; // iframe → parent
export const MDX_THEME = "viewright:mdx-theme"; // parent → iframe (resolved app theme vars)

// ── Senders (parent → iframe) ─────────────────────────────────────────────────────

function postMdx(
  iframe: HTMLIFrameElement | null,
  type: string,
  sessionId: string,
  payload: Record<string, unknown> = {},
): void {
  // The sandboxed preview has an opaque origin, so parent-to-frame postMessage must use "*".
  iframe?.contentWindow?.postMessage({ type, sessionId, ...payload }, "*"); // nosemgrep: javascript.browser.security.wildcard-postmessage-configuration.wildcard-postmessage-configuration
}

/** Push the parent's freshly-compiled contained module graph into the iframe for evaluation. */
export function postMdxCompiled(
  iframe: HTMLIFrameElement | null,
  sessionId: string,
  entryId: string,
  modules: Record<string, CompiledMdxModule>,
): void {
  postMdx(iframe, MDX_COMPILED, sessionId, { entryId, modules });
}

/** Run a find inside the sandboxed MDX preview. The frame wraps matches in `<span>`s and replies
 *  with `MDX_FIND_RESULT` carrying the same `requestId` so the caller can correlate. */
export function postMdxFindQuery(
  iframe: HTMLIFrameElement | null,
  sessionId: string,
  requestId: number,
  query: string,
  options: SearchOptions,
): void {
  postMdx(iframe, MDX_FIND, sessionId, {
    requestId,
    query,
    matchCase: options.matchCase,
    wholeWord: options.wholeWord,
  });
}

/** Step the active match (`delta` +1 next / -1 prev), wrapping. */
export function postMdxFindStep(
  iframe: HTMLIFrameElement | null,
  sessionId: string,
  requestId: number,
  delta: number,
): void {
  postMdx(iframe, MDX_FIND_STEP, sessionId, { requestId, delta });
}

/** Clear all in-frame find highlights. Fire-and-forget. */
export function postMdxFindClear(iframe: HTMLIFrameElement | null, sessionId: string): void {
  postMdx(iframe, MDX_FIND_CLEAR, sessionId);
}

/** Push the app's resolved theme colors (frame `--vr-*` name -> value) into the frame. */
export function postMdxTheme(
  iframe: HTMLIFrameElement | null,
  sessionId: string,
  vars: Record<string, string>,
): void {
  postMdx(iframe, MDX_THEME, sessionId, { vars });
}

/** Print the sandboxed document itself, never its empty parent iframe shell. */
export function postMdxPrint(iframe: HTMLIFrameElement | null, sessionId: string): void {
  postMdx(iframe, MDX_PRINT, sessionId);
}

// ── Guards (validate iframe → parent messages) ─────────────────────────────────────

function isObject(data: unknown): data is Record<string, unknown> {
  return !!data && typeof data === "object";
}

export function isMdxLinkMessage(
  data: unknown,
  sessionId: string,
): data is { type: typeof MDX_LINK; sessionId: string; href: string; newTab?: boolean } {
  if (!isObject(data)) return false;
  const msg = data as { type?: unknown; sessionId?: unknown; href?: unknown; newTab?: unknown };
  return (
    msg.type === MDX_LINK &&
    msg.sessionId === sessionId &&
    typeof msg.href === "string" &&
    (msg.newTab === undefined || typeof msg.newTab === "boolean")
  );
}

export function isMdxCompileError(
  data: unknown,
  sessionId: string,
): data is {
  type: typeof MDX_COMPILE_ERROR;
  sessionId: string;
  kind: string;
  message: string;
  line?: number;
  column?: number;
} {
  if (!isObject(data)) return false;
  const msg = data as { type?: unknown; sessionId?: unknown; kind?: unknown; message?: unknown };
  return (
    msg.type === MDX_COMPILE_ERROR &&
    msg.sessionId === sessionId &&
    typeof msg.kind === "string" &&
    typeof msg.message === "string"
  );
}

export function isMdxRuntimeError(
  data: unknown,
  sessionId: string,
): data is {
  type: typeof MDX_RUNTIME_ERROR;
  sessionId: string;
  name?: string;
  message: string;
  stack?: string;
} {
  if (!isObject(data)) return false;
  const msg = data as { type?: unknown; sessionId?: unknown; message?: unknown };
  return (
    msg.type === MDX_RUNTIME_ERROR && msg.sessionId === sessionId && typeof msg.message === "string"
  );
}

export function isMdxHeightMessage(
  data: unknown,
  sessionId: string,
): data is { type: typeof MDX_HEIGHT; sessionId: string; height: number } {
  if (!isObject(data)) return false;
  const msg = data as { type?: unknown; sessionId?: unknown; height?: unknown };
  return msg.type === MDX_HEIGHT && msg.sessionId === sessionId && typeof msg.height === "number";
}

export function isMdxFindOpen(data: unknown, sessionId: string): boolean {
  if (!isObject(data)) return false;
  const msg = data as { type?: unknown; sessionId?: unknown };
  return msg.type === MDX_FIND_OPEN && msg.sessionId === sessionId;
}

export function isMdxFindResult(
  data: unknown,
  sessionId: string,
): data is {
  type: typeof MDX_FIND_RESULT;
  sessionId: string;
  requestId: number;
  count: number;
  activeIndex: number;
} {
  if (!isObject(data)) return false;
  const msg = data as {
    type?: unknown;
    sessionId?: unknown;
    requestId?: unknown;
    count?: unknown;
    activeIndex?: unknown;
  };
  return (
    msg.type === MDX_FIND_RESULT &&
    msg.sessionId === sessionId &&
    typeof msg.requestId === "number" &&
    typeof msg.count === "number" &&
    typeof msg.activeIndex === "number"
  );
}

/** The frame's serialized document, sent in reply to postMdxPrint. */
export function isMdxPrintContent(
  data: unknown,
  sessionId: string,
): data is { type: typeof MDX_PRINT_CONTENT; sessionId: string; html: string } {
  if (!isObject(data)) return false;
  const msg = data as { type?: unknown; sessionId?: unknown; html?: unknown };
  return (
    msg.type === MDX_PRINT_CONTENT && msg.sessionId === sessionId && typeof msg.html === "string"
  );
}
