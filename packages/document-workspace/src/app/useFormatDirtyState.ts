import { useCallback, useRef, type Dispatch, type SetStateAction } from "react";
import { writeDocumentBytes } from "@rightkit/document-core/state/tauriBridge";
import { errorMessage } from "@rightkit/document-core/state/errors";
import type { ImageSaveFunction, ImageSavePayload } from "@rightkit/media-ui";
import type { PdfSaveFunction } from "@rightkit/document-core/documents/pdf/pdfEditor";
import type { PdfSavePlan } from "@rightkit/document-core/documents/pdf/pdfEditModel";

interface FormatDirtyStateDeps {
  setError: Dispatch<SetStateAction<string | null>>;
  setDocxDirty: Dispatch<SetStateAction<Set<string>>>;
  setPdfDirty: Dispatch<SetStateAction<Set<string>>>;
  setImageDirty: Dispatch<SetStateAction<Set<string>>>;
  markSelfSaved: (path: string) => void;
  refreshFingerprint: (path: string) => void;
  onDirty?: (path: string) => void;
}

export function useFormatDirtyState({
  setError,
  setDocxDirty,
  setPdfDirty,
  setImageDirty,
  markSelfSaved,
  refreshFingerprint,
  onDirty,
}: FormatDirtyStateDeps) {
  const docxSaver = useRef<(() => Promise<ArrayBuffer | null>) | null>(null);
  const pdfSaversRef = useRef<Map<string, PdfSaveFunction>>(new Map());
  const pdfSavePlansRef = useRef<Map<string, () => Promise<PdfSavePlan>>>(new Map());
  const imageSaver = useRef<ImageSaveFunction | null>(null);
  const docxSaversRef = useRef<Map<string, () => Promise<ArrayBuffer | null>>>(new Map());
  const imageSaversRef = useRef<Map<string, ImageSaveFunction>>(new Map());
  // One dirty-set pattern for DOCX and image: a path is added or removed through these helpers,
  // each a no-op when the set already matches (so repeated edits never churn state).
  const addDirtyPath = useCallback(
    (setter: Dispatch<SetStateAction<Set<string>>>, path: string) => {
      setter((s) => (s.has(path) ? s : new Set(s).add(path)));
    },
    [],
  );
  const removeDirtyPath = useCallback(
    (setter: Dispatch<SetStateAction<Set<string>>>, path: string) => {
      setter((s) => {
        if (!s.has(path)) return s;
        const n = new Set(s);
        n.delete(path);
        return n;
      });
    },
    [],
  );

  const onDocxSave = useCallback(
    async (path: string, buffer: ArrayBuffer) => {
      try {
        await writeDocumentBytes(path, buffer);
        markSelfSaved(path);
        refreshFingerprint(path);
        removeDirtyPath(setDocxDirty, path);
      } catch (e) {
        setError(errorMessage(e));
      }
    },
    [markSelfSaved, refreshFingerprint, removeDirtyPath, setDocxDirty, setError],
  );

  const onDocxChange = useCallback(
    (path: string) => {
      addDirtyPath(setDocxDirty, path);
      onDirty?.(path);
    },
    [addDirtyPath, onDirty, setDocxDirty],
  );

  const onPdfDirtyChange = useCallback(
    (path: string, dirty: boolean) => {
      setPdfDirty((paths) => {
        if (dirty === paths.has(path)) return paths;
        const next = new Set(paths);
        if (dirty) next.add(path);
        else next.delete(path);
        return next;
      });
      if (dirty) onDirty?.(path);
    },
    [onDirty, setPdfDirty],
  );

  const onImageChange = useCallback(
    (path: string) => {
      addDirtyPath(setImageDirty, path);
      onDirty?.(path);
    },
    [addDirtyPath, onDirty, setImageDirty],
  );

  const clearImageDirty = useCallback(
    (path: string) => removeDirtyPath(setImageDirty, path),
    [removeDirtyPath, setImageDirty],
  );

  const restoreImageDirty = useCallback(
    (path: string) => addDirtyPath(setImageDirty, path),
    [addDirtyPath, setImageDirty],
  );

  const writeImagePayload = useCallback(
    async (path: string, payload: ImageSavePayload) => {
      await writeDocumentBytes(path, payload.buffer);
      markSelfSaved(path);
      refreshFingerprint(path);
      removeDirtyPath(setImageDirty, path);
    },
    [markSelfSaved, refreshFingerprint, removeDirtyPath, setImageDirty],
  );

  return {
    docxSaver,
    imageSaver,
    docxSaversRef,
    pdfSaversRef,
    pdfSavePlansRef,
    imageSaversRef,
    onDocxSave,
    onDocxChange,
    onPdfDirtyChange,
    onImageChange,
    clearImageDirty,
    restoreImageDirty,
    writeImagePayload,
  };
}
