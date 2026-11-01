import { useLayoutEffect } from "react";
import { getCurrentWindow, type Color } from "@tauri-apps/api/window";
import { logError, logPerf } from "@rightkit/document-core/state/diagnostics";

// Captured at module load (early in the import graph) so app:shown measures from near-boot.
const bootStartedAt = performance.now();
let shown = false;

// WebView2 can briefly reveal the native surface while maximizing. Match it to the selected reading
// pane so exposed resize pixels blend into the document instead of flashing white.
function nativeColor(value: string): Color {
  const channels = value.match(
    /^rgba?\(\s*([\d.]+)[,\s]+([\d.]+)[,\s]+([\d.]+)(?:\s*[,/]\s*([\d.]+%?))?\s*\)$/i,
  );
  if (!channels) return value;
  const byte = (channel: string) => Math.round(Math.max(0, Math.min(255, Number(channel))));
  const alpha = channels[4]
    ? Math.round(
        Math.max(
          0,
          Math.min(
            1,
            channels[4].endsWith("%")
              ? Number(channels[4].slice(0, -1)) / 100
              : Number(channels[4]),
          ),
        ) * 255,
      )
    : 255;
  return [byte(channels[1]), byte(channels[2]), byte(channels[3]), alpha];
}

function currentReaderBackground(): Color {
  try {
    const htmlSurface = document
      .querySelector<HTMLIFrameElement>(
        '[data-vr-pane-state="active"] iframe[data-vr-html-surface]',
      )
      ?.dataset.vrHtmlSurface?.trim();
    if (htmlSurface) return nativeColor(htmlSurface);
    const v = getComputedStyle(document.documentElement)
      .getPropertyValue("--reader-surface")
      .trim();
    return v || "#F7F3EC";
  } catch {
    return "#F7F3EC";
  }
}

async function setNativeBackground(): Promise<void> {
  await getCurrentWindow().setBackgroundColor(currentReaderBackground());
}

function showWindow(beforeShow?: () => void | Promise<void>) {
  if (shown) return;
  shown = true;
  void Promise.resolve()
    .then(async () => {
      const win = getCurrentWindow();
      // Set the native surface to the theme color FIRST, then reveal — so even the single composite
      // between show() and the webview's first paint is themed, never white.
      try {
        await win.setBackgroundColor(currentReaderBackground());
      } catch {
        if ("__TAURI_INTERNALS__" in window) {
          logError("window:background-color-failed");
        }
      }
      let resizeStartedAt: number | undefined;
      let resizeTimer: number | undefined;
      void win
        .onResized(() => {
          resizeStartedAt ??= performance.now();
          window.clearTimeout(resizeTimer);
          resizeTimer = window.setTimeout(() => {
            const startedAt = resizeStartedAt;
            if (startedAt === undefined) return;
            logPerf("window:resize", startedAt);
            resizeStartedAt = undefined;
          }, 150);
        })
        .then((unlisten) => window.addEventListener("beforeunload", unlisten, { once: true }))
        .catch(() => {
          if ("__TAURI_INTERNALS__" in window) {
            logError("window:resize-listener-failed");
          }
        });
      // Caller-supplied pre-reveal step (e.g. the legal gate resizing/centering the window to its
      // own 960x560 before it becomes visible) so the FIRST thing the user ever sees is already the
      // right size — never a flash of the full app window followed by a shrink.
      if (beforeShow) {
        try {
          await beforeShow();
        } catch {
          // Best-effort — a failed resize must never block the reveal itself.
        }
      }
      await win.show();
    })
    .then(() => logPerf("app:shown", bootStartedAt))
    .catch(() => {
      // Browser QA has no Tauri window; the app remains visible in that environment.
    });
}

/**
 * The window launches hidden (tauri.conf.json `"visible": false`) and is revealed the instant React
 * commits its first frame. We use useLayoutEffect: it runs synchronously right after the DOM is
 * committed and — unlike requestAnimationFrame — fires even while the window is hidden. A hidden
 * macOS WKWebView never schedules rAF (no compositing off-screen), so the old rAF-gated reveal always
 * fell through to the safety timeout and cost ~4s on every macOS launch. Committing-then-showing is
 * deterministic and instant on both macOS and Windows; the native surface is pre-painted to the theme
 * color in showWindow() so the reveal never flashes white. Idempotent (shows once) so StrictMode's
 * double-invoke in dev is harmless.
 *
 */

export function useReadyShow(beforeShow?: () => void | Promise<void>) {
  useLayoutEffect(() => {
    showWindow(beforeShow);
    const isTauri = "__TAURI_INTERNALS__" in window;
    let color = JSON.stringify(currentReaderBackground());
    const refreshBackground = () => {
      const next = currentReaderBackground();
      const serialized = JSON.stringify(next);
      if (serialized === color) return;
      color = serialized;
      void setNativeBackground().catch(() => {
        if (isTauri) logError("window:background-color-failed");
      });
    };
    const rootObserver = new MutationObserver(refreshBackground);
    rootObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["style"],
    });
    const paneObserver = new MutationObserver(refreshBackground);
    paneObserver.observe(document.documentElement, {
      attributes: true,
      subtree: true,
      attributeFilter: ["data-vr-pane-state", "data-vr-html-surface"],
    });
    // beforeShow is intentionally a dependency: re-running this effect after the window has already
    // shown is a harmless no-op (`showWindow`'s module-level `shown` guard short-circuits it), so callers
    // that pass a beforeShow whose identity changes (e.g. AppRoot's resize callback, stable via
    // useCallback but re-created when the legal-gate/app decision flips) never risk a stale closure.
    return () => {
      rootObserver.disconnect();
      paneObserver.disconnect();
    };
  }, [beforeShow]);
}
