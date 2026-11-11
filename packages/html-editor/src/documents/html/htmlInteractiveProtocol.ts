import type { SearchOptions } from "@rightkit/document-core/documents/types";
import type { HtmlSafeBlockTarget } from "./sourceMap.js";

// The postMessage wire protocol between the app (parent) and the sandboxed interactive HTML preview
// (an opaque-origin iframe). The parent can't read the frame's DOM, so link clicks, scroll position,
// Ctrl+F, and find run INSIDE the frame and only these tagged messages cross the boundary. The frame
// side of this protocol is the injected bridge script (see htmlBridgeScript.ts).

export const INTERACTIVE_HTML_LINK_MESSAGE = "rk-document:interactive-html-link";
/** Pointer entered/left a link in the preview; carries the href to show (or null to clear). */
export const INTERACTIVE_HTML_LINK_HOVER = "rk-document:interactive-html-link-hover";
export const INTERACTIVE_HTML_APPLY_CSS = "rk-document:interactive-html-apply-css";
export const INTERACTIVE_HTML_APPLY_THEME = "rk-document:interactive-html-apply-theme";
export const INTERACTIVE_HTML_ACTIVITY = "rk-document:interactive-html-activity";
export const INTERACTIVE_HTML_SCROLL = "rk-document:interactive-html-scroll";
export const INTERACTIVE_HTML_SCROLL_TOP = "rk-document:interactive-html-scroll-top";
export const INTERACTIVE_HTML_ZOOM = "rk-document:interactive-html-zoom";
export const INTERACTIVE_HTML_ZOOM_RESET = "rk-document:interactive-html-zoom-reset";
// Find bridge: the sandboxed (opaque-origin) preview runs the actual search + highlighting itself —
// the parent can't read its DOM — and only the query/step commands and the {count, activeIndex}
// results travel across postMessage. FIND_OPEN is the in-frame Ctrl+F asking the app to show its FindBar.
export const INTERACTIVE_HTML_FIND_OPEN = "rk-document:interactive-html-find-open";
export const INTERACTIVE_HTML_HISTORY = "rk-document:interactive-html-history";
export const INTERACTIVE_HTML_OPEN_DEVTOOLS = "rk-document:interactive-html-open-devtools";
export const INTERACTIVE_HTML_FIND = "rk-document:interactive-html-find";
export const INTERACTIVE_HTML_FIND_STEP = "rk-document:interactive-html-find-step";
export const INTERACTIVE_HTML_FIND_CLEAR = "rk-document:interactive-html-find-clear";
export const INTERACTIVE_HTML_FIND_RESULT = "rk-document:interactive-html-find-result";
export const INTERACTIVE_HTML_SELECT_ALL = "rk-document:interactive-html-select-all";
export const INTERACTIVE_HTML_INSPECT_MODE = "rk-document:interactive-html-inspect-mode";
export const INTERACTIVE_HTML_INSPECT_TARGET = "rk-document:interactive-html-inspect-target";
export const INTERACTIVE_HTML_INSPECT_MENU = "rk-document:interactive-html-inspect-menu";
// On right-click, the in-iframe bridge records the element under the cursor (WITHOUT preventing the
// native WebView2 menu) so that when the user picks the grafted native "View source" item, the app
// can reveal that element's source. Distinct from INSPECT_TARGET, which opens the drawer immediately.
export const INTERACTIVE_HTML_CONTEXT_TARGET = "rk-document:interactive-html-context-target";
// After "View source" opens the drawer, the preview narrows and can reflow (e.g. a page @media
// breakpoint), scrolling the clicked element out of view — so ask the frame to scroll the LAST
// right-clicked element back into view, keeping rendered + source aligned.
export const INTERACTIVE_HTML_SCROLL_TO_INSPECT = "rk-document:interactive-html-scroll-to-inspect";
export const INTERACTIVE_HTML_PRINT = "rk-document:interactive-html-print";
// The frame replies with its own fully-rendered markup (parent → iframe → parent) instead of calling
// window.print() itself: this iframe is pinned to the on-screen content-pane box (h-full w-full), and
// printing an iframe prints only that rendered box, not the document's full scrollable height —
// anything below the on-screen fold would silently never reach paper. Serializing the DOM back to the
// parent lets the app print it in-flow (see documentPrinting.ts printHtmlMarkup), where it can paginate
// naturally across as many pages as the content needs.
export const INTERACTIVE_HTML_PRINT_CONTENT = "rk-document:interactive-html-print-content";
// Interactive double-click hint: session-scoped & untrusted. Parent resolves stamped identity
// against current source before opening an app-owned editor; this message never mutates source.
export const INTERACTIVE_HTML_EDIT_VISUAL = "rk-document:interactive-html-edit-visual";
// Double-click on content outside any editable (stamped) block. Carries no page data: the parent
// only ever shows its fixed "can only be edited in Source" toast in response.
export const INTERACTIVE_HTML_EDIT_REFUSED = "rk-document:interactive-html-edit-refused";

