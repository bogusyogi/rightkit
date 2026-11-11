import { useEffect, type MutableRefObject, type RefObject } from "react";
import {
  INTERACTIVE_HTML_SCROLL,
  isInteractiveFindOpen,
  isInteractiveHistory,
  isInteractiveOpenDevtools,
  isInteractiveFindResult,
  isInteractiveHtmlLinkMessage,
  isInteractiveHtmlLinkHoverMessage,
  isInteractiveInspectMenu,
  isInteractiveInspectTarget,
  isInteractiveContextTarget,
  isInteractiveZoomMessage,
  isInteractivePrintContent,
  isInteractiveEditVisual,
  isInteractiveEditRefused,
  postInteractiveActivity,
} from "./htmlInteractiveProtocol.js";
import {
  onContextOpenLinkExternal,
  onContextOpenLinkTab,
  onContextViewSource,
  openExternal,
  openDevtools,
} from "@rightkit/document-core/state/tauriBridge";
import { printIsolatedHtmlMarkup } from "@rightkit/document-core/app/documentPrinting";
import { logError } from "@rightkit/document-core/state/diagnostics";
import { toast } from "@rightkit/platform-ui/headless";
import type { HtmlInspectTarget, HtmlSafeBlockTarget } from "./sourceMap.js";
import type { SearchResult } from "@rightkit/document-core/documents/types";
import { HTML_SURFACE_MESSAGE, publishHtmlSurface } from "./htmlBridgeScript.js";
import { documentEvent } from "@rightkit/document-core/host";

/**
 * The interactive-preview postMessage bridge + the three native-context-menu Tauri event listeners
 * (View source / Open link in new tab / Open link externally). Extracted verbatim from HtmlDocument —
 * no behavior change. Trust model unchanged: only messages from THIS iframe (`event.source`) carrying
 * THIS session's id are accepted (see docs/HTML_SECURITY_INVARIANTS.md).
 *
 * Renders nothing; it's a hook-shaped module kept as a sibling file per the decomposition plan.
 */
