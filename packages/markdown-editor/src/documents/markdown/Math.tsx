import { useContext, useEffect, useState } from "react";
import DOMPurify from "dompurify";
// Static side-effect import so Vite reliably injects KaTeX's stylesheet — a dynamic import() of the
// CSS wasn't being injected, which left super/subscripts and fraction limits overlapping. This
// loads with the (already lazy) markdown chunk, so cold launch / non-markdown opens don't pay for it.
import { MarkdownRenderTrackerContext } from "./export/MarkdownRenderTracker.js";

// Lazy-load the KaTeX engine only when a document actually contains math.
let katexPromise: Promise<typeof import("katex").default> | null = null;
function getKatex() {
  if (!katexPromise) {
    katexPromise = import("katex").then((m) => m.default);
  }
  return katexPromise;
}

export function MathBlock({ value, display }: { value: string; display: boolean }) {
  const tracker = useContext(MarkdownRenderTrackerContext);
  const [html, setHtml] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const finish = tracker?.begin("KaTeX");
    let cancelled = false;
    setHtml(null);
    setError(null);
    getKatex()
      .then((katex) => {
        if (cancelled) return;
        // Defense-in-depth: main webview runs script-src 'unsafe-inline'. KaTeX output is normally
        // safe, but we sanitize before injecting. Verified: DOMPurify's default config preserves
        // KaTeX's katex-html visual layer, MathML accessibility layer, SVG, and inline styles.
        setHtml(
          DOMPurify.sanitize(
            katex.renderToString(value, { displayMode: display, throwOnError: true }),
          ),
        );
      })
      .catch((error) => {
        tracker?.fail("KaTeX", error);
        if (!cancelled) {
          setHtml(null);
          setError(`Math could not render: ${String(error?.message ?? error)}`);
        }
      })
      .finally(() => finish?.());
    return () => {
      cancelled = true;
      finish?.();
    };
  }, [value, display, tracker]);

  if (html == null) {
    // Fallback to raw TeX while loading / on error.
    return display ? (
      <div data-vr-export-state={error ? "error" : "loading"} className="my-3 overflow-auto">
        {error && (
          <p role="alert" className="vr-protected-render-alert">
            {error}
          </p>
        )}
        <pre className="font-mono text-sm text-muted">{value}</pre>
      </div>
    ) : (
      <span data-vr-export-state={error ? "error" : "loading"}>
        {error && (
          <span role="alert" className="vr-protected-render-alert">
            {error}
          </span>
        )}
        <code className="font-mono text-sm text-muted">{value}</code>
      </span>
    );
  }
  return display ? (
    <div
      data-vr-export-state="ready"
      className="my-3 overflow-auto text-center"
      // biome-ignore lint/security/noDangerouslySetInnerHtml: KaTeX output is sanitized above with DOMPurify before injection.
      dangerouslySetInnerHTML={{ __html: html }}
    />
  ) : (
    <span
      data-vr-export-state="ready"
      // biome-ignore lint/security/noDangerouslySetInnerHtml: KaTeX output is sanitized above with DOMPurify before injection.
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
