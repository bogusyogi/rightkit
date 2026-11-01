import { useCallback, useEffect, useRef, type MutableRefObject } from "react";
import { isUntitled, type LoadedDocument } from "@rightkit/document-core/documents/types";
import { editableImageOutputForPath } from "@rightkit/media-ui";
import { flushDrafts } from "./documentDrafts";

const AUTO_SAVE_DELAY_MS = 1000;

interface AutoSaveDeps {
  /** settings.autoSave — file-backed edits persist after a short idle delay. */
  autoSave: boolean;
  openDocs: LoadedDocument[];
  openDocsRef: MutableRefObject<LoadedDocument[]>;
  activePath: string | null;
  activePathRef: MutableRefObject<string | null>;
  isDirty: (doc: LoadedDocument) => boolean;
  docxSaver: MutableRefObject<(() => Promise<ArrayBuffer | null>) | null>;
  onDocxSave: (path: string, buffer: ArrayBuffer) => Promise<void>;
  onSave: (
    doc: LoadedDocument,
    options?: { silent?: boolean; resolveDrafts?: boolean },
  ) => Promise<string | null>;
  saveAllHtmlPages: (entryPath: string, options?: { silent?: boolean }) => Promise<boolean>;
  /** Scheduler slot the orchestrator's stable `queueAutoSave` forwarder calls through — assigned
   *  here each render (same ref-forward contract as before: the forwarder exists before this hook
   *  can be constructed, so the ref bridges the gap). */
  autoSaveSchedulerRef: MutableRefObject<(path: string) => void>;
}

/**
 * Idle-delay autosave for file-backed tabs (txt/md via onSave, docx via the active bridge's
 * exporter, image via the payload saver, html via the session's saveAllHtmlPages), plus the T2-1
 * draft-flush guards on unload/hide. Extracted verbatim from useDocuments — bodies unchanged; the
 * only edits are `settings.autoSave`/`htmlSession.saveAllHtmlPages` read through their narrower
 * params. `autoSaveTimers` is owned here; `autoSaveSchedulerRef` stays owned by the orchestrator so
 * `queueAutoSave` (needed earlier, before this hook's deps exist) keeps its stable identity.
 */
export function useAutoSave({
  autoSave,
  openDocs,
  openDocsRef,
  activePath,
  activePathRef,
  isDirty,
  docxSaver,
  onDocxSave,
  onSave,
  saveAllHtmlPages,
  autoSaveSchedulerRef,
}: AutoSaveDeps) {
  const autoSaveTimers = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map());

  const scheduleAutoSave = useCallback(
    (path: string) => {
      const prior = autoSaveTimers.current.get(path);
      if (prior) clearTimeout(prior);
      autoSaveTimers.current.delete(path);
      if (!autoSave || isUntitled(path)) return;
      const timer = setTimeout(() => {
        autoSaveTimers.current.delete(path);
        const doc = openDocsRef.current.find((candidate) => candidate.path === path);
        if (!doc || !isDirty(doc)) return;
        if (doc.format === "docx") {
          // The imperative DOCX saver belongs to the active mounted bridge. Never risk using another
          // tab's bridge if the user switched tabs during the idle delay.
          if (activePathRef.current !== path) return;
          void docxSaver.current?.().then((buffer) => {
            if (buffer) void onDocxSave(path, buffer);
          });
          return;
        }
        if (doc.format === "image") {
          if (activePathRef.current !== path || !editableImageOutputForPath(path)) return;
          void onSave(doc, { silent: true, resolveDrafts: false });
          return;
        }
        if (doc.format === "html") {
          void saveAllHtmlPages(path, { silent: true });
          return;
        }
        void onSave(doc, { silent: true, resolveDrafts: false });
      }, AUTO_SAVE_DELAY_MS);
      autoSaveTimers.current.set(path, timer);
    },
    [
      docxSaver,
      saveAllHtmlPages,
      isDirty,
      onDocxSave,
      onSave,
      autoSave,
      activePathRef,
      openDocsRef,
    ],
  );
  autoSaveSchedulerRef.current = scheduleAutoSave;

  useEffect(() => {
    if (autoSave) {
      for (const doc of openDocs) if (isDirty(doc)) scheduleAutoSave(doc.path);
      return;
    }
    for (const timer of autoSaveTimers.current.values()) clearTimeout(timer);
    autoSaveTimers.current.clear();
  }, [isDirty, openDocs, scheduleAutoSave, autoSave]);

  // DOCX and image exporters are registered by their active viewer. If a user switches tabs during
  // the idle delay, their timer intentionally skips that unsafe save; retry when the tab becomes
  // active again so autosave does not remain stranded behind the active-viewer guard.
  useEffect(() => {
    if (!autoSave || !activePath) return;
    const active = openDocs.find((doc) => doc.path === activePath);
    if (active && isDirty(active)) scheduleAutoSave(active.path);
  }, [activePath, isDirty, openDocs, scheduleAutoSave, autoSave]);

  useEffect(
    () => () => {
      for (const timer of autoSaveTimers.current.values()) clearTimeout(timer);
      autoSaveTimers.current.clear();
    },
    [],
  );

  // T2-1 data-loss guard: onTxtChange's draft writes are debounced (~400ms). If the window/process goes
  // away before a pending write's timer fires, that in-flight edit would be lost from the recovery
  // draft (the live in-memory editsRef is what save/close actually use, so this only protects the
  // startup-restore draft, not save correctness). Flush synchronously on both signals — beforeunload
  // covers window close/reload, visibilitychange-hidden also covers OS-level app switch/minimize/quit
  // paths that don't reliably fire beforeunload in a Tauri webview.
  useEffect(() => {
    const onBeforeUnload = (event: BeforeUnloadEvent) => {
      flushDrafts();
      // Browser close guard: block navigation if any file-backed dirty doc or modified block draft exists.
      const hasUnsavedWork = openDocsRef.current.some((doc) => isDirty(doc));
      if (hasUnsavedWork) {
        event.preventDefault();
        event.returnValue = "";
      }
    };
    const onVisibilityChange = () => {
      if (document.visibilityState === "hidden") flushDrafts();
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      window.removeEventListener("beforeunload", onBeforeUnload);
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, [isDirty, openDocsRef]);
}