export function useHtmlDocumentBridge({
  iframeRef,
  sessionId,
  lastContextTargetRef,
  handleHref,
  onRequestFind,
  resolveFindResult,
  handleInspectTarget,
  openInspectMenu,
  onPreviewScrollRef,
  scrollYRef,
  setPreviewZoomHud,
  previewZoomTimer,
  onEnterVisual,
  onEditRefused,
  onLinkHover,
}: {
  iframeRef: RefObject<HTMLIFrameElement | null>;
  sessionId: string;
  lastContextTargetRef: MutableRefObject<HtmlInspectTarget | null>;
  handleHref: (href: string, newTab?: boolean) => void | Promise<void>;
  onRequestFind?: () => void;
  resolveFindResult: (requestId: number, result: SearchResult) => void;
  handleInspectTarget: (target: HtmlInspectTarget) => void;
  openInspectMenu: (target: HtmlInspectTarget, point: { x: number; y: number }) => void;
  onPreviewScrollRef: MutableRefObject<((y: number) => void) | undefined>;
  scrollYRef: MutableRefObject<number>;
  setPreviewZoomHud: (value: string | null) => void;
  previewZoomTimer: MutableRefObject<number | null>;
  onEnterVisual?: (target: HtmlSafeBlockTarget) => void;
  onEditRefused?: (pagePath: string) => void;
  onLinkHover?: (href: string | null) => void;
}) {
  // Bridge: trust only messages from THIS iframe (event.source) carrying THIS session id. Handles
  // link clicks (open/navigate) and scroll reports (to restore scroll across a live rebuild).
  useEffect(() => {
    const onMessage = (event: MessageEvent) => {
      const frame = iframeRef.current?.contentWindow;
      if (!frame || event.source !== frame) return;
      const data = event.data as {
        type?: unknown;
        sessionId?: unknown;
        y?: unknown;
        body?: unknown;
        root?: unknown;
      };
      if (data?.type === HTML_SURFACE_MESSAGE && data.sessionId === sessionId) {
        const iframe = iframeRef.current;
        if (iframe && typeof data.body === "string" && typeof data.root === "string") {
          publishHtmlSurface(iframe, data.body) || publishHtmlSurface(iframe, data.root);
        }
        return;
      }
      if (
        data &&
        data.type === INTERACTIVE_HTML_SCROLL &&
        data.sessionId === sessionId &&
        typeof data.y === "number"
      ) {
        scrollYRef.current = data.y;
        onPreviewScrollRef.current?.(data.y); // persist to App's durable per-tab store
        return;
      }
      if (isInteractiveZoomMessage(event.data, sessionId)) {
        const percent = event.data.percent;
        if (previewZoomTimer.current != null) window.clearTimeout(previewZoomTimer.current);
        // Off 100% the read-out stays put and becomes the reset control; at 100% there is nothing
        // to reset, so it reverts to the old transient confirmation and fades.
        setPreviewZoomHud(`${percent}%`);
        if (percent === 100) {
          previewZoomTimer.current = window.setTimeout(() => setPreviewZoomHud(null), 900);
        }
        return;
      }
      // The sandboxed frame's Ctrl+F asks the app to open its FindBar (the frame can't reach it directly).
      if (isInteractiveFindOpen(event.data, sessionId)) {
        onRequestFind?.();
        return;
      }
      if (isInteractiveHistory(event.data, sessionId)) {
        window.dispatchEvent(
          new Event(event.data.direction === "undo" ? documentEvent("undo") : documentEvent("redo"), {
            cancelable: true,
          }),
        );
        return;
      }
      if (isInteractiveOpenDevtools(event.data, sessionId)) {
        void openDevtools();
        return;
      }
      // A find result from the frame — hand it to the preview-find hook to settle its pending promise.
      if (isInteractiveFindResult(event.data, sessionId)) {
        resolveFindResult(event.data.requestId, {
          count: event.data.count,
          activeIndex: event.data.activeIndex,
        });
        return;
      }
      if (isInteractiveInspectMenu(event.data, sessionId)) {
        const rect = iframeRef.current?.getBoundingClientRect();
        openInspectMenu(event.data, {
          x: (rect?.left ?? 0) + event.data.x,
          y: (rect?.top ?? 0) + event.data.y,
        });
        return;
      }
      if (isInteractiveInspectTarget(event.data, sessionId)) {
        handleInspectTarget(event.data);
        return;
      }
      if (isInteractiveContextTarget(event.data, sessionId)) {
        // Native menu is open; just remember the target so "View source" (grafted) can reveal it.
        lastContextTargetRef.current = event.data;
        return;
      }
      if (isInteractiveEditVisual(event.data, sessionId)) {
        // Interactive mode stays read-only; HtmlDocument switches into Safe before creating
        // any draft or editor.
        onEnterVisual?.(event.data);
        return;
      }
      if (isInteractiveEditRefused(event.data, sessionId)) {
        onEditRefused?.(event.data.pagePath);
        return;
      }
      // The frame replied to postInteractivePrint with its own serialized document — print THAT in-flow
      // (documentPrinting.ts) instead of the frame's viewport-sized iframe box.
      if (isInteractivePrintContent(event.data, sessionId)) {
        void printIsolatedHtmlMarkup(event.data.html).then((printed) => {
          if (printed) return;
          logError("print:failed");
          toast("Couldn't open print dialog", "error");
        });
        return;
      }
      if (isInteractiveHtmlLinkHoverMessage(event.data, sessionId)) {
        onLinkHover?.(event.data.href);
        return;
      }
      if (!isInteractiveHtmlLinkMessage(event.data, sessionId)) return;
      void handleHref(event.data.href, event.data.newTab);
    };
    window.addEventListener("message", onMessage);
    const pane = iframeRef.current?.closest("[data-vr-pane-state]");
    // Inactive-tab policy: the frame pauses media and defers timers unless its pane is active and the
    // window visible. Re-posted on pane change, window visibility change and every frame load.
    const postActivity = () => {
      const active =
        document.visibilityState === "visible" &&
        (!pane || pane.getAttribute("data-vr-pane-state") === "active");
      postInteractiveActivity(iframeRef.current, sessionId, active);
    };
    const frameEl = iframeRef.current;
    frameEl?.addEventListener("load", postActivity);
    document.addEventListener("visibilitychange", postActivity);
    postActivity();
    const observer = pane
      ? new MutationObserver(() => {
          postActivity();
          const iframe = iframeRef.current;
          if (iframe?.dataset.vrHtmlSurface)
            publishHtmlSurface(iframe, iframe.dataset.vrHtmlSurface);
        })
      : null;
    if (pane && observer)
      observer.observe(pane, { attributes: true, attributeFilter: ["data-vr-pane-state"] });
    return () => {
      window.removeEventListener("message", onMessage);
      observer?.disconnect();
      frameEl?.removeEventListener("load", postActivity);
      document.removeEventListener("visibilitychange", postActivity);
    };
  }, [
    handleHref,
    onLinkHover,
    sessionId,
    onRequestFind,
    resolveFindResult,
    handleInspectTarget,
    openInspectMenu,
    onEnterVisual,
    onEditRefused,
    iframeRef,
    lastContextTargetRef,
    onPreviewScrollRef,
    scrollYRef,
    setPreviewZoomHud,
    previewZoomTimer,
  ]);

  // The grafted native "View source" item (context_menu.rs) emits this Tauri event — reveal the element
  // the bridge recorded on the right-click that opened the menu.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void onContextViewSource(() => {
      const target = lastContextTargetRef.current;
      if (target) handleInspectTarget(target);
    }).then((un) => {
      if (cancelled) un();
      else unlisten = un;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [handleInspectTarget, lastContextTargetRef]);

  // The grafted native "Open link in new tab" item (context_menu.rs, srcdoc preview) emits this — open
  // the link the bridge recorded on the right-click as a new tab (local doc or VR web tab).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void onContextOpenLinkTab(() => {
      const href = lastContextTargetRef.current?.href;
      if (href) void handleHref(href, true);
    }).then((un) => {
      if (cancelled) un();
      else unlisten = un;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [handleHref, lastContextTargetRef]);

  // Explicit escape hatch: keep normal link following in the host app, but let right-click send a captured
  // link to the OS browser/email handler when the user asks for it.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void onContextOpenLinkExternal(() => {
      const href = lastContextTargetRef.current?.href;
      if (href) void openExternal(href);
    }).then((un) => {
      if (cancelled) un();
      else unlisten = un;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [lastContextTargetRef]);
}
