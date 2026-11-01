import { useContext, useEffect, useState } from "react";
import DOMPurify from "dompurify";
import { MarkdownRenderTrackerContext } from "./export/MarkdownRenderTracker.js";
import { ensureShikiLanguage, getShikiHighlighter } from "@rightkit/doc-render";
import { SHIKI_DARK_THEME, SHIKI_LIGHT_THEME } from "./shikiThemes.js";
import { writeClipboardText } from "@rightkit/document-core/state/tauriBridge";

// Cache the sanitized highlighted HTML by (lang, code). Highlighting is async, so without this a
// remounted preview (e.g. toggling Markdown Preview <-> Source) re-renders every code block plain
// then flashes to highlighted. Seeded into initial state so a cached block renders highlighted on the
// first paint — no flash. Bounded LRU.
interface HighlightCacheEntry {
  readonly html: string;
  readonly notice: string | null;
}

const HIGHLIGHT_CACHE = new Map<string, HighlightCacheEntry>();
const HIGHLIGHT_CACHE_MAX = 64;
const highlightKey = (code: string, lang: string | null) => `${lang ?? "text"}::${code}`;
function rememberHighlight(key: string, entry: HighlightCacheEntry) {
  HIGHLIGHT_CACHE.set(key, entry);
  if (HIGHLIGHT_CACHE.size > HIGHLIGHT_CACHE_MAX) {
    const oldest = HIGHLIGHT_CACHE.keys().next().value;
    if (oldest !== undefined) HIGHLIGHT_CACHE.delete(oldest);
  }
}

export function CodeBlock({ code, lang }: { code: string; lang: string | null }) {
  const tracker = useContext(MarkdownRenderTrackerContext);
  const [html, setHtml] = useState<string | null>(
    () => HIGHLIGHT_CACHE.get(highlightKey(code, lang))?.html ?? null,
  );
  const [highlightNotice, setHighlightNotice] = useState<string | null>(
    () => HIGHLIGHT_CACHE.get(highlightKey(code, lang))?.notice ?? null,
  );
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    const key = highlightKey(code, lang);
    const cached = HIGHLIGHT_CACHE.get(key);
    if (cached) {
      setHtml(cached.html); // already highlighted — render synchronously, no re-highlight flash
      setHighlightNotice(cached.notice);
      return;
    }
    const finish = tracker?.begin(`Shiki: ${lang || "text"}`);
    let cancelled = false;
    setHtml(null);
    setHighlightNotice(null);
    (async () => {
      try {
        const hl = await getShikiHighlighter();
        const resolvedLanguage = lang ? await ensureShikiLanguage(hl, lang) : null;
        const languageAvailable = resolvedLanguage !== null;
        const out = hl.codeToHtml(code, {
          lang: resolvedLanguage ?? "text",
          themes: { light: SHIKI_LIGHT_THEME, dark: SHIKI_DARK_THEME },
          defaultColor: false, // emits CSS vars for both themes; we switch via [data-theme]
        });
        // Defense-in-depth: the main webview runs script-src 'unsafe-inline', so any HTML injected
        // here could execute. Shiki escapes code text, but we still sanitize before rendering.
        // Verified: DOMPurify preserves Shiki's --shiki-light/--shiki-dark CSS vars + classes.
        const sanitized = DOMPurify.sanitize(out);
        const notice =
          lang && !languageAvailable
            ? `Syntax highlighting is unavailable for “${lang}”; showing plain text.`
            : null;
        rememberHighlight(key, { html: sanitized, notice });
        if (!cancelled) {
          setHtml(sanitized);
          setHighlightNotice(notice);
        }
      } catch (error) {
        tracker?.fail(`Shiki: ${lang || "text"}`, error);
        if (!cancelled) {
          setHtml(null);
          setHighlightNotice("Syntax highlighting could not render; showing source text.");
        }
      } finally {
        finish?.();
      }
    })();
    return () => {
      cancelled = true;
      finish?.();
    };
  }, [code, lang, tracker]);

  const copy = () => {
    void writeClipboardText(code)
      .then(() => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1200);
      })
      .catch(() => {
        // The copy affordance simply stays in its idle state when the clipboard is unavailable.
      });
  };

  return (
    <div
      data-vr-export-state={html ? "ready" : "loading"}
      className="group/code vr-md-code relative my-4 overflow-hidden rounded-lg border border-line"
    >
      {/* Language as a corner label (polish pass) — no full-width header strip; the code surface
          keeps top padding so the first line clears the overlay. */}
      <div className="pointer-events-none absolute right-2.5 top-1.5 z-10 flex items-center gap-2 font-mono text-chrome-xs text-muted-2">
        <span>{lang || "text"}</span>
        <button
          type="button"
          data-vr-export-ignore=""
          onClick={copy}
          title="Copy code"
          aria-label="Copy code"
          className="vr-press vr-focus pointer-events-auto rounded opacity-0 transition-opacity hover:text-copper-hi group-hover/code:opacity-100 focus:opacity-100"
        >
          {copied ? "copied" : "copy"}
        </button>
      </div>
      {highlightNotice && (
        <p role="status" className="vr-protected-render-alert px-3 pb-1">
          {highlightNotice}
        </p>
      )}
      {html ? (
        <div
          className="shiki-host overflow-auto pt-5"
          // biome-ignore lint/security/noDangerouslySetInnerHtml: Shiki returns escaped token markup for code highlighting.
          dangerouslySetInnerHTML={{ __html: html }}
        />
      ) : (
        <pre className="overflow-auto bg-panel p-3 pt-7 font-mono text-sm text-paper/85">
          <code>{code}</code>
        </pre>
      )}
    </div>
  );
}
