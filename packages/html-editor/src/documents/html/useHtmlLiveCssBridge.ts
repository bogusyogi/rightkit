import { useEffect, type RefObject } from "react";
import { extractInlineStyleCss } from "./htmlInteractive";
import { postLiveCss, postInteractiveThemeCss } from "./htmlInteractiveProtocol";
import { htmlThemeStyleCss } from "./htmlTheme";

interface HtmlLiveCssBridgeDeps {
  mode: "interactive" | "preview";
  sessionId: string;
  drawerOpen: boolean;
  /** Live, editable source for the active page. */
  activeBuffer: string;
  iframeRef: RefObject<HTMLIFrameElement | null>;
}

/**
 * The live-CSS hot-apply bridge into the running interactive iframe: theme CSS and inline <style>
 * edits are pushed via postMessage with no reload, so scripts/state survive (matching DevTools'
 * live CSS). Structural edits still rebuild — the debounced live-preview rebuild stays beside the
 * applied snapshot in the component. Extracted from HtmlDocument verbatim — no behavior change.
 */
export function useHtmlLiveCssBridge({
  mode,
  sessionId,
  drawerOpen,
  activeBuffer,
  iframeRef,
}: HtmlLiveCssBridgeDeps) {
  // biome-ignore lint/correctness/useExhaustiveDependencies: refs arrive as stable hook params
  useEffect(() => {
    if (mode !== "interactive") return;
    postInteractiveThemeCss(iframeRef.current, sessionId, htmlThemeStyleCss());
  }, [mode, sessionId]);

  // Live CSS hot-apply (Interactive only): debounce the live buffer, extract its inline <style> CSS,
  // and push it into the running iframe via postMessage — no reload, so scripts/state survive. HTML
  // structure and JS changes still wait for Save (the rebuild above). Mirrors DevTools' live CSS.
  // biome-ignore lint/correctness/useExhaustiveDependencies: refs arrive as stable hook params
  useEffect(() => {
    if (mode !== "interactive" || !drawerOpen) return;
    const id = window.setTimeout(() => {
      const css = extractInlineStyleCss(activeBuffer);
      postLiveCss(iframeRef.current, sessionId, css);
    }, 200);
    return () => window.clearTimeout(id);
  }, [activeBuffer, drawerOpen, mode, sessionId]);
}
