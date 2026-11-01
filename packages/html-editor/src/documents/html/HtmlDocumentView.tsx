import type { SiteRootGrant } from "./htmlInline";
import { useCallback, useEffect, useMemo, useRef, type ReactNode, type RefObject } from "react";
import { HtmlViewer } from "./HtmlViewer";
import { BackToTopButton } from "@rightkit/document-core/documents/BackToTopButton";
import { appColorScheme } from "./htmlInline";
import { PreviewMessage } from "@rightkit/document-core/documents/PreviewMessage";
import { formatFromPath, type SearchProvider } from "@rightkit/document-core/documents/types";
import { sourceChecksum, type HtmlInspectTarget, type HtmlSafeBlockTarget } from "./sourceMap";
import { trackHtmlIframe } from "./htmlResourceLifecycle";
import {
  releaseInspectableHtmlPreview,
  setDevtoolsShortcutScope,
  setInspectableHtmlPreview,
} from "@rightkit/document-core/state/tauriBridge";
import { usePreviewFrameSrc } from "@rightkit/document-core/documents/preview/usePreviewFrameSrc";

function InteractiveHtmlFrame({
  nonce,
  iframeRef,
  srcDoc,
  active,
}: {
  nonce: number;
  iframeRef: RefObject<HTMLIFrameElement | null>;
  srcDoc: string;
  active: boolean;
}) {
  const frameRevision = useMemo(() => sourceChecksum(srcDoc), [srcDoc]);
  // Served from the viewright-preview protocol under its own CSP header (src-tauri/src/
  // preview_protocol.rs), so the main window needs no 'unsafe-eval' or remote script allowance.
  // srcdoc is only the plain-browser (browser QA) fallback.
  const frameSource = usePreviewFrameSrc("interactive", srcDoc);
  const ownsDevtoolsScopeRef = useRef(false);
  const iframeFocusedRef = useRef(false);
  const iframeHoveredRef = useRef(false);
  const syncDevtoolsScope = useCallback(
    (nextActive = active) => {
      const shouldOwn = nextActive && (iframeFocusedRef.current || iframeHoveredRef.current);
      if (shouldOwn === ownsDevtoolsScopeRef.current) return;
      ownsDevtoolsScopeRef.current = shouldOwn;
      void setDevtoolsShortcutScope(shouldOwn);
    },
    [active],
  );
  useEffect(() => {
    trackHtmlIframe(1);
    return () => {
      if (ownsDevtoolsScopeRef.current) void setDevtoolsShortcutScope(false);
      trackHtmlIframe(-1);
    };
  }, []);
  useEffect(() => {
    syncDevtoolsScope(active);
  }, [active, syncDevtoolsScope]);
  // DevTools opens this frame's registered document (separate no-IPC content window).
  const devtoolsToken = frameSource.mode === "src" ? frameSource.token : null;
  useEffect(() => {
    if (!active || !devtoolsToken) return;
    setInspectableHtmlPreview(devtoolsToken);
    return () => releaseInspectableHtmlPreview(devtoolsToken);
  }, [active, devtoolsToken]);
  if (frameSource.mode === "pending") {
    return <div className="h-full w-full" style={{ background: "var(--ink)" }} aria-hidden />;
  }
  if (frameSource.mode === "failed") {
    return <PreviewMessage title="Preview failed" body={frameSource.message} />;
  }
  return (
    <iframe
      // A new applied document owns a new browsing context. Updating the document in place can leave
      // Chromium on the boot document after source/tab transitions. Tab switches keep this key.
      key={`interactive:${nonce}:${frameRevision}`}
      ref={iframeRef}
      title="Interactive HTML preview"
      // Opaque origin: never allow-same-origin for user content.
      sandbox="allow-scripts"
      {...(frameSource.mode === "src" ? { src: frameSource.src } : { srcDoc })}
      onFocus={() => {
        iframeFocusedRef.current = true;
        syncDevtoolsScope();
      }}
      onBlur={() => {
        iframeFocusedRef.current = false;
        syncDevtoolsScope();
      }}
      onMouseEnter={() => {
        iframeHoveredRef.current = true;
        syncDevtoolsScope();
      }}
      onMouseLeave={() => {
        iframeHoveredRef.current = false;
        syncDevtoolsScope();
      }}
      className="h-full w-full border-0"
      style={{ colorScheme: appColorScheme(), background: "var(--ink)" }}
    />
  );
}

/**
 * The rendered preview surface: switches between the opaque-origin interactive iframe (scripts ON)
 * and the sanitized scriptless HtmlViewer (Safe), wrapped in the optional device-frame viewport
 * container, plus the zoom HUD, mode-swap mask, and back-to-top button. Extracted verbatim from
 * HtmlDocument — no behavior change and no change to either iframe's `sandbox`/CSP (see
 * docs/HTML_SECURITY_INVARIANTS.md).
 */