// ── Senders (parent → iframe) ─────────────────────────────────────────────────────────────────

function postInteractive(
  iframe: HTMLIFrameElement | null,
  type: string,
  sessionId: string,
  payload: Record<string, unknown> = {},
): void {
  // The sandboxed preview has an opaque origin, so parent-to-frame postMessage must use "*".
  iframe?.contentWindow?.postMessage({ type, sessionId, ...payload }, "*"); // nosemgrep: javascript.browser.security.wildcard-postmessage-configuration.wildcard-postmessage-configuration
}

/** Push edited inline CSS into the running interactive iframe; the in-frame bridge swaps a live
 *  `<style>` with no reload. No-op if the iframe isn't mounted/loaded yet. */
export function postLiveCss(
  iframe: HTMLIFrameElement | null,
  sessionId: string,
  css: string,
): void {
  postInteractive(iframe, INTERACTIVE_HTML_APPLY_CSS, sessionId, { css });
}

/** Tell the preview whether its tab is active and the window visible. While inactive the in-frame
 *  bridge pauses media and defers timers/animation frames (docs/FEATURES.md, HTML inactive policy). */
export function postInteractiveActivity(
  iframe: HTMLIFrameElement | null,
  sessionId: string,
  active: boolean,
): void {
  postInteractive(iframe, INTERACTIVE_HTML_ACTIVITY, sessionId, { active });
}

/** Push app theme CSS into the sandboxed iframe without rebuilding srcDoc or rerunning page scripts. */
export function postInteractiveThemeCss(
  iframe: HTMLIFrameElement | null,
  sessionId: string,
  css: string,
): void {
  postInteractive(iframe, INTERACTIVE_HTML_APPLY_THEME, sessionId, { css });
}

/** Run a find in the sandboxed preview. The in-frame engine highlights matches and replies with a
 *  `*_FIND_RESULT` carrying the same `requestId` so the caller can correlate the async response. */
export function postInteractiveFindQuery(
  iframe: HTMLIFrameElement | null,
  sessionId: string,
  requestId: number,
  query: string,
  options: SearchOptions,
): void {
  postInteractive(iframe, INTERACTIVE_HTML_FIND, sessionId, {
    requestId,
    query,
    matchCase: options.matchCase,
    wholeWord: options.wholeWord,
  });
}

/** Move the active match in the sandboxed preview (delta +1 next / -1 prev), wrapping. */
export function postInteractiveFindStep(
  iframe: HTMLIFrameElement | null,
  sessionId: string,
  requestId: number,
  delta: number,
): void {
  postInteractive(iframe, INTERACTIVE_HTML_FIND_STEP, sessionId, { requestId, delta });
}

/** Clear all find highlights in the sandboxed preview (fire-and-forget). */
export function postInteractiveFindClear(
  iframe: HTMLIFrameElement | null,
  sessionId: string,
): void {
  postInteractive(iframe, INTERACTIVE_HTML_FIND_CLEAR, sessionId);
}

/** Select only the sandboxed preview document, not the host app surrounding chrome. */
export function postInteractiveSelectAll(
  iframe: HTMLIFrameElement | null,
  sessionId: string,
): void {
  postInteractive(iframe, INTERACTIVE_HTML_SELECT_ALL, sessionId);
}

/** Scroll the sandboxed preview document back to the top. */
export function postInteractiveScrollTop(
  iframe: HTMLIFrameElement | null,
  sessionId: string,
): void {
  postInteractive(iframe, INTERACTIVE_HTML_SCROLL_TOP, sessionId);
}

/** Ask the frame to scroll the most-recently right-clicked element back into view (after View source). */
export function postScrollToInspect(iframe: HTMLIFrameElement | null, sessionId: string): void {
  postInteractive(iframe, INTERACTIVE_HTML_SCROLL_TO_INSPECT, sessionId);
}

/** Ask the sandboxed preview to drop its own Ctrl/Cmd+wheel zoom back to 100%. The zoom lives
 *  inside the frame (an opaque-origin document the parent cannot touch), so resetting it has to be
 *  a request rather than a direct style write. */
