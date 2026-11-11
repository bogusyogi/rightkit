import { useCallback, type Dispatch, type MutableRefObject, type SetStateAction } from "react";
import { isMarkdownFormat } from "../documents/capabilities.js";
import {
  isUnsupportedBinary,
  type LoadedDocument,
  type ViewMeta,
  type ViewMode,
} from "@rightkit/document-core/documents/types";
import {
  isHostAppOrigin,
  normalizeLivePreviewUrl,
  webDisplayTitle,
  webTabPath,
} from "@rightkit/document-core/documents/web/webUrl";
import { openFileDialog, readDocument, grantKnownFileToken } from "@rightkit/document-core/state/tauriBridge";
import { recordRecent, type FileEntry } from "../state/library.js";
import type { DocumentTabs } from "./useDocumentTabs.js";
import { readFileDraft, clearFileDraft } from "./documentDrafts.js";
import { errorMessage } from "@rightkit/document-core/state/errors";
import { logError, logEvent, logPerf } from "@rightkit/document-core/state/diagnostics";
import { hostAppName } from "../appName.js";

export interface OpenDocumentOptions {
  /** Add the tab without activating it. */
  background?: boolean;
  /** Skip the per-open recents refresh; the caller refreshes once after a batch. */
  deferLibraryRefresh?: boolean;
  /** Leave the current error banner alone. Session-restore opens use this so they cannot clear an
   *  error they didn't set (e.g. a launched file that failed to open). Their own failures still
   *  report. */
  keepError?: boolean;
  /** Place the new tab within the open-tab list (applied in the same state update that adds it), so
   *  a batch opener can insert tabs at their final position instead of re-ordering them later. */
  arrange?: (docs: LoadedDocument[]) => LoadedDocument[];
}

interface DocumentLoaderDeps {
  openDocs: LoadedDocument[];
  setOpenDocs: DocumentTabs["setOpenDocs"];
  setViews: Dispatch<SetStateAction<Record<string, ViewMode>>>;
  viewsRef: MutableRefObject<Record<string, ViewMode>>;
  viewMetaRef: MutableRefObject<Record<string, ViewMeta>>;
  writeEdits: DocumentTabs["writeEdits"];
  setError: Dispatch<SetStateAction<string | null>>;
  setOpeningPath: Dispatch<SetStateAction<string | null>>;
  setActivePath: DocumentTabs["setActivePath"];
  fingerprints: MutableRefObject<Map<string, { mtimeMs: number; size: number }>>;
  refreshLibrary: () => Promise<void>;
  syncPinned: (path: string) => Promise<void>;
  isDirty: (doc: LoadedDocument) => boolean;
  clearImageDirty: (path: string) => void;
  withTabKey: <T extends LoadedDocument>(doc: T, existingKey?: string) => T;
}

/**
 * Open/read paths into tabs: file-picker open, live-URL "tabs", reopening a known-recent entry, the
 * open-file dialog, and paging to a sibling image in place. Extracted verbatim from useDocuments —
 * bodies, error handling, restore-draft logic, and view-default selection are unchanged. `activate`
 * stays with the orchestrator (it's just an activePath setter, and requestClose/doClose/reload all
 * need it too), so loader functions take it as a dependency instead of owning it.
 */