export function HtmlDocumentView({
  mode,
  error,
  nonce,
  srcDoc,
  iframeRef,
  safeIframeRef,
  activePath,
  entryPath,
  siteRoot,
  resourcesReady,
  applied,
  themeKey,
  drawerOpen,
  registerSearchProvider,
  onInspectTarget,
  onInspectMenu,
  onRequestFind,
  navigate,
  onOpenPath,
  viewportWidth,
  previewZoomHud,
  onResetPreviewZoom,
  swapping,
  onBackToTop,
  onSafeBlockTarget,
  onLinkHover,
  overlay,
  active,
}: {
  mode: "interactive" | "preview";
  error: string | null;
  nonce: number;
  srcDoc: string;
  iframeRef: RefObject<HTMLIFrameElement | null>;
  safeIframeRef?: RefObject<HTMLIFrameElement | null>;
  activePath: string;
  entryPath: string;
  /** Granted site root for this tab; forwarded to Safe mode so it resolves subresources the same
   *  way Interactive mode does. */
  siteRoot?: SiteRootGrant | null;
  /** False while native site-root grant resolves; prevents transient tauri:// subresource loads. */
  resourcesReady: boolean;
  applied: string;
  themeKey: string;
  drawerOpen: boolean;
  registerSearchProvider: (provider: SearchProvider | null) => void;
  onInspectTarget: (target: HtmlInspectTarget) => void;
  onInspectMenu: (target: HtmlInspectTarget, point: { x: number; y: number }) => void;
  onRequestFind?: () => void;
  navigate: (path: string, fragment?: string | null) => void;
  onOpenPath?: (path: string, opts?: { background?: boolean }) => void;
  viewportWidth: number | null;
  previewZoomHud: string | null;
  /** Asks the sandboxed frame to drop its own zoom back to 100% — the parent cannot restyle it. */
  onResetPreviewZoom: () => void;
  swapping: boolean;
  onBackToTop: () => void;
  onSafeBlockTarget?: (target: HtmlSafeBlockTarget) => void;
  onLinkHover?: (href: string | null) => void;
  overlay?: ReactNode;
  active: boolean;
}) {
  const previewBody =
    mode === "interactive" ? (
      <InteractiveHtmlFrame nonce={nonce} iframeRef={iframeRef} srcDoc={srcDoc} active={active} />
    ) : (
      <HtmlViewer
        key={`${activePath}:${nonce}`}
        content={applied}
        docPath={activePath}
        rootPath={entryPath}
        siteRoot={siteRoot}
        themeKey={themeKey}
        registerSearchProvider={drawerOpen ? undefined : registerSearchProvider}
        showBackToTop={!drawerOpen}
        onInspectTarget={onInspectTarget}
        onInspectMenu={onInspectMenu}
        onRequestFind={onRequestFind}
        onOpenPath={(p, opts) =>
          formatFromPath(p) === "html" && !opts?.background ? navigate(p) : onOpenPath?.(p, opts)
        }
        onSafeBlockTarget={onSafeBlockTarget}
        onLinkHover={onLinkHover}
        active={active && resourcesReady}
        externalIframeRef={safeIframeRef}
      />
    );

  const preview = error ? (
    <PreviewMessage title="Preview failed" body={error} />
  ) : viewportWidth ? (
    // Browser-style device frame: constrain the preview width, centered on a backdrop.
    <div className="flex min-h-0 min-w-0 flex-1 justify-center overflow-auto bg-ink/50 p-4">
      <div
        className="h-full overflow-hidden rounded-md border border-line bg-ink shadow-2xl"
        style={{ width: viewportWidth, maxWidth: "100%" }}
      >
        {previewBody}
      </div>
    </div>
  ) : (
    previewBody
  );

  return (
    <div className="relative flex min-h-0 min-w-0 flex-1 flex-col">
      {previewZoomHud &&
        (previewZoomHud === "100%" ? (
          <div className="no-print pointer-events-none absolute right-4 top-4 z-30 rounded border border-line bg-panel-2 px-2 py-1 font-mono text-xs text-muted shadow-xl">
            {previewZoomHud}
          </div>
        ) : (
          <button
            type="button"
            onClick={onResetPreviewZoom}
            aria-label="Reset zoom to 100%"
            className="no-print absolute right-4 top-4 z-30 rounded border border-line bg-panel-2 px-2 py-1 font-mono text-xs text-muted shadow-xl hover:text-fg"
          >
            {`${previewZoomHud} · Reset`}
          </button>
        ))}
      {preview}
      {overlay}
      {swapping && <div className="pointer-events-none absolute inset-0 z-20 bg-ink" aria-hidden />}
      {mode === "interactive" && !drawerOpen && !error && <BackToTopButton onClick={onBackToTop} />}
    </div>
  );
}
