import { useEffect, useRef } from "react";
import { clearImageEditSnapshot } from "@rightkit/media-ui";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import { inTauri } from "@rightkit/document-core/state/tauriBridge";
import { isMac } from "@rightkit/document-core/platform";

/** Auto-evict per-path caches when tabs close: compare previous openDocs to current and evict removed
 * paths. Extracted verbatim from App.tsx. */
export function useClosedTabEviction({
  openDocs,
  onEvictPath,
  dropHtmlProps,
  sourceViewStateRef,
  pruneClosedPaths,
}: {
  openDocs: LoadedDocument[];
  onEvictPath: (path: string) => void;
  dropHtmlProps: (path: string) => void;
  sourceViewStateRef: React.MutableRefObject<Record<string, unknown>>;
  pruneClosedPaths: (paths: string[]) => void;
}) {
  const prevOpenDocsRef = useRef(openDocs);
  useEffect(() => {
    const prev = prevOpenDocsRef.current;
    if (prev !== openDocs) {
      const current = new Set(openDocs.map((d) => d.path));
      const closedPaths: string[] = [];
      for (const doc of prev) {
        if (!current.has(doc.path)) {
          onEvictPath(doc.path);
          dropHtmlProps(doc.path);
          clearImageEditSnapshot(doc.path);
          // A PDF pane can unmount merely because its router leaves the live-tab budget.
          // Only removal from openDocs is terminal for its parked native reader.
          if (doc.format === "pdf" && inTauri() && isMac()) {
            void import("@rightkit/document-core/documents/pdf/macPdfNativeBridge")
              .then(({ nativePdfClosePath }) => nativePdfClosePath(doc.path))
              .catch(() => undefined);
          }
          delete sourceViewStateRef.current[doc.path];
          closedPaths.push(doc.path);
        }
      }
      // T5-2: drop closed tabs' rendered/live/recent keep-alive markers too — owned by
      // usePaneLiveness (see the hook for why each set must be pruned).
      if (closedPaths.length) {
        pruneClosedPaths(closedPaths);
      }
      prevOpenDocsRef.current = openDocs;
    }
  }, [openDocs, pruneClosedPaths, dropHtmlProps, onEvictPath, sourceViewStateRef]);
}
