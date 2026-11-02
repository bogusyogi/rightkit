import {
  useCallback,
  useRef,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { LoadedDocument, ViewMode } from "@rightkit/document-core/documents/types";
import type { ImageSaveFunction } from "@rightkit/media-ui";
import type { DocumentTabs } from "./useDocumentTabs.js";
import type { BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import { errorMessage } from "@rightkit/document-core/state/errors";
import {
  captureSetupRecovery,
  clearSetupRecoverySnapshot,
  loadSetupRecoverySnapshot,
  saveSetupRecoverySnapshot,
} from "./setupRecovery.js";
import { resolveAllDrafts } from "./draftResolution.js";

interface SetupRecoveryDeps {
  blockDrafts: BlockDraftRegistry;
  setError: Dispatch<SetStateAction<string | null>>;
  openDocsRef: MutableRefObject<LoadedDocument[]>;
  activePathRef: MutableRefObject<string | null>;
  viewsRef: MutableRefObject<Record<string, ViewMode>>;
  isDirty: (doc: LoadedDocument) => boolean;
  editsRef: DocumentTabs["editsRef"];
  htmlPagesRef: DocumentTabs["htmlPagesRef"];
  docxSaversRef: MutableRefObject<Map<string, () => Promise<ArrayBuffer | null>>>;
  imageSaversRef: MutableRefObject<Map<string, ImageSaveFunction>>;
  setOpenDocs: DocumentTabs["setOpenDocs"];
  writeEdits: DocumentTabs["writeEdits"];
  writeHtmlPages: DocumentTabs["writeHtmlPages"];
  setViews: Dispatch<SetStateAction<Record<string, ViewMode>>>;
  setDocxDirty: DocumentTabs["setDocxDirty"];
  restoreImageDirty: (path: string) => void;
  fingerprints: MutableRefObject<Map<string, { mtimeMs: number; size: number }>>;
  withTabKey: <T extends LoadedDocument>(doc: T, existingKey?: string) => T;
}

/**
 * Setup-replacement recovery: snapshot every open dirty document (committed sources only — open
 * block drafts are resolved first so the snapshot never carries a half-typed draft) before the
 * installer destroys the window, then rehydrate documents/edits/views/dirty flags on the next
 * launch when the snapshot is present. The pure snapshot (de)serialization lives in
 * setupRecovery.ts; this hook owns the refs/callbacks that feed it and the re-entrant guard.
 * Extracted verbatim from useDocuments; bodies are unchanged.
 */
export function useSetupRecovery({
  blockDrafts,
  setError,
  openDocsRef,
  activePathRef,
  viewsRef,
  isDirty,
  editsRef,
  htmlPagesRef,
  docxSaversRef,
  imageSaversRef,
  setOpenDocs,
  writeEdits,
  writeHtmlPages,
  setViews,
  setDocxDirty,
  restoreImageDirty,
  fingerprints,
  withTabKey,
}: SetupRecoveryDeps) {
  const setupPreparingRef = useRef(false);
  const prepareSetupRecovery = useCallback(async () => {
    if (setupPreparingRef.current) return;
    setupPreparingRef.current = true;
    try {
      // Setup replacement resolves every open block draft before snapshotting, so the recovery
      // snapshot carries committed source — never a half-typed draft.
      const outcome = await resolveAllDrafts(blockDrafts, "setup-replacement", "done");
      if (outcome !== "resolved") {
        setupPreparingRef.current = false;
        setError("Setup could not preserve an open block edit.");
        return;
      }
      const snapshot = await captureSetupRecovery({
        openDocs: openDocsRef.current,
        activePath: activePathRef.current,
        views: viewsRef.current,
        isDirty,
        editsRef,
        htmlPagesRef,
        docxSaversRef,
        imageSaversRef,
      });
      await saveSetupRecoverySnapshot(snapshot);
      await getCurrentWindow().destroy();
    } catch (e) {
      setupPreparingRef.current = false;
      setError(`Setup could not preserve unsaved changes: ${errorMessage(e)}`);
    }
  }, [
    docxSaversRef,
    editsRef,
    htmlPagesRef,
    imageSaversRef,
    isDirty,
    viewsRef,
    blockDrafts,
    openDocsRef,
    activePathRef,
    setError,
  ]);

  const restoreSetupRecovery = useCallback(async (): Promise<string | null> => {
    const snapshot = await loadSetupRecoverySnapshot();
    if (!snapshot) return null;
    try {
      const restored = snapshot.documents.map((entry) => {
        const document = { ...entry.document } as LoadedDocument;
        if (entry.document.format === "docx") {
          if (!entry.binary?.base64)
            throw new Error(`Recovery data is missing for ${entry.document.displayName}.`);
          document.content = entry.binary.base64;
        } else if (entry.document.format === "image") {
          if (!entry.binary?.base64 || !entry.binary.mime)
            throw new Error(`Recovery data is missing for ${entry.document.displayName}.`);
          document.content = `data:${entry.binary.mime};base64,${entry.binary.base64}`;
        }
        return { entry, document };
      });

      setOpenDocs((prev) => {
        let next = prev;
        for (const { document } of restored) {
          const existing = next.find((candidate) => candidate.path === document.path);
          const recovered = withTabKey(document, existing?.tabKey);
          next = existing
            ? next.map((candidate) => (candidate.path === document.path ? recovered : candidate))
            : [...next, recovered];
        }
        return next;
      });
      writeEdits((prev) => {
        const next = { ...prev };
        for (const { entry, document } of restored) {
          if (entry.editedText !== undefined) next[document.path] = entry.editedText;
        }
        return next;
      });
      writeHtmlPages((prev) => {
        const next = { ...prev };
        for (const { entry, document } of restored) {
          if (entry.htmlPages) next[document.path] = entry.htmlPages;
        }
        return next;
      });
      setViews((prev) => {
        const next = { ...prev };
        for (const { entry, document } of restored) {
          if (entry.view) next[document.path] = entry.view;
        }
        viewsRef.current = next;
        return next;
      });
      setDocxDirty((prev) => {
        const next = new Set(prev);
        for (const { document } of restored)
          if (document.format === "docx") next.add(document.path);
        return next;
      });
      for (const { document } of restored) {
        if (document.format === "image") restoreImageDirty(document.path);
        if (document.fingerprint) fingerprints.current.set(document.path, document.fingerprint);
      }
      const restoredPaths = new Set(restored.map(({ document }) => document.path));
      const active =
        snapshot.activePath && restoredPaths.has(snapshot.activePath)
          ? snapshot.activePath
          : activePathRef.current
            ? null
            : (restored[0]?.document.path ?? null);
      await clearSetupRecoverySnapshot();
      return active;
    } catch (e) {
      setError(`Setup recovery could not be restored: ${errorMessage(e)}`);
      return null;
    }
  }, [
    restoreImageDirty,
    setDocxDirty,
    setOpenDocs,
    setViews,
    viewsRef,
    withTabKey,
    writeEdits,
    writeHtmlPages,
    activePathRef,
    fingerprints,
    setError,
  ]);

  return { prepareSetupRecovery, restoreSetupRecovery };
}
