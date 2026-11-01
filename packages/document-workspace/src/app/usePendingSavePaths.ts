import { useCallback, useMemo, useRef, useState } from "react";

interface PendingSavePathsDeps {
  /** The raw DOCX persist (useFormatDirtyState's onDocxSave) — wrapped here with pending tracking. */
  persistDocx: (path: string, buffer: ArrayBuffer) => Promise<void>;
  /** useDocumentSaver's own pending set (text/image saves in flight). */
  saverPendingSavePaths: ReadonlySet<string>;
}

/**
 * The union of all in-flight save paths across the three save surfaces — the saver's own pending
 * set (text/image writes), tracked DOCX binary writes, and per-page HTML session writes — exposed
 * as `pendingSavePaths` so the shell keeps a tab's router pinned while a save is in flight.
 * Extracted verbatim from useDocuments; bodies are unchanged.
 */
export function usePendingSavePaths({ persistDocx, saverPendingSavePaths }: PendingSavePathsDeps) {
  const pendingDocxSaveCounts = useRef<Map<string, number>>(new Map());
  const [pendingDocxSavePaths, setPendingDocxSavePaths] = useState<Set<string>>(() => new Set());
  const pendingHtmlSaveCounts = useRef<Map<string, number>>(new Map());
  const [pendingHtmlSavePaths, setPendingHtmlSavePaths] = useState<Set<string>>(() => new Set());
  const pendingSavePathsRef = useRef<ReadonlySet<string>>(new Set());
  const onHtmlSaveActivity = useCallback((entryPath: string, delta: 1 | -1) => {
    const nextCount = Math.max(0, (pendingHtmlSaveCounts.current.get(entryPath) ?? 0) + delta);
    if (nextCount > 0) pendingHtmlSaveCounts.current.set(entryPath, nextCount);
    else pendingHtmlSaveCounts.current.delete(entryPath);
    const pendingNow = new Set(pendingSavePathsRef.current);
    if (nextCount > 0) pendingNow.add(entryPath);
    else pendingNow.delete(entryPath);
    pendingSavePathsRef.current = pendingNow;
    setPendingHtmlSavePaths((prev) => {
      if (nextCount > 0 === prev.has(entryPath)) return prev;
      const next = new Set(prev);
      if (nextCount > 0) next.add(entryPath);
      else next.delete(entryPath);
      return next;
    });
  }, []);
  const onDocxSave = useCallback(
    async (path: string, buffer: ArrayBuffer) => {
      pendingDocxSaveCounts.current.set(path, (pendingDocxSaveCounts.current.get(path) ?? 0) + 1);
      const pendingNow = new Set(pendingSavePathsRef.current);
      pendingNow.add(path);
      pendingSavePathsRef.current = pendingNow;
      setPendingDocxSavePaths((prev) => (prev.has(path) ? prev : new Set(prev).add(path)));
      try {
        await persistDocx(path, buffer);
      } finally {
        const remaining = (pendingDocxSaveCounts.current.get(path) ?? 1) - 1;
        if (remaining > 0) pendingDocxSaveCounts.current.set(path, remaining);
        else {
          pendingDocxSaveCounts.current.delete(path);
          const pendingNow = new Set(pendingSavePathsRef.current);
          pendingNow.delete(path);
          pendingSavePathsRef.current = pendingNow;
          setPendingDocxSavePaths((prev) => {
            if (!prev.has(path)) return prev;
            const next = new Set(prev);
            next.delete(path);
            return next;
          });
        }
      }
    },
    [persistDocx],
  );
  const pendingSavePaths = useMemo(
    () => new Set([...saverPendingSavePaths, ...pendingDocxSavePaths, ...pendingHtmlSavePaths]),
    [saverPendingSavePaths, pendingDocxSavePaths, pendingHtmlSavePaths],
  );
  return { onDocxSave, onHtmlSaveActivity, pendingSavePaths, pendingSavePathsRef };
}
