import { useCallback, useEffect, useReducer, useRef, useState, type SetStateAction } from "react";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import type { HtmlSessionPages } from "@rightkit/html-editor/documents/html/sessionTypes";

/**
 * The shared open-document state: tabs (openDocs + activePath), per-path unsaved TXT buffers
 * (edits, with a synchronous editsRef mirror so a save/close in the same tick as a keystroke reads
 * the latest text), the DOCX dirty set, and the per-HTML-session sub-page buffers. Owned separately
 * so useViewState/useLibrary can take activePath/openDocs while useDocuments takes the setters
 * (breaks the otherwise-circular wiring).
 */
export function useDocumentTabs() {
  const [openDocs, setOpenDocs] = useState<LoadedDocument[]>([]);
  const [activePath, setActivePath] = useState<string | null>(null);
  // editsRef + a per-path pub/sub ARE the unsaved-text store (the single source of truth — NOT React
  // state). A keystroke mutates the ref and notifies ONLY that path's subscribers (the active editor's
  // bottom-bar word-count/dirty and that tab's title), so App and the rest of the chrome never re-render
  // on a keystroke. `bumpEdits` exists only to re-render App on the rare EXTERNAL write
  // (reopen/conflict/restore/save) so the controlled editor's `text` prop re-reads the ref.
  const editsRef = useRef<Record<string, string>>({});
  const [, bumpEdits] = useReducer((n: number) => n + 1, 0);
  const editsSubsRef = useRef<Map<string, Set<() => void>>>(new Map());
  const liveNotifyTimersRef = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map());
  const notifyEdits = useCallback((path: string) => {
    // Snapshot the Set before iterating: a subscriber that unsubscribes inside its own callback would
    // otherwise make Set.forEach skip not-yet-visited callbacks in the same pass.
    const cbs = editsSubsRef.current.get(path);
    if (cbs) for (const cb of [...cbs]) cb();
  }, []);
  const scheduleLiveNotify = useCallback(
    (path: string) => {
      const existing = liveNotifyTimersRef.current.get(path);
      if (existing) clearTimeout(existing);
      liveNotifyTimersRef.current.set(
        path,
        setTimeout(() => {
          liveNotifyTimersRef.current.delete(path);
          notifyEdits(path);
        }, 90),
      );
    },
    [notifyEdits],
  );
  const clearLiveNotify = useCallback((path: string) => {
    const existing = liveNotifyTimersRef.current.get(path);
    if (!existing) return;
    clearTimeout(existing);
    liveNotifyTimersRef.current.delete(path);
  }, []);
  useEffect(
    () => () => {
      for (const timer of liveNotifyTimersRef.current.values()) clearTimeout(timer);
      liveNotifyTimersRef.current.clear();
    },
    [],
  );
  const subscribeEdits = useCallback((path: string, cb: () => void) => {
    let set = editsSubsRef.current.get(path);
    if (!set) {
      set = new Set();
      editsSubsRef.current.set(path, set);
    }
    set.add(cb);
    return () => {
      set.delete(cb);
      if (set.size === 0) editsSubsRef.current.delete(path);
    };
  }, []);
  const getEditSnapshot = useCallback((path: string) => editsRef.current[path], []);
  // Origin tag for external ingress: bumped ONLY by writeEdits (reopen/reload/restore/conflict/
  // duplicate) when a path's value actually changes — never by writeEditsLive (core publishes).
  // The Markdown core provider applies external text only when this advances past what it applied.
  const externalSeqRef = useRef<Record<string, number>>({});
  const getExternalSeq = useCallback((path: string) => externalSeqRef.current[path] ?? 0, []);
  // Keystroke path: mutate the ref synchronously + debounce this path's live chrome subscribers. No App
  // re-render. Saves/closes read editsRef immediately, while title/word-count repaint once after a burst
  // of typing instead of making a React commit for every key.
  const writeEditsLive = useCallback(
    (path: string, text: string) => {
      editsRef.current[path] = text;
      scheduleLiveNotify(path);
    },
    [scheduleLiveNotify],
  );
  // External path (reopen/conflict/restore/save/duplicate): mutate the ref, notify any changed path's
  // subscribers, and bump so App re-renders once and the controlled editor re-syncs from the ref.
  const writeEdits = useCallback(
    (updater: (prev: Record<string, string>) => Record<string, string>) => {
      const prev = editsRef.current;
      editsRef.current = updater(prev);
      for (const path of new Set([...Object.keys(prev), ...Object.keys(editsRef.current)])) {
        if (editsRef.current[path] !== prev[path])
          externalSeqRef.current[path] = (externalSeqRef.current[path] ?? 0) + 1;
      }
      for (const path of editsSubsRef.current.keys()) {
        if (editsRef.current[path] !== prev[path]) {
          clearLiveNotify(path);
          notifyEdits(path);
        }
      }
      bumpEdits();
    },
    [clearLiveNotify, notifyEdits],
  );
  const [docxDirty, setDocxDirtyState] = useState<Set<string>>(new Set());
  const docxDirtyRef = useRef<Set<string>>(new Set());
  const setDocxDirty = useCallback((next: SetStateAction<Set<string>>) => {
    const value =
      typeof next === "function"
        ? (next as (prev: Set<string>) => Set<string>)(docxDirtyRef.current)
        : next;
    docxDirtyRef.current = value;
    setDocxDirtyState(value);
  }, []);
  const [pdfDirty, setPdfDirtyState] = useState<Set<string>>(new Set());
  const pdfDirtyRef = useRef<Set<string>>(new Set());
  const setPdfDirty = useCallback((next: SetStateAction<Set<string>>) => {
    const value =
      typeof next === "function"
        ? (next as (prev: Set<string>) => Set<string>)(pdfDirtyRef.current)
        : next;
    pdfDirtyRef.current = value;
    setPdfDirtyState(value);
  }, []);
  // Text/HTML keep their buffers here; binary editors contribute path markers to the same tab store.
  const [imageDirty, setImageDirtyState] = useState<Set<string>>(new Set());
  const imageDirtyRef = useRef<Set<string>>(new Set());
  const setImageDirty = useCallback((next: SetStateAction<Set<string>>) => {
    const value =
      typeof next === "function"
        ? (next as (prev: Set<string>) => Set<string>)(imageDirtyRef.current)
        : next;
    imageDirtyRef.current = value;
    setImageDirtyState(value);
  }, []);

  // HTML site sessions. Sub-page buffers live here (App level), not inside HtmlDocument, because only
  // the active document is mounted — component state would be lost on a tab switch and invisible to
  // the window-quit guard. The synchronous htmlPagesRef mirror lets a save/close read the latest
  // edits in the same tick as a keystroke (same pattern as editsRef).
  const [htmlPages, setHtmlPages] = useState<HtmlSessionPages>({});
  const htmlPagesRef = useRef<HtmlSessionPages>({});
  // T2-3: the React-state publish below feeds App's per-path html props (htmlPropsFor), which cascade
  // into a DocumentRouter re-render — debounce it for the per-keystroke edit path (typing in an HTML
  // sub-page) so that doesn't storm the whole app. `htmlPagesRef` is updated SYNCHRONOUSLY on every
  // call regardless of debounce, so save/dirty logic (which reads the ref, not this state) is never
  // stale; `notifyEdits` also stays synchronous so the bottom-bar/tab dirty marker stays live. Every
  // caller EXCEPT the live-typing path (useHtmlSession's setHtmlPageEdited) passes immediate=true
  // (the default), which also FLUSHES any pending debounced publish — save/discard/reload always see
  // the latest buffer, so the state is never stale when it actually matters.
  const htmlPagesPublishTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const writeHtmlPages = useCallback(
    (
      updater: (prev: HtmlSessionPages) => HtmlSessionPages,
      options: { immediate?: boolean } = {},
    ) => {
      htmlPagesRef.current = updater(htmlPagesRef.current);
      // Notify entry-path subscribers so the bottom bar's isDirty(htmlDoc) — which folds in sub-page
      // dirtiness — re-evaluates live while editing a navigated sub-page (no tab switch needed).
      for (const entryPath of Object.keys(htmlPagesRef.current)) notifyEdits(entryPath);
      if (options.immediate === false) {
        if (htmlPagesPublishTimerRef.current != null)
          clearTimeout(htmlPagesPublishTimerRef.current);
        htmlPagesPublishTimerRef.current = setTimeout(() => {
          htmlPagesPublishTimerRef.current = null;
          setHtmlPages(htmlPagesRef.current);
        }, 300);
        return;
      }
      if (htmlPagesPublishTimerRef.current != null) {
        clearTimeout(htmlPagesPublishTimerRef.current);
        htmlPagesPublishTimerRef.current = null;
      }
      setHtmlPages(htmlPagesRef.current);
    },
    [notifyEdits],
  );
  useEffect(
    () => () => {
      if (htmlPagesPublishTimerRef.current != null) clearTimeout(htmlPagesPublishTimerRef.current);
    },
    [],
  );
  // entryPath → the in-site page currently shown in that HTML tab (roams as the user navigates).
  // App needs it for Ctrl+S / Save targeting and the per-page dirty indicator.
  const [htmlActivePage, setHtmlActivePage] = useState<Record<string, string>>({});

  // MDX component-import grants. Unlike HTML sub-pages, these have no editable buffer — the sandboxed
  // iframe only reads them at compile time — so a plain ref (entryPath -> Set<grantedSubPath>) is
  // enough bookkeeping. Tracked so the owning tab's close can revoke them (mirrors htmlPagesRef's
  // shape one level down: Map<entryPath, Set<subPath>> instead of Map<entryPath, Map<subPath, buffer>>).
  const mdxGrantsRef = useRef<Record<string, Set<string>>>({});
  const trackMdxGrant = useCallback((entryPath: string, grantedPath: string) => {
    if (grantedPath === entryPath) return;
    let set = mdxGrantsRef.current[entryPath];
    if (!set) {
      set = new Set();
      mdxGrantsRef.current[entryPath] = set;
    }
    set.add(grantedPath); // Set semantics: re-resolving the same import on HMR-style re-render is a no-op.
  }, []);

  return {
    openDocs,
    setOpenDocs,
    activePath,
    setActivePath,
    editsRef,
    writeEdits,
    writeEditsLive,
    subscribeEdits,
    getEditSnapshot,
    getExternalSeq,
    docxDirty,
    docxDirtyRef,
    setDocxDirty,
    pdfDirty,
    pdfDirtyRef,
    setPdfDirty,
    imageDirty,
    imageDirtyRef,
    setImageDirty,
    htmlPages,
    htmlPagesRef,
    writeHtmlPages,
    htmlActivePage,
    setHtmlActivePage,
    mdxGrantsRef,
    trackMdxGrant,
  };
}

export type DocumentTabs = ReturnType<typeof useDocumentTabs>;