export function postInteractiveZoomReset(
  iframe: HTMLIFrameElement | null,
  sessionId: string,
): void {
  postInteractive(iframe, INTERACTIVE_HTML_ZOOM_RESET, sessionId);
}

/** Ask the frame to serialize and return its own document for printing, rather than printing its
 *  viewport-sized parent iframe box directly. */
export function postInteractivePrint(iframe: HTMLIFrameElement | null, sessionId: string): void {
  postInteractive(iframe, INTERACTIVE_HTML_PRINT, sessionId);
}

// ── Guards (validate iframe → parent messages) ──────────────────────────────────────────────────

function isObject(data: unknown): data is Record<string, unknown> {
  return !!data && typeof data === "object";
}

function isInspectPayload(msg: Record<string, unknown>, sessionId: string): boolean {
  return (
    msg.sessionId === sessionId &&
    typeof msg.text === "string" &&
    (msg.selectedText === undefined || typeof msg.selectedText === "string") &&
    (msg.tagName === undefined || typeof msg.tagName === "string") &&
    (msg.id === undefined || typeof msg.id === "string") &&
    (msg.classNames === undefined ||
      (Array.isArray(msg.classNames) &&
        msg.classNames.every((value) => typeof value === "string"))) &&
    (msg.href === undefined || typeof msg.href === "string") &&
    (msg.src === undefined || typeof msg.src === "string")
  );
}

export function isInteractiveHtmlLinkMessage(
  data: unknown,
  sessionId: string,
): data is {
  type: typeof INTERACTIVE_HTML_LINK_MESSAGE;
  sessionId: string;
  href: string;
  newTab?: boolean;
} {
  if (!isObject(data)) return false;
  const msg = data as { type?: unknown; sessionId?: unknown; href?: unknown };
  return (
    msg.type === INTERACTIVE_HTML_LINK_MESSAGE &&
    msg.sessionId === sessionId &&
    typeof msg.href === "string"
  );
}

export function isInteractiveHtmlLinkHoverMessage(
  data: unknown,
  sessionId: string,
): data is { type: typeof INTERACTIVE_HTML_LINK_HOVER; sessionId: string; href: string | null } {
  if (!isObject(data)) return false;
  const msg = data as { type?: unknown; sessionId?: unknown; href?: unknown };
  return (
    msg.type === INTERACTIVE_HTML_LINK_HOVER &&
    msg.sessionId === sessionId &&
    (typeof msg.href === "string" || msg.href === null)
  );
}

/** The in-frame Ctrl+F asking the app to open its FindBar for this session. */
export function isInteractiveFindOpen(data: unknown, sessionId: string): boolean {
  if (!isObject(data)) return false;
  const msg = data as { type?: unknown; sessionId?: unknown };
  return msg.type === INTERACTIVE_HTML_FIND_OPEN && msg.sessionId === sessionId;
}

/** Cmd/Ctrl+Z or Y pressed while focus is inside the frame, where the app's own shortcut never arrives. */
export function isInteractiveHistory(
  data: unknown,
  sessionId: string,
): data is { type: typeof INTERACTIVE_HTML_HISTORY; sessionId: string; direction: "undo" | "redo" } {
  if (!isObject(data)) return false;
  const msg = data as { type?: unknown; sessionId?: unknown; direction?: unknown };
  return (
    msg.type === INTERACTIVE_HTML_HISTORY &&
    msg.sessionId === sessionId &&
    (msg.direction === "undo" || msg.direction === "redo")
  );
}

/** F12 / inspect chord pressed while focus is inside the opaque-origin frame. */
export function isInteractiveOpenDevtools(data: unknown, sessionId: string): boolean {
  if (!isObject(data)) return false;
  const msg = data as { type?: unknown; sessionId?: unknown };
  return msg.type === INTERACTIVE_HTML_OPEN_DEVTOOLS && msg.sessionId === sessionId;
}

/** The sandboxed preview applied its own Ctrl/Cmd+wheel zoom and reports the current percent. */
export function isInteractiveZoomMessage(
  data: unknown,
  sessionId: string,
): data is { type: typeof INTERACTIVE_HTML_ZOOM; sessionId: string; percent: number } {
  if (!isObject(data)) return false;
  const msg = data as { type?: unknown; sessionId?: unknown; percent?: unknown };
  return (
    msg.type === INTERACTIVE_HTML_ZOOM &&
    msg.sessionId === sessionId &&
    typeof msg.percent === "number"
  );
}

