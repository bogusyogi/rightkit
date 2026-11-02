import { useLayoutEffect } from "react";
import { MarkdownDocumentBody } from "../MarkdownDocumentBody.js";
import type { MarkdownCoreSnapshot } from "../core/MarkdownDocumentTypes.js";
import { type MarkdownRenderTracker, MarkdownRenderTrackerContext } from "./MarkdownRenderTracker.js";

export function MarkdownExportDocument({
  snapshot,
  documentPath,
  tracker,
  safeHtmlEnabled,
  onMounted,
}: {
  snapshot: MarkdownCoreSnapshot;
  documentPath?: string;
  tracker: MarkdownRenderTracker;
  safeHtmlEnabled: boolean;
  onMounted: () => void;
}) {
  useLayoutEffect(onMounted, [onMounted]);
  return (
    <MarkdownRenderTrackerContext.Provider value={tracker}>
      <MarkdownDocumentBody
        snapshot={snapshot}
        documentPath={documentPath}
        showOutline={false}
        showBackToTop={false}
        interactiveTasks={false}
        safeHtmlEnabled={safeHtmlEnabled}
        exportMode
      />
    </MarkdownRenderTrackerContext.Provider>
  );
}
