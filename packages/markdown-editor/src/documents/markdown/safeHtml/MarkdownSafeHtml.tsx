import { createContext, useContext, useEffect, useMemo, useRef, type ReactNode } from "react";
import { readRelativeAsset } from "@rightkit/document-core/state/tauriBridge";
import { MarkdownRenderTrackerContext } from "../export/MarkdownRenderTracker";
import { sanitizeMarkdownHtml } from "../security/sanitizeMarkdownHtml";

export const MarkdownSafeHtmlContext = createContext({
  enabled: false,
  documentPath: undefined as string | undefined,
});

export function MarkdownSafeHtml({ raw }: { raw: string }): ReactNode {
  const { enabled, documentPath } = useContext(MarkdownSafeHtmlContext);
  const tracker = useContext(MarkdownRenderTrackerContext);
  const rootRef = useRef<HTMLDivElement>(null);
  const html = useMemo(() => (enabled ? sanitizeMarkdownHtml(raw) : ""), [enabled, raw]);

  useEffect(() => {
    if (!enabled || !html || !documentPath || !rootRef.current) return;
    let cancelled = false;
    const finishes: Array<() => void> = [];
    const images = Array.from(
      rootRef.current.querySelectorAll<HTMLImageElement>("img[data-vr-local-src]"),
    );
    for (const image of images) {
      const path = image.dataset.vrLocalSrc;
      if (!path) continue;
      const finish = tracker?.begin(`Safe HTML image: ${path}`);
      if (finish) finishes.push(finish);
      void readRelativeAsset(documentPath, path)
        .then((asset) => {
          if (!cancelled && asset) image.src = `data:${asset.mime};base64,${asset.base64}`;
        })
        .catch((error) => tracker?.fail(`Safe HTML image: ${path}`, error))
        .finally(() => finish?.());
    }
    return () => {
      cancelled = true;
      finishes.forEach((finish) => finish());
    };
  }, [documentPath, enabled, html, tracker]);

  if (!enabled || !html) return null;
  // Sanitized by the sanitizeMarkdownHtml allowlist; raw Markdown source is never modified.
  // biome-ignore lint/security/noDangerouslySetInnerHtml: DOMPurify fixed allowlist.
  return <div ref={rootRef} data-vr-safe-html dangerouslySetInnerHTML={{ __html: html }} />;
}