/** A find result from the sandboxed preview, correlated to a request by `requestId`. */
export function isInteractiveFindResult(
  data: unknown,
  sessionId: string,
): data is {
  type: typeof INTERACTIVE_HTML_FIND_RESULT;
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
    msg.type === INTERACTIVE_HTML_FIND_RESULT &&
    msg.sessionId === sessionId &&
    typeof msg.requestId === "number" &&
    typeof msg.count === "number" &&
    typeof msg.activeIndex === "number"
  );
}

export function isInteractiveInspectTarget(
  data: unknown,
  sessionId: string,
): data is {
  type: typeof INTERACTIVE_HTML_INSPECT_TARGET;
  sessionId: string;
  text: string;
  selectedText?: string;
  tagName?: string;
  id?: string;
  classNames?: string[];
  href?: string;
  src?: string;
} {
  return (
    isObject(data) &&
    data.type === INTERACTIVE_HTML_INSPECT_TARGET &&
    isInspectPayload(data, sessionId)
  );
}

/** The bridge's record of the element under the last right-click (native menu was NOT suppressed). */
export function isInteractiveContextTarget(
  data: unknown,
  sessionId: string,
): data is {
  type: typeof INTERACTIVE_HTML_CONTEXT_TARGET;
  sessionId: string;
  text: string;
  selectedText?: string;
  tagName?: string;
  id?: string;
  classNames?: string[];
  href?: string;
  src?: string;
} {
  return (
    isObject(data) &&
    data.type === INTERACTIVE_HTML_CONTEXT_TARGET &&
    isInspectPayload(data, sessionId)
  );
}

export function isInteractiveInspectMenu(
  data: unknown,
  sessionId: string,
): data is {
  type: typeof INTERACTIVE_HTML_INSPECT_MENU;
  sessionId: string;
  text: string;
  selectedText?: string;
  tagName?: string;
  id?: string;
  classNames?: string[];
  href?: string;
  src?: string;
  x: number;
  y: number;
} {
  return (
    isObject(data) &&
    data.type === INTERACTIVE_HTML_INSPECT_MENU &&
    isInspectPayload(data, sessionId) &&
    typeof data.x === "number" &&
    typeof data.y === "number"
  );
}

/** The frame's serialized document, sent in reply to postInteractivePrint. */
export function isInteractivePrintContent(
  data: unknown,
  sessionId: string,
): data is { type: typeof INTERACTIVE_HTML_PRINT_CONTENT; sessionId: string; html: string } {
  if (!isObject(data)) return false;
  const msg = data as { type?: unknown; sessionId?: unknown; html?: unknown };
  return (
    msg.type === INTERACTIVE_HTML_PRINT_CONTENT &&
    msg.sessionId === sessionId &&
    typeof msg.html === "string"
  );
}

export function isInteractiveEditRefused(
  data: unknown,
  sessionId: string,
): data is { type: typeof INTERACTIVE_HTML_EDIT_REFUSED; sessionId: string; pagePath: string } {
  if (!isObject(data)) return false;
  return (
    data.type === INTERACTIVE_HTML_EDIT_REFUSED &&
    data.sessionId === sessionId &&
    typeof data.pagePath === "string" &&
    Object.keys(data).every((key) => key === "type" || key === "sessionId" || key === "pagePath")
  );
}

export function isInteractiveEditVisual(
  data: unknown,
  sessionId: string,
): data is HtmlSafeBlockTarget & {
  type: typeof INTERACTIVE_HTML_EDIT_VISUAL;
  sessionId: string;
} {
  if (!isObject(data)) return false;
  if (data.type !== INTERACTIVE_HTML_EDIT_VISUAL || data.sessionId !== sessionId) return false;
  const geometry = data.geometry;
  return (
    isInspectPayload(data, sessionId) &&
    typeof data.pagePath === "string" &&
    Array.isArray(data.childPath) &&
    data.childPath.length > 0 &&
    data.childPath.every((value) => Number.isInteger(value) && Number(value) >= 0) &&
    typeof data.fingerprint === "string" &&
    data.fingerprint.length > 0 &&
    isObject(geometry) &&
    [geometry.left, geometry.top, geometry.width, geometry.height].every(
      (value) => typeof value === "number" && Number.isFinite(value),
    ) &&
    (data.style === undefined ||
      (isObject(data.style) &&
        Object.values(data.style).every(
          (value) => value === undefined || typeof value === "string",
        ))) &&
    (data.textSelection === undefined ||
      (isObject(data.textSelection) &&
        typeof data.textSelection.from === "number" &&
        Number.isInteger(data.textSelection.from) &&
        data.textSelection.from >= 0 &&
        typeof data.textSelection.to === "number" &&
        Number.isInteger(data.textSelection.to) &&
        data.textSelection.to > data.textSelection.from))
  );
}
