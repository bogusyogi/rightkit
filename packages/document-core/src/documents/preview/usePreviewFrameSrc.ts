import { useEffect, useState } from "react";
import { errorMessage } from "../../state/errors.js";
import {
  inTauriWebview,
  registeredPreviewSrc,
  revokePreview,
  type PreviewKind,
} from "./previewFrame.js";

export type PreviewFrameSource =
  /** Registration in flight: render no iframe yet. */
  | { mode: "pending" }
  /** Serve from the preview protocol via iframe `src`. */
  | { mode: "src"; src: string; token: string }
  /** Plain browser (vite dev / browser QA, no Tauri protocol): fall back to `srcdoc`. */
  | { mode: "srcdoc" }
  | { mode: "failed"; message: string };

/**
 * Registers `html` with the preview protocol under a fresh one-time token and returns the iframe
 * source. The token is revoked when `html` changes or the component unmounts; between those it stays
 * valid so a webview-initiated frame reload still resolves.
 */
export function usePreviewFrameSrc(kind: PreviewKind, html: string): PreviewFrameSource {
  const native = inTauriWebview();
  const [entry, setEntry] = useState<{
    html: string;
    result: { src: string; token: string } | { message: string };
  } | null>(null);

  useEffect(() => {
    if (!native) return;
    let cancelled = false;
    let token: string | null = null;
    registeredPreviewSrc(kind, html).then(
      (registered) => {
        if (cancelled) {
          void revokePreview(registered.token);
          return;
        }
        token = registered.token;
        setEntry({ html, result: { src: registered.src, token: registered.token } });
      },
      (error) => {
        if (!cancelled) setEntry({ html, result: { message: errorMessage(error) } });
      },
    );
    return () => {
      cancelled = true;
      if (token) void revokePreview(token);
    };
  }, [native, kind, html]);

  if (!native) return { mode: "srcdoc" };
  if (!entry || entry.html !== html) return { mode: "pending" };
  return "src" in entry.result
    ? { mode: "src", src: entry.result.src, token: entry.result.token }
    : { mode: "failed", message: entry.result.message };
}