export function useDocumentLoader({
  openDocs,
  setOpenDocs,
  setViews,
  viewsRef,
  viewMetaRef,
  writeEdits,
  setError,
  setOpeningPath,
  setActivePath,
  fingerprints,
  refreshLibrary,
  syncPinned,
  isDirty,
  clearImageDirty,
  withTabKey,
}: DocumentLoaderDeps) {
  const activate = useCallback(
    async (path: string) => {
      // Pure state change. Every open file is watched (App's watch effect re-arms on the open set), so an
      // external change to ANY tab — active or background — arrives via the `file-changed` event and
      // reloads that tab when it happens. So activation does NOT re-stat or reload-on-switch; switching
      // tabs is instant, and a background tab is already fresh by the time you click it.
      setActivePath(path);
    },
    [setActivePath],
  );

  const open = useCallback(
    async (path: string, opts: OpenDocumentOptions = {}) => {
      const startedAt = performance.now();
      if (!opts.keepError) setError(null);
      logEvent("open:start");
      if (isUnsupportedBinary(path)) {
        setError(
          `${hostAppName()} can't open ${path.split(".").pop()?.toUpperCase()} files yet. Supported: Markdown, TXT, HTML, DOCX, PDF, and images (PNG, JPG, WEBP, GIF, BMP, SVG, ICO, AVIF).`,
        );
        logEvent("open:unsupported");
        return;
      }
      setOpeningPath(path);
      try {
        const loaded = await readDocument(path);
        logPerf("open:read", startedAt);
        if (loaded.fingerprint) fingerprints.current.set(path, loaded.fingerprint);
        setOpenDocs((prev) => {
          const existing = prev.find((d) => d.path === path);
          const next = withTabKey(loaded, existing?.tabKey);
          const docs = existing ? prev.map((d) => (d.path === path ? next : d)) : [...prev, next];
          return opts.arrange ? opts.arrange(docs) : docs;
        });
        // Notepad-style restore for file-backed txt: if the user left unsaved edits last session and
        // the file hasn't changed on disk since, silently restore the in-progress text. If the file
        // changed on disk (base mismatch), the draft is stale — discard it so the user sees fresh disk.
        if (loaded.format === "txt") {
          const draft = readFileDraft(path);
          if (draft) {
            if (draft.base === loaded.content && draft.text !== loaded.content) {
              writeEdits((e) => ({ ...e, [path]: draft.text }));
            } else {
              clearFileDraft(path); // stale — file changed on disk; discard silently
            }
          }
        }
        // HTML always opens in Interactive; Markdown always opens in Preview. Source is a within-session
        // drawer toggle, NOT a sticky per-file default — so neither format restores a persisted source/
        // split view (you always land on the rendered view; hit the switch for source).
        const defaultView = loaded.format === "html" ? "interactive" : "preview";
        const restoredView =
          loaded.format === "html"
            ? "interactive"
            : isMarkdownFormat(loaded.format)
              ? "preview"
              : loaded.lastView;
        setViews((v) => ({ ...v, [path]: v[path] ?? restoredView ?? defaultView }));
        viewsRef.current[path] = viewsRef.current[path] ?? restoredView ?? defaultView;
        viewMetaRef.current[path] = loaded.viewMeta ?? {};
        // Prefetch the renderer's lazy chunk so the FIRST tab activation (after this background
        // open) doesn't pay a 200–1000ms Suspense fallback. Each chunk is small (HTML pulls in
        // CodeMirror via TxtEditor for its source drawer; MD pulls in remark/unified), so warming
        // them in parallel with the open's other awaits costs nothing extra. The DocumentRouter
        // useEffect at lines 131–145 still re-prefetches as a safety net on tab activation.
        // Errors here are non-fatal — a chunk that can't load in the current environment (e.g.
        // jsdom missing DOMMatrix for PDF.js) just makes the first activation pay its lazy cost;
        // production code is unaffected because every browser/embedded WebView has DOMMatrix.
        switch (loaded.format) {
          case "html":
            void import("../documents/html/HtmlDocument.js").catch(() => {});
            void import("../documents/txt/TxtEditor.js").catch(() => {}); // HTML source drawer uses CodeMirror
            break;
          case "md":
            void import("../documents/markdown/MarkdownSplitPane.js").catch(() => {});
            void import("@rightkit/markdown-editor/documents/markdown/MarkdownCoreProvider").catch(() => {});
            void import("../documents/txt/TxtEditor.js").catch(() => {});
            break;
          case "mdx":
            void import("../documents/mdx/MdxSplitPane.js").catch(() => {});
            void import("@rightkit/markdown-editor/documents/mdx/mdxCompiler").catch(() => {});
            void import("../documents/markdown/MarkdownSplitPane.js").catch(() => {}); // shared TxtEditor + outline code
            void import("../documents/txt/TxtEditor.js").catch(() => {});
            break;
          case "docx":
            void import("@rightkit/docx-editor/documents/docx/DocxPane").catch(() => {});
            break;
          case "pdf":
            void import("../documents/pdf/PdfViewer.js").catch(() => {});
            break;
          case "image":
            void import("../documents/ImageDocument.js").catch(() => {});
            break;
          // "txt" reuses TxtEditor's chunk too; the prefetch is idempotent.
          case "txt":
          case "web":
            void import("../documents/txt/TxtEditor.js").catch(() => {});
            break;
          default:
            break;
        }
        // Background open (Ctrl/middle-click "open in new tab"): add the tab but DON'T switch to it —
        // the current document stays mounted and keeps its scroll position, like a browser. Skipping
        // activate() also skips the active-file watcher swap; clicking the tab later activates normally.
        if (!opts.background) await activate(path);
        // The document is on screen once activation lands. Release the spinner and mark the open
        // READY here rather than after the library writes below: those only feed the sidebar's
        // recent list, and awaiting them used to hold the indicator up for ~1s on a large recents
        // DB (see the 3.7s open:ready in the 2026-08-08 diagnostics). The `finally` clear becomes a
        // no-op second call. The bookkeeping is still awaited BELOW rather than detached, so it can
        // never settle after the tab (or the whole app) is torn down.
        setOpeningPath((current) => (current === path ? null : current));
        logPerf("open:ready", startedAt);
        // recordRecent and syncPinned touch independent DB tables (record_recent vs. is_pinned_path)
        // and neither depends on the other's result — run them concurrently. Callers doing a batch
        // of background opens (session restore) skip the per-open library refresh and refresh once
        // after the whole batch instead — refreshLibrary re-queries the entire recents list, so
        // doing it per-tab is redundant work during a multi-tab restore.
        const libraryStartedAt = performance.now();
        await Promise.all([recordRecent(loaded), syncPinned(loaded.path)]);
        if (!opts.deferLibraryRefresh) await refreshLibrary();
        logPerf("open:library", libraryStartedAt);
      } catch (e) {
        const message = errorMessage(e);
        setError(message);
        logError("open:failed");
        logPerf("open:error", startedAt);
      } finally {
        // Several paths may be opened concurrently (for example Ctrl+click). Only the request
        // that still owns the indicator may clear it.
        setOpeningPath((current) => (current === path ? null : current));
      }
    },
    [
      activate,
      refreshLibrary,
      syncPinned,
      setOpenDocs,
      setViews,
      viewsRef,
      viewMetaRef,
      withTabKey,
      fingerprints,
      writeEdits,
      setError,
      setOpeningPath,
    ],
  );

  const openLiveUrl = useCallback(
    async (input: string, opts: { background?: boolean } = {}) => {
      setError(null);
      const url = normalizeLivePreviewUrl(input);
      if (!url) {
        setError("Open URL accepts http(s) URLs, localhost previews, and bare domains.");
        return;
      }
      if (isHostAppOrigin(url)) {
        setError(
          `${hostAppName()} cannot frame its own app origin. Use the localhost port for the app you want to preview.`,
        );
        return;
      }
      const path = webTabPath(url);
      if (openDocs.some((d) => d.path === path)) {
        if (!opts.background) await activate(path);
        return;
      }
      const web = { kind: "url" as const, url };
      const doc: LoadedDocument = withTabKey({
        path,
        displayName: webDisplayTitle(web),
        format: "web",
        content: "",
        web,
      });
      setOpenDocs((prev) => [...prev, doc]);
      setViews((v) => ({ ...v, [path]: "preview" }));
      viewsRef.current[path] = "preview";
      viewMetaRef.current[path] = {};
      // Background (new-tab gesture): add it without stealing focus from the current document.
      if (!opts.background) await activate(path);
    },
    [activate, openDocs, setOpenDocs, setViews, viewsRef, viewMetaRef, withTabKey, setError],
  );

  const openRecent = useCallback(
    async (entry: FileEntry) => {
      const path = await grantKnownFileToken(entry.token);
      if (!path) return;
      if (openDocs.some((d) => d.path === path)) return activate(path);
      return open(path);
    },
    [openDocs, activate, open],
  );

  const onOpenDialog = useCallback(async () => {
    const path = await openFileDialog();
    if (path) await open(path);
  }, [open]);

  // Page to a neighbouring image in the same folder (Windows-Photos style): REPLACE the current
  // image tab's document in place — same tab slot, no pile-up — so scrolling a 200-image folder
  // never spawns 200 tabs. Guards: a current image with unsaved edits opens the neighbour as its own
  // tab instead (never silently discards edits); a neighbour already open elsewhere just activates.
  // The target path was pre-granted by `list_sibling_images`, so readDocument is authorized.
  const openImageSibling = useCallback(
    async (currentPath: string, targetPath: string) => {
      if (targetPath === currentPath) return;
      const cur = openDocs.find((d) => d.path === currentPath);
      if (cur && isDirty(cur)) {
        await open(targetPath);
        return;
      }
      if (openDocs.some((d) => d.path === targetPath)) {
        await activate(targetPath);
        return;
      }
      try {
        const loaded = await readDocument(targetPath);
        if (loaded.fingerprint) fingerprints.current.set(targetPath, loaded.fingerprint);
        setOpenDocs((prev) =>
          prev.map((d) => (d.path === currentPath ? withTabKey(loaded, d.tabKey) : d)),
        );
        setViews((v) => ({ ...v, [targetPath]: "preview" }));
        viewsRef.current[targetPath] = "preview";
        viewMetaRef.current[targetPath] = {};
        clearImageDirty(currentPath);
        await activate(targetPath);
        // Deliberately NOT recordRecent here: paging through a folder shouldn't flood the recent
        // list with every image scrolled past (only the file you explicitly opened is "recent").
        await syncPinned(loaded.path);
      } catch (e) {
        setError(errorMessage(e));
      }
    },
    [
      openDocs,
      isDirty,
      open,
      activate,
      clearImageDirty,
      syncPinned,
      setOpenDocs,
      setViews,
      viewsRef,
      viewMetaRef,
      withTabKey,
      fingerprints,
      setError,
    ],
  );

  return {
    activate,
    open,
    openLiveUrl,
    openRecent,
    onOpenDialog,
    openImageSibling,
  };
}
