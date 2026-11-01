import { useLayoutEffect } from "react";
import { MarkdownDocumentBody } from "../MarkdownDocumentBody";
import type { MarkdownCoreSnapshot } from "../core/MarkdownDocumentTypes";
import { type MarkdownRenderTracker, MarkdownRenderTrackerContext } from "./MarkdownRenderTracker";

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
