import {
  useCallback,
  useEffect,
  useRef,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import { isUntitled, type LoadedDocument } from "@rightkit/document-core/documents/types";
import { isWebTabKey } from "@rightkit/document-core/documents/web/webUrl";
import { fileFingerprint, readDocument, revokePath } from "@rightkit/document-core/state/tauriBridge";
import type { DocumentTabs } from "./useDocumentTabs";
import { clearDraft, clearFileDraft } from "./documentDrafts";
import { toast } from "@rightkit/platform-ui/headless";
import { resolveDocumentDrafts } from "./draftResolution";
import type { BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";

const CLOSED_STACK_MAX = 10;
const EMPTY_PENDING_SAVE_PATHS: ReadonlySet<string> = new Set();

function hasSynchronousPendingPath(
  path: string,
  primary: MutableRefObject<ReadonlySet<string>> | undefined,
  mirrors: ReadonlyArray<MutableRefObject<ReadonlySet<string>>> | undefined,
): boolean {
  if (primary?.current.has(path)) return true;
  for (const ref of mirrors ?? []) if (ref.current.has(path)) return true;
  return false;
}

// After closing the active tab, which tab gets focus. Prefer the most-recently-used still-open tab
// (MRU — matches VS Code's `focusRecentEditorAfterClose`); if there's no usable history, fall back to
// the spatial neighbour (the tab to the right, then to the left); return null only when nothing remains.
export function pickNextActive(
  closedPath: string,
  prevDocs: LoadedDocument[],
  remaining: LoadedDocument[],
  activationOrder: string[],
): string | null {
  if (remaining.length === 0) return null;
  const remainingSet = new Set(remaining.map((d) => d.path));
  for (let i = activationOrder.length - 1; i >= 0; i--) {
    const p = activationOrder[i];
    if (p !== closedPath && remainingSet.has(p)) return p;
  }
  const idx = prevDocs.findIndex((d) => d.path === closedPath);
  const neighbour = prevDocs[idx + 1] ?? prevDocs[idx - 1];
  return neighbour ? neighbour.path : remaining[0].path;
}

interface DocumentWatcherDeps {
  openDocs: LoadedDocument[];
  openDocsRef: MutableRefObject<LoadedDocument[]>;
  activationOrderRef: MutableRefObject<string[]>;
  setOpenDocs: DocumentTabs["setOpenDocs"];
  setActivePath: DocumentTabs["setActivePath"];
  activePath: string | null;
  writeEdits: DocumentTabs["writeEdits"];
  setDocxDirty: DocumentTabs["setDocxDirty"];
  setPdfDirty: DocumentTabs["setPdfDirty"];
  htmlPagesRef: DocumentTabs["htmlPagesRef"];
  writeHtmlPages: DocumentTabs["writeHtmlPages"];
  setHtmlActivePage: DocumentTabs["setHtmlActivePage"];
  mdxGrantsRef: DocumentTabs["mdxGrantsRef"];
  clearImageDirty: (path: string) => void;
  isDirty: (doc: LoadedDocument) => boolean;
  openLiveUrl: (input: string, opts?: { background?: boolean }) => Promise<void>;
  activate: (path: string) => Promise<void>;
  open: (path: string) => Promise<void>;
  fingerprints: MutableRefObject<Map<string, { mtimeMs: number; size: number }>>;
  selfSaved: MutableRefObject<Set<string>>;
  /** Save paths whose writes are still in flight; watcher events are replayed after completion. */
  pendingSavePaths?: ReadonlySet<string>;
  /** Synchronous saver mirror; covers the gap before pendingSavePaths state renders. */
  synchronousPendingSavePathsRef?: MutableRefObject<ReadonlySet<string>>;
  /** Synchronous mirrors for DOCX/HTML save wrappers. */
  synchronousPendingSavePathsRefs?: ReadonlyArray<MutableRefObject<ReadonlySet<string>>>;
  reloadHtmlSubpage: (path: string) => void;
  setHtmlConfirm: Dispatch<SetStateAction<{ entryPath: string; mode: "save" | "close" } | null>>;
  setConfirmClose: Dispatch<SetStateAction<string | null>>;
  confirmClose: string | null;
  setConflict: Dispatch<SetStateAction<string | null>>;
  onSave: (doc: LoadedDocument) => Promise<string | null>;
  onDocxSave: (path: string, buffer: ArrayBuffer) => Promise<void>;
  docxSaver: MutableRefObject<(() => Promise<ArrayBuffer | null>) | null>;
  docxSaversRef?: MutableRefObject<Map<string, () => Promise<ArrayBuffer | null>>>;
  onDocxSaveAs: (doc: LoadedDocument, buffer?: ArrayBuffer) => Promise<string | null>;
  withTabKey: <T extends LoadedDocument>(doc: T, existingKey?: string) => T;
  /** Block-draft registry for draft-aware close/save/reload. Optional so this hook's signature stays
   *  backward-compatible with callers that haven't threaded it through yet. */
  blockDrafts?: BlockDraftRegistry;
  /** useViewState's teardown (see useViewState.ts) — deletes a closed path's views/viewsRef/
   *  viewMetaRef entries. Optional so this hook's signature stays backward-compatible with callers
   *  that haven't threaded it through yet; when absent, view-state pruning on close is simply
   *  skipped (same as before this dep existed). */
  dropViewState?: (path: string) => void;
}

/**
 * Tab lifecycle (close/teardown/reopen) and external-change detection (fingerprint-checked
 * reload). Extracted verbatim from useDocuments — bodies, fingerprint suppression, and the reopen
 * stack's bounded-eviction grant revocation are unchanged. Owns `closedTabs` (the Ctrl+Shift+T stack);
 * `fingerprints` and `selfSaved` are owned by sibling hooks and threaded in so all three read/write the
 * same single source of truth. `confirmClose`/`conflict` STATE stays with the orchestrator (it's part
 * of the public return shape and `requestClose` needs to set it before this hook can read it back), so
 * this hook only receives the current value + setter.
 */
export function useDocumentWatcher({
  openDocs,
  openDocsRef,
  activationOrderRef,
  setOpenDocs,
  setActivePath,
  activePath,
  writeEdits,
  setDocxDirty,
  setPdfDirty,
  htmlPagesRef,
  writeHtmlPages,
  setHtmlActivePage,
  mdxGrantsRef,
  clearImageDirty,
  isDirty,
  openLiveUrl,
  activate,
  open,
  fingerprints,
  selfSaved,
  pendingSavePaths = EMPTY_PENDING_SAVE_PATHS,
  synchronousPendingSavePathsRef,
  synchronousPendingSavePathsRefs,
  reloadHtmlSubpage,
  setHtmlConfirm,
  setConfirmClose,
  confirmClose,
  setConflict,
  onSave,
  onDocxSave,
  docxSaver,
  docxSaversRef,
  onDocxSaveAs,
  withTabKey,
  blockDrafts,
  dropViewState,
}: DocumentWatcherDeps) {
  // Recently-closed file/web tabs for Ctrl+Shift+T reopen (most-recent last). Bounded; evicting an
  // entry revokes its retained broker grant (see doClose).
  const closedTabs = useRef<Array<{ path: string; url?: string }>>([]);
  const pendingSavePathsStateRef = useRef(pendingSavePaths);
  pendingSavePathsStateRef.current = pendingSavePaths;
  const deferredReloadsRef = useRef<Set<string>>(new Set());

  // Per-doc teardown shared by single close (doClose) and bulk close (closeAll): drop drafts, retain
  // the reopen-stack grant, revoke HTML sub-page grants, and clear docx/image/html session state. It
  // deliberately does NOT touch openDocs/activePath — the caller owns the tab-list mutation, so a bulk
  // close can drop many tabs in one functional setState (a doClose loop would stale-close only one).
  const teardownDoc = useCallback(
    (path: string, closingDoc: LoadedDocument | undefined) => {
      deferredReloadsRef.current.delete(path);
      // Closing an untitled buffer discards it (no backing file) — drop its cached draft.
      if (isUntitled(path)) {
        clearDraft(path);
      } else {
        // File-backed txt draft: clear on explicit close (Don't Save / after Save).
        clearFileDraft(path);
        // Reopen-closed-tab (Ctrl+Shift+T): remember the closed file/web tab and KEEP its broker grant
        // alive so it can reopen without a fresh OS-level grant. The authorized set still shrinks —
        // just lazily: a grant is revoked when its entry is evicted past the bounded stack depth.
        if (!isWebTabKey(path)) {
          closedTabs.current = closedTabs.current.filter((e) => e.path !== path);
          closedTabs.current.push({ path });
          while (closedTabs.current.length > CLOSED_STACK_MAX) {
            const evicted = closedTabs.current.shift();
            if (evicted) void revokePath(evicted.path);
          }
        } else if (closingDoc?.web?.url) {
          closedTabs.current = closedTabs.current.filter((e) => e.path !== path);
          closedTabs.current.push({ path, url: closingDoc.web.url });
          if (closedTabs.current.length > CLOSED_STACK_MAX) closedTabs.current.shift();
        }
      }
      fingerprints.current.delete(path);
      // Prune this path's view state (mode + scroll/page/zoom meta) alongside the rest of the
      // per-close teardown — otherwise views/viewsRef/viewMetaRef only ever grow for the app's
      // lifetime as tabs are opened and closed.
      dropViewState?.(path);
      // Drop the in-memory unsaved text so reopening a closed-without-saving file shows disk content
      // (not the discarded edits), and closed tabs don't accumulate in editsRef across the session.
      writeEdits((e) => {
        if (!(path in e)) return e;
        const n = { ...e };
        delete n[path];
        return n;
      });
      setDocxDirty((s) => {
        if (!s.has(path)) return s;
        const n = new Set(s);
        n.delete(path);
        return n;
      });
      setPdfDirty((paths) => {
        if (!paths.has(path)) return paths;
        const next = new Set(paths);
        next.delete(path);
        return next;
      });
      clearImageDirty(path);
      // Tear down any HTML site session this tab owned: revoke every sub-page grant minted during
      // in-tab navigation (shrinks the authorized set), drop the buffers, and clear its view state.
      const session = htmlPagesRef.current[path];
      if (session) {
        for (const sub of Object.keys(session)) if (sub !== path) void revokePath(sub);
        writeHtmlPages((p) => {
          if (!(path in p)) return p;
          const n = { ...p };
          delete n[path];
          return n;
        });
      }
      // Tear down any MDX component-import grants this tab minted (compile-time relative imports +
      // in-preview link clicks) — mirrors the HTML sub-page revocation above so an MDX grant doesn't
      // outlive its document until app exit.
      const mdxGrants = mdxGrantsRef.current[path];
      if (mdxGrants) {
        for (const sub of mdxGrants) void revokePath(sub);
        delete mdxGrantsRef.current[path];
      }
      setHtmlActivePage((s) => {
        if (!(path in s)) return s;
        const n = { ...s };
        delete n[path];
        return n;
      });
      // Teardown discards any live block drafts for this document so nothing can commit after close.
      if (blockDrafts) {
        for (const d of blockDrafts.snapshotAll(path)) blockDrafts.discard(d.identity);
      }
    },
    [
      writeEdits,
      setDocxDirty,
      setPdfDirty,
      clearImageDirty,
      htmlPagesRef,
      writeHtmlPages,
      setHtmlActivePage,
      mdxGrantsRef,
      fingerprints,
      dropViewState,
      blockDrafts,
    ],
  );

  const doClose = useCallback(
    (path: string) => {
      const prevDocs = openDocsRef.current;
      const closingDoc = prevDocs.find((d) => d.path === path);
      const remaining = prevDocs.filter((d) => d.path !== path);
      const nextActive = pickNextActive(path, prevDocs, remaining, activationOrderRef.current);
      const at = activationOrderRef.current.indexOf(path);
      if (at !== -1) activationOrderRef.current.splice(at, 1);
      setOpenDocs((prev) => prev.filter((d) => d.path !== path));
      setActivePath((current) => (current === path ? nextActive : current));
      teardownDoc(path, closingDoc);
    },
    [setOpenDocs, setActivePath, teardownDoc, openDocsRef, activationOrderRef],
  );

  // Ctrl+Shift+T: reopen the most-recently-closed file/web tab. The retained grant (see doClose) lets
  // a file reopen without a fresh OS grant; a web tab re-validates its URL through openLiveUrl.
  const reopenLastClosed = useCallback(() => {
    const entry = closedTabs.current.pop();
    if (!entry) return;
    if (entry.url) {
      void openLiveUrl(entry.url);
    } else if (openDocs.some((d) => d.path === entry.path)) {
      void activate(entry.path);
    } else {
      void open(entry.path).catch(() => {
        toast("Couldn't reopen — the file may have been removed", "info");
      });
    }
  }, [activate, open, openLiveUrl, openDocs]);

  // Closing a tab with unsaved changes prompts Save / Don't Save / Cancel (Notepad/Word-style).
  const requestClose = useCallback(
    (path: string) => {
      const doc = openDocs.find((d) => d.path === path);
      if (doc && isDirty(doc)) {
        // A DOCX's live bytes exist only in its mounted editor — activate it so the dialog's Save works.
        if (doc.format === "docx" && path !== activePath) setActivePath(path);
        // HTML sessions can have several dirty pages — use the page-aware Save Current / Save All /
        // Discard All / Cancel dialog instead of the single-doc Save / Don't Save prompt.
        if (doc.format === "html") setHtmlConfirm({ entryPath: path, mode: "close" });
        else setConfirmClose(path);
      } else doClose(path);
    },
    [openDocs, isDirty, doClose, activePath, setActivePath, setHtmlConfirm, setConfirmClose],
  );

  // Close dialog: Save → persist then close; Don't Save → discard + close; Cancel → keep open.
  const confirmSave = useCallback(async () => {
    const path = confirmClose;
    if (!path) return;
    const doc = openDocs.find((d) => d.path === path);
    if (!doc) {
      setConfirmClose(null);
      return;
    }
    // Save means "Done then persist": resolve this document's block drafts first; a stale/failed
    // draft leaves the dialog + editor open with the draft error visible.
    if (blockDrafts) {
      const outcome = await resolveDocumentDrafts(blockDrafts, path, "close", "done");
      if (outcome !== "resolved") return;
    }
    if (doc.format === "docx") {
      if (isUntitled(doc.path)) {
        const savedPath = await onDocxSaveAs(doc);
        if (savedPath) doClose(path);
        setConfirmClose(null);
        return;
      }
      const ab = await (docxSaversRef?.current.get(path) ?? docxSaver.current)?.();
      if (ab) {
        await onDocxSave(path, ab);
        doClose(path);
      }
      setConfirmClose(null);
      return;
    }
    if (doc.format === "image") {
      const saved = await onSave(doc);
      if (saved) doClose(saved);
      setConfirmClose(null);
      return;
    }
    const saved = await onSave(doc); // resulting path, or null if the save dialog was cancelled
    if (saved) doClose(saved);
    setConfirmClose(null);
  }, [
    confirmClose,
    openDocs,
    onSave,
    onDocxSave,
    doClose,
    setConfirmClose,
    docxSaver,
    docxSaversRef,
    onDocxSaveAs,
    blockDrafts,
  ]);

  const confirmDontSave = useCallback(async () => {
    // Don't Save discards the document's block drafts before the tab closes.
    if (blockDrafts && confirmClose)
      await resolveDocumentDrafts(blockDrafts, confirmClose, "close", "discard");
    if (confirmClose) doClose(confirmClose);
    setConfirmClose(null);
  }, [confirmClose, doClose, setConfirmClose, blockDrafts]);

  const applyReload = useCallback(
    (path: string) => {
      if (isWebTabKey(path)) return;
      void readDocument(path)
        .then((loaded) => {
          if (loaded.fingerprint) fingerprints.current.set(path, loaded.fingerprint);
          setOpenDocs((prev) =>
            prev.map((d) => (d.path === path ? withTabKey(loaded, d.tabKey) : d)),
          );
          // Drop any cached edits for this path so the editor shows the on-disk content (clean).
          writeEdits((e) => {
            if (!(path in e)) return e;
            const n = { ...e };
            delete n[path];
            return n;
          });
          // Mark any open block draft for this document/page stale — external content must never be
          // overwritten by a Done that started from older bytes.
          if (blockDrafts)
            for (const draft of blockDrafts.snapshotModified())
              if (draft.identity.documentPath === path || draft.identity.pagePath === path)
                blockDrafts.markStale(
                  draft.identity,
                  "Source changed on disk while this block was open.",
                );
        })
        .catch(() => {
          toast("Couldn't reload the changed file on disk", "info");
        });
    },
    [writeEdits, setOpenDocs, fingerprints, withTabKey, blockDrafts],
  );

  const reload = useCallback(
    (path: string, options: { force?: boolean; replayDeferred?: boolean } = {}) => {
      if (isWebTabKey(path)) return;
      if (
        !options.force &&
        (pendingSavePathsStateRef.current.has(path) ||
          hasSynchronousPendingPath(
            path,
            synchronousPendingSavePathsRef,
            synchronousPendingSavePathsRefs,
          ))
      ) {
        // Preserve a real external event that races an in-flight own write; replay after save
        // completion, when selfSaved/fingerprint can distinguish own bytes from external bytes.
        deferredReloadsRef.current.add(path);
        return;
      }
      const selfSavedAtStart = selfSaved.current.has(path);

      const applyReloadForPath = (p: string) => {
        const doc = openDocs.find((d) => d.path === p);
        if (doc) {
          // External change while a block draft is open must keep draft text, mark stale,
          // and block Done — even when the document is already dirty. Do that before
          // deciding between conflict reload vs clean apply.
          if (blockDrafts) {
            for (const draft of blockDrafts.snapshotModified())
              if (draft.identity.documentPath === p || draft.identity.pagePath === p)
                blockDrafts.markStale(
                  draft.identity,
                  "Source changed on disk while this block was open.",
                );
          }
          if (isDirty(doc)) setConflict(p);
          else applyReload(p);
        } else {
          // Sub-page path: also mark any draft for that page stale before the session decides
          // between conflict vs silent refresh (reloadHtmlSubpage handles the buffer but not drafts).
          if (blockDrafts) {
            for (const draft of blockDrafts.snapshotModified())
              if (draft.identity.pagePath === p)
                blockDrafts.markStale(
                  draft.identity,
                  "Source changed on disk while this block was open.",
                );
          }
          reloadHtmlSubpage(p);
        }
      };

      // Fingerprint check: if the stored post-save/post-read fingerprint matches the live one,
      // the change was ours — suppress even if the watcher echo arrived after the selfSaved TTL.
      const prev = fingerprints.current.get(path);
      if (!options.force && prev) {
        void fileFingerprint(path)
          .then((cur) => {
            if (cur && cur.mtimeMs === prev.mtimeMs && cur.size === prev.size) return;
            // The stored fingerprint is the post-write one (the saver drops it when it could not
            // refresh), so a differing live fingerprint is an external write even inside the
            // self-save echo window; only the no-fingerprint fast path below trusts the window.
            applyReloadForPath(path);
          })
          .catch(() => applyReloadForPath(path));
        return;
      }
      if (!options.force && selfSavedAtStart) return; // watcher fast path: ignore our own write
      applyReloadForPath(path);
    },
    [
      openDocs,
      isDirty,
      applyReload,
      reloadHtmlSubpage,
      selfSaved,
      fingerprints,
      setConflict,
      blockDrafts,
      synchronousPendingSavePathsRef,
      synchronousPendingSavePathsRefs,
    ],
  );
  // Stable identity: the `file-changed` listener resubscribes whenever `reload` changes, and the
  // async unlisten/listen gap drops watcher events (one emit per external write, never replayed).
  // `reload`'s deps churn (openDocs, an inline mirror array), so consumers get a forwarding wrapper.
  const reloadRef = useRef(reload);
  reloadRef.current = reload;
  const stableReload = useCallback(
    (path: string, options?: { force?: boolean; replayDeferred?: boolean }) =>
      reloadRef.current(path, options),
    [],
  );
  useEffect(() => {
    for (const path of deferredReloadsRef.current) {
      if (
        pendingSavePaths.has(path) ||
        hasSynchronousPendingPath(
          path,
          synchronousPendingSavePathsRef,
          synchronousPendingSavePathsRefs,
        )
      )
        continue;
      deferredReloadsRef.current.delete(path);
      reload(path, { replayDeferred: true });
    }
  }, [pendingSavePaths, reload, synchronousPendingSavePathsRef, synchronousPendingSavePathsRefs]);
  return {
    teardownDoc,
    doClose,
    reopenLastClosed,
    requestClose,
    confirmSave,
    confirmDontSave,
    applyReload,
    reload: stableReload,
  };
}
