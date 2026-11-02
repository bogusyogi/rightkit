import { useCallback, type Dispatch, type MutableRefObject, type SetStateAction } from "react";
import { docTitle, isUntitled, type LoadedDocument, type ViewMode } from "@rightkit/document-core/documents/types";
import { bufferToBase64, createBlankDocxBuffer } from "@rightkit/docx-editor/documents/docx/blankDocx";
import {
  clearDraft,
  clearFileDraft,
  writeDraftDebounced,
  writeFileDraftDebounced,
} from "./documentDrafts.js";
import { toast } from "@rightkit/platform-ui/headless";
import type { DocumentTabs } from "./useDocumentTabs.js";

/** New-untitled / duplicate-as-draft creation and the txt keystroke path (edits + draft persistence). */
export function useDocumentCreation(deps: {
  untitledSeq: MutableRefObject<number>;
  withTabKey: <T extends LoadedDocument>(doc: T, existingKey?: string) => T;
  setOpenDocs: DocumentTabs["setOpenDocs"];
  setActivePath: DocumentTabs["setActivePath"];
  setViews: Dispatch<SetStateAction<Record<string, ViewMode>>>;
  writeEdits: DocumentTabs["writeEdits"];
  writeEditsLive: DocumentTabs["writeEditsLive"];
  queueAutoSave: (path: string) => void;
}) {
  const {
    untitledSeq,
    withTabKey,
    setOpenDocs,
    setActivePath,
    setViews,
    writeEdits,
    writeEditsLive,
    queueAutoSave,
  } = deps;

  const newDocument = useCallback(
    (format: "txt" | "md" | "docx" = "txt") => {
      const path = `untitled:${untitledSeq.current++}`;
      const content = format === "docx" ? bufferToBase64(createBlankDocxBuffer()) : "";
      const doc: LoadedDocument = withTabKey({
        path,
        displayName: docTitle(path),
        format,
        content,
      });
      setOpenDocs((prev) => [...prev, doc]);
      if (format !== "docx") writeEdits((e) => ({ ...e, [path]: "" }));
      setViews((v) => ({ ...v, [path]: "preview" }));
      setActivePath(path);
    },
    [untitledSeq, writeEdits, setOpenDocs, setViews, setActivePath, withTabKey],
  );

  // Duplicate a document's text into a new, never-saved draft (Word-style "make a copy") — leaves the
  // original untouched on disk; the copy is a scratch buffer the user can edit and Save As later.
  const duplicateAsDraft = useCallback(
    (text: string, toastMsg = "Duplicated as a draft") => {
      const path = `untitled:${untitledSeq.current++}`;
      const doc: LoadedDocument = withTabKey({
        path,
        displayName: docTitle(path),
        format: "txt",
        content: "",
      });
      setOpenDocs((prev) => [...prev, doc]);
      writeEdits((e) => ({ ...e, [path]: text }));
      setViews((v) => ({ ...v, [path]: "preview" }));
      setActivePath(path);
      toast(toastMsg, "success");
    },
    [untitledSeq, writeEdits, setOpenDocs, setViews, setActivePath, withTabKey],
  );

  const onTxtChange = useCallback(
    (path: string, content: string, text: string) => {
      // Keystroke path: writeEditsLive mutates editsRef + notifies only this path's live subscribers
      // (the bottom-bar word-count/dirty and this tab's title) — App does NOT re-render. The save/dirty
      // path reads editsRef, so it always sees the latest text.
      writeEditsLive(path, text);
      queueAutoSave(path);
      // Untitled buffers: persist to draftKey so they restore on next launch. The write is debounced
      // (T2-1) — clearDraft cancels any pending debounced write so an equal-to-saved edit can't be
      // resurrected by a stale write landing after the clear.
      if (isUntitled(path)) {
        if (text === content) clearDraft(path);
        else writeDraftDebounced(path, text);
        return;
      }
      // File-backed txt (Notepad-style): persist edit + the disk base so we can detect stale drafts.
      if (text === content) clearFileDraft(path);
      else writeFileDraftDebounced(path, { base: content, text });
    },
    [queueAutoSave, writeEditsLive],
  );

  return { newDocument, duplicateAsDraft, onTxtChange };
}
