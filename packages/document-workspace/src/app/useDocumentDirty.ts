import { useCallback, type MutableRefObject } from "react";
import {
  displayTitle,
  isUntitled,
  untitledTitle,
  type LoadedDocument,
} from "@rightkit/document-core/documents/types";
import { isWebDocument } from "@rightkit/document-core/documents/web/webUrl";
import type { BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import type { DocumentTabs } from "./useDocumentTabs";

/** Dirty/title/draft read helpers (pure reads over tab refs + the block-draft registry). */
export function useDocumentDirty(deps: {
  blockDrafts: BlockDraftRegistry;
  pdfDirtyRef: DocumentTabs["pdfDirtyRef"];
  docxDirtyRef: DocumentTabs["docxDirtyRef"];
  imageDirtyRef: DocumentTabs["imageDirtyRef"];
  editsRef: MutableRefObject<Record<string, string>>;
  htmlPagesRef: DocumentTabs["htmlPagesRef"];
}) {
  const { blockDrafts, docxDirtyRef, pdfDirtyRef, editsRef, imageDirtyRef, htmlPagesRef } = deps;

  const hasModifiedDraft = useCallback(
    (path: string) => blockDrafts.snapshotModified(path).length > 0,
    [blockDrafts],
  );

  const snapshotModified = useCallback(
    (path?: string) => blockDrafts.snapshotModified(path),
    [blockDrafts],
  );

  const isDirty = useCallback(
    (doc: LoadedDocument) => {
      if (isWebDocument(doc)) return false;
      if (doc.format === "pdf") return pdfDirtyRef.current.has(doc.path);
      // An open block draft counts as unsaved work (close/quit must resolve it).
      if (hasModifiedDraft(doc.path)) return true;
      if (doc.format === "docx") return docxDirtyRef.current.has(doc.path);
      if (doc.format === "image") return imageDirtyRef.current.has(doc.path);
      // Read the synchronous mirrors so a save/close in the same tick as a keystroke sees the latest
      // text. Reactivity is preserved because every edit also calls a setter, which re-renders.
      const entryDirty = (editsRef.current[doc.path] ?? doc.content) !== doc.content;
      if (doc.format === "html") {
        if (entryDirty) return true;
        // An HTML tab is also dirty if any navigated sub-page in its session has unsaved edits.
        const session = htmlPagesRef.current[doc.path];
        if (session)
          for (const p in session) if (session[p].edited !== session[p].original) return true;
        return false;
      }
      return entryDirty;
    },
    [hasModifiedDraft, docxDirtyRef, pdfDirtyRef, editsRef, imageDirtyRef, htmlPagesRef],
  );

  // Reads editsRef.current (a live ref). Keystrokes never re-render App; the expanded-sidebar rows +
  // the titlebar get LIVE untitled titles by subscribing to this doc's edit slice (LiveTitle/useEditText),
  // which re-renders just that label and re-invokes titleFor with the fresh ref value.
  const titleFor = useCallback(
    (doc: LoadedDocument) =>
      isWebDocument(doc)
        ? doc.displayName
        : isUntitled(doc.path)
          ? untitledTitle(editsRef.current[doc.path] ?? "")
          : displayTitle(doc.path),
    [editsRef],
  );

  return { hasModifiedDraft, snapshotModified, isDirty, titleFor };
}
