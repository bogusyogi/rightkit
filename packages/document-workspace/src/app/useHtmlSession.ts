import { useCallback, useRef, type Dispatch, type SetStateAction } from "react";
import {
  fileFingerprint,
  parseHostError,
  readDocument,
  writeDocument,
} from "@rightkit/document-core/state/tauriBridge";
import { errorMessage } from "@rightkit/document-core/state/errors";
import { baseName, type LoadedDocument } from "@rightkit/document-core/documents/types";
import type { DocumentTabs } from "./useDocumentTabs.js";
import type { BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import { incDiskWrites } from "@rightkit/document-core/documents/perfEventCounters";

/** The page-aware close/save chooser target: which session, and whether saving should also close it. */
export type HtmlConfirmState = { entryPath: string; mode: "save" | "close" } | null;
/** A sub-page on-disk conflict awaiting the user's reload/keep decision. */
export type HtmlConflictState = { entryPath: string; pagePath: string } | null;

/** Shared tab/edit state this hook reads through (owned by useDocumentTabs, threaded via useDocuments). */
type TabsForHtml = Pick<
  DocumentTabs,
  | "openDocs"
  | "editsRef"
  | "writeEdits"
  | "htmlPagesRef"
  | "writeHtmlPages"
  | "htmlActivePage"
  | "setHtmlActivePage"
>;

interface HtmlSessionDeps extends TabsForHtml {
  // Dialog state stays owned by useDocuments (its `requestClose` opens the close chooser before this
  // hook is constructed, so the setters must exist earlier in the render). This hook drives it.
  htmlConfirm: HtmlConfirmState;
  setHtmlConfirm: Dispatch<SetStateAction<HtmlConfirmState>>;
  htmlConflict: HtmlConflictState;
  setHtmlConflict: Dispatch<SetStateAction<HtmlConflictState>>;
  // Core document callbacks the session leans on: the entry page saves through the shared text path,
  // a successful close routes through doClose, and our own writes are suppressed from the watcher echo.
  onSave: (
    doc: LoadedDocument,
    options?: { silent?: boolean; resolveDrafts?: boolean },
  ) => Promise<string | null>;
  doClose: (path: string) => void;
  markSelfSaved: (path: string) => void;
  setError: Dispatch<SetStateAction<string | null>>;
  onDirty?: (entryPath: string) => void;
  onSaveActivity?: (entryPath: string, delta: 1 | -1) => void;
  blockDrafts?: BlockDraftRegistry;
}

/**
 * HTML site sessions: the navigated SUB-page buffers (the entry page reuses the shared edits/onSave
 * path), their per-page save/discard/dirty model, the page-aware close/save chooser, the sub-page
 * on-disk conflict flow, and the external-change reconciliation for a session's sub-pages.
 *
 * Extracted verbatim from useDocuments so that hook stays a focused open/save/close/conflict surface.
 * Bodies are unchanged from when this lived inline; the only edits are deps-array entries for the
 * dialog/error setters, which are stable props here rather than this hook's own useState setters.
 */
export function useHtmlSession({
  openDocs,
  editsRef,
  writeEdits,
  htmlPagesRef,
  writeHtmlPages,
  htmlActivePage,
  setHtmlActivePage,
  htmlConfirm,
  setHtmlConfirm,
  htmlConflict,
  setHtmlConflict,
  onSave,
  doClose,
  markSelfSaved,
  setError,
  onDirty,
  onSaveActivity,
  blockDrafts,
}: HtmlSessionDeps) {
  // The entry page reuses the shared text path (edits + onSave); these handle the navigated SUB-pages,
  // whose editable buffers live in htmlPages (App level — only the active doc is mounted).

  // The dirty pages of one session: the entry (if its edits differ) plus every sub-page buffer that
  // diverges from disk. Reads the synchronous mirrors so it's correct mid-keystroke.
  const htmlDirtyPages = useCallback(
    (entryPath: string): { path: string; name: string; isEntry: boolean }[] => {
      const out: { path: string; name: string; isEntry: boolean }[] = [];
      const entryDoc = openDocs.find((d) => d.path === entryPath);
      if (entryDoc && (editsRef.current[entryPath] ?? entryDoc.content) !== entryDoc.content) {
        out.push({ path: entryPath, name: baseName(entryPath), isEntry: true });
      }
      const session = htmlPagesRef.current[entryPath] ?? {};
      for (const [path, buf] of Object.entries(session)) {
        if (buf.edited !== buf.original) out.push({ path, name: baseName(path), isEntry: false });
      }
      return out;
    },
    [openDocs, editsRef, htmlPagesRef],
  );

  // Seed a sub-page buffer from disk on first visit (preserving any existing dirty buffer on a
  // back/forward revisit). Called by HtmlDocument as the in-tab nav lands on a non-entry page.
  const ensureHtmlPage = useCallback(
    async (entryPath: string, pagePath: string) => {
      if (pagePath === entryPath) return; // entry uses the shared edits buffer
      if (htmlPagesRef.current[entryPath]?.[pagePath]) return; // already loaded — keep its (maybe dirty) buffer
      try {
        const d = await readDocument(pagePath);
        if (htmlPagesRef.current[entryPath]?.[pagePath]) return; // raced another visit — keep existing
        writeHtmlPages((p) => ({
          ...p,
          [entryPath]: {
            ...(p[entryPath] ?? {}),
            [pagePath]: {
              original: d.content,
              edited: d.content,
              revision: 0,
              encoding: d.encoding ?? "UTF-8",
              fingerprint: d.fingerprint,
              sourceSha256: d.sourceSha256,
            },
          },
        }));
      } catch (e) {
        setError(errorMessage(e));
      }
    },
    [htmlPagesRef, writeHtmlPages, setError],
  );

  // Live edit of a sub-page buffer (the drawer editor's onChange for a non-entry page). The keystroke
  // path: `immediate: false` debounces the React-state publish (App's htmlPropsFor re-render cascade)
  // — see useDocumentTabs' writeHtmlPages. htmlPagesRef is still updated synchronously above, so every
  // save/close/discard path (which reads the ref, not this state) is never stale.
  const setHtmlPageEdited = useCallback(
    (entryPath: string, pagePath: string, edited: string) => {
      writeHtmlPages(
        (p) => {
          const buf = p[entryPath]?.[pagePath];
          if (!buf || buf.edited === edited) return p;
          return {
            ...p,
            [entryPath]: {
              ...p[entryPath],
              [pagePath]: { ...buf, edited, revision: (buf.revision ?? 0) + 1 },
            },
          };
        },
        { immediate: false },
      );
      onDirty?.(entryPath);
    },
    [onDirty, writeHtmlPages],
  );

  // Record which in-site page a tab is currently showing (drives Ctrl+S targeting + dirty marker).
  const setHtmlActive = useCallback(
    (entryPath: string, pagePath: string) => {
      setHtmlActivePage((s) => (s[entryPath] === pagePath ? s : { ...s, [entryPath]: pagePath }));
    },
    [setHtmlActivePage],
  );

  // Revert one page's buffer to its on-disk content (entry → drop its edits; sub → edited = original).
  const discardHtmlPage = useCallback(
    (entryPath: string, pagePath: string) => {
      if (pagePath === entryPath) {
        writeEdits((e) => {
          if (!(entryPath in e)) return e;
          const n = { ...e };
          delete n[entryPath];
          return n;
        });
      } else {
        writeHtmlPages((p) => {
          const buf = p[entryPath]?.[pagePath];
          if (!buf) return p;
          return {
            ...p,
            [entryPath]: {
              ...p[entryPath],
              [pagePath]: { ...buf, edited: buf.original, revision: (buf.revision ?? 0) + 1 },
            },
          };
        });
      }
    },
    [writeEdits, writeHtmlPages],
  );

  // Save one page. Entry routes through the shared text save (encoding/fingerprint already handled).
  // For a sub-page: a save-time fingerprint re-check guards against silently clobbering an external
  // edit (defense-in-depth alongside the background watcher) — on a mismatch it raises the conflict
  // dialog instead of writing. Returns true only when the page was actually written.
  const htmlSaveSeqRef = useRef<Map<string, number>>(new Map());
  const saveHtmlPage = useCallback(
    async (
      entryPath: string,
      pagePath: string,
      options: { silent?: boolean } = {},
    ): Promise<boolean> => {
      // A manual page save resolves that page's block draft first (Done → commit → write). Autosave
      // (silent) never commits a draft. A stale/failed draft blocks the write.
      if (!options.silent && blockDrafts) {
        const draft = blockDrafts.get({ documentPath: entryPath, pagePath });
        if (draft) {
          const result = await blockDrafts.resolve(draft.identity, "manual-save", "done");
          if (result.outcome === "failed" || result.outcome === "cancelled") return false;
        }
      }
      if (pagePath === entryPath) {
        const doc = openDocs.find((d) => d.path === entryPath);
        if (!doc) return false;
        // Page-specific draft already resolved above; prevent onSave's generic draft resolution
        // from double-committing other page drafts when this is Save Current.
        const saved = await onSave(doc, { ...options, resolveDrafts: false } as typeof options & {
          resolveDrafts: boolean;
        });
        return saved !== null;
      }
      const buf = htmlPagesRef.current[entryPath]?.[pagePath];
      if (!buf) return false;
      if (buf.edited === buf.original) return true; // already clean — nothing to write
      const submitted = buf.edited; // bytes handed to the writer; completion marks only these clean (#362)
      const saveKey = `${entryPath}\u0000${pagePath}`;
      let saveSeq = 0;
      onSaveActivity?.(entryPath, 1);
      try {
        const cur = await fileFingerprint(pagePath);
        if (
          buf.fingerprint &&
          cur &&
          (cur.mtimeMs !== buf.fingerprint.mtimeMs || cur.size !== buf.fingerprint.size)
        ) {
          setHtmlConflict({ entryPath, pagePath });
          return false;
        }
        // Sequence only saves that reach the writer, so a later save refused on conflict
        // cannot orphan this completion's metadata.
        saveSeq = (htmlSaveSeqRef.current.get(saveKey) ?? 0) + 1;
        htmlSaveSeqRef.current.set(saveKey, saveSeq);
        const result = await writeDocument(pagePath, submitted, buf.encoding, buf.sourceSha256);
        const written = typeof result === "string" ? result : result.encoding;
        const sourceSha256 =
          typeof result === "string" ? buf.sourceSha256 : result.sourceSha256 || buf.sourceSha256;
        incDiskWrites(1);
        markSelfSaved(pagePath);
        const fp = await fileFingerprint(pagePath);
        writeHtmlPages((p) => {
          const cur2 = p[entryPath]?.[pagePath];
          if (!cur2) return p;
          // An older completion must not overwrite metadata from a newer save of this page.
          if (htmlSaveSeqRef.current.get(saveKey) !== saveSeq) return p;
          return {
            ...p,
            [entryPath]: {
              ...p[entryPath],
              [pagePath]: {
                ...cur2,
                original: submitted,
                stale: false,
                encoding: written,
                fingerprint: fp ?? cur2.fingerprint,
                sourceSha256,
              },
            },
          };
        });
        return true;
      } catch (e) {
        if (parseHostError(e).code === "write_conflict")
          setHtmlConflict({ entryPath, pagePath });
        else setError(errorMessage(e));
        return false;
      } finally {
        onSaveActivity?.(entryPath, -1);
      }
    },
    [
      openDocs,
      onSave,
      htmlPagesRef,
      writeHtmlPages,
      markSelfSaved,
      setHtmlConflict,
      setError,
      blockDrafts,
      onSaveActivity,
    ],
  );

  // Save every dirty page in the session, stopping (returning false) on the first that can't write
  // (e.g. a conflict) so the caller doesn't then close the tab over unsaved work.
  const saveAllHtmlPages = useCallback(
    async (entryPath: string, options: { silent?: boolean } = {}): Promise<boolean> => {
      const canonicalDirty = htmlDirtyPages(entryPath);
      // Manual save must also persist pages that have an open modified draft even if the
      // canonical buffer hasn't changed yet (draft holds the edit). Autosave (silent) intentionally
      // skips drafts — it saves canonical dirty content only and never commits a draft.
      let pages = canonicalDirty;
      if (!options.silent && blockDrafts) {
        const draftPaths = new Set(
          blockDrafts.snapshotModified(entryPath).map((d) => d.identity.pagePath),
        );
        const already = new Set(canonicalDirty.map((p) => p.path));
        for (const pagePath of draftPaths) {
          if (!already.has(pagePath)) {
            pages = [
              ...pages,
              { path: pagePath, name: baseName(pagePath), isEntry: pagePath === entryPath },
            ];
          }
        }
      }
      for (const p of pages) {
        if (!(await saveHtmlPage(entryPath, p.path, options))) return false;
      }
      return true;
    },
    [htmlDirtyPages, saveHtmlPage, blockDrafts],
  );

  const discardAllHtmlPages = useCallback(
    (entryPath: string) => {
      // Discard any open block drafts for this session first — Don't Save means discard edit, not keep.
      if (blockDrafts) {
        for (const draft of blockDrafts.snapshotModified(entryPath)) {
          void blockDrafts.resolve(draft.identity, "close", "discard");
        }
      }
      writeEdits((e) => {
        if (!(entryPath in e)) return e;
        const n = { ...e };
        delete n[entryPath];
        return n;
      });
      writeHtmlPages((p) => {
        const session = p[entryPath];
        if (!session) return p;
        const next: typeof session = {};
        for (const k of Object.keys(session))
          next[k] = {
            ...session[k],
            edited: session[k].original,
            revision: (session[k].revision ?? 0) + 1,
          };
        return { ...p, [entryPath]: next };
      });
    },
    [writeEdits, writeHtmlPages, blockDrafts],
  );

  // Ctrl+S on an HTML tab: one dirty page saves directly; several open the page-aware chooser.
  const onHtmlSaveShortcut = useCallback(
    (entryPath: string) => {
      const canonical = htmlDirtyPages(entryPath);
      // Include draft pages so Ctrl/Cmd+S inside a valid draft resolves once then saves once
      // even when the canonical buffer is still clean (draft holds the edit).
      let dirty = canonical;
      if (blockDrafts) {
        const draftPaths = new Set(
          blockDrafts.snapshotModified(entryPath).map((d) => d.identity.pagePath),
        );
        const already = new Set(canonical.map((p) => p.path));
        for (const pagePath of draftPaths) {
          if (!already.has(pagePath)) {
            dirty = [
              ...dirty,
              { path: pagePath, name: baseName(pagePath), isEntry: pagePath === entryPath },
            ];
          }
        }
      }
      if (dirty.length === 0) return;
      if (dirty.length === 1) {
        void saveHtmlPage(entryPath, dirty[0].path);
        return;
      }
      setHtmlConfirm({ entryPath, mode: "save" });
    },
    [htmlDirtyPages, saveHtmlPage, setHtmlConfirm, blockDrafts],
  );

  // Close/save chooser actions. In "close" mode each save path also closes the tab — but only once
  // the save actually succeeds, so a conflict mid-close leaves the tab open with its conflict prompt.
  const htmlSaveCurrent = useCallback(async () => {
    const c = htmlConfirm;
    if (!c) return;
    setHtmlConfirm(null);
    const active = htmlActivePage[c.entryPath] ?? c.entryPath;
    const ok = await saveHtmlPage(c.entryPath, active);
    if (c.mode === "close" && ok) doClose(c.entryPath);
  }, [htmlConfirm, htmlActivePage, saveHtmlPage, doClose, setHtmlConfirm]);

  const htmlSaveAll = useCallback(async () => {
    const c = htmlConfirm;
    if (!c) return;
    setHtmlConfirm(null);
    const ok = await saveAllHtmlPages(c.entryPath);
    if (c.mode === "close" && ok) doClose(c.entryPath);
  }, [htmlConfirm, saveAllHtmlPages, doClose, setHtmlConfirm]);

  const htmlDiscardAll = useCallback(() => {
    const c = htmlConfirm;
    if (!c) return;
    setHtmlConfirm(null);
    discardAllHtmlPages(c.entryPath);
    if (c.mode === "close") doClose(c.entryPath);
  }, [htmlConfirm, discardAllHtmlPages, doClose, setHtmlConfirm]);

  const htmlConfirmCancel = useCallback(() => setHtmlConfirm(null), [setHtmlConfirm]);

  // Sub-page on-disk conflict (from the save-time guard or the watcher): reload re-reads the file
  // into the buffer (discarding edits); keep dismisses and leaves the buffer as-is.
  const htmlConflictReload = useCallback(async () => {
    const c = htmlConflict;
    if (!c) return;
    setHtmlConflict(null);
    try {
      const d = await readDocument(c.pagePath);
      writeHtmlPages((p) => ({
        ...p,
        [c.entryPath]: {
          ...(p[c.entryPath] ?? {}),
          [c.pagePath]: {
            original: d.content,
            edited: d.content,
            revision: (p[c.entryPath]?.[c.pagePath]?.revision ?? 0) + 1,
            encoding: d.encoding ?? "UTF-8",
            fingerprint: d.fingerprint,
            sourceSha256: d.sourceSha256,
          },
        },
      }));
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [htmlConflict, writeHtmlPages, setHtmlConflict, setError]);

  const htmlConflictKeep = useCallback(async () => {
    const c = htmlConflict;
    if (!c) return;
    try {
      const d = await readDocument(c.pagePath);
      if (!d.sourceSha256) {
        setError("Couldn't refresh file identity before overwrite.");
        return;
      }
      writeHtmlPages((p) => {
        const session = p[c.entryPath];
        const current = session?.[c.pagePath];
        if (!session || !current) return p;
        return {
          ...p,
          [c.entryPath]: {
            ...session,
            [c.pagePath]: {
              ...current,
              fingerprint: d.fingerprint ?? current.fingerprint,
              sourceSha256: d.sourceSha256,
            },
          },
        };
      });
      setHtmlConflict(null);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, [htmlConflict, setError, setHtmlConflict, writeHtmlPages]);

  // A reloaded path that isn't an open tab may be a navigated sub-page of an open HTML site session.
  // A dirty buffer raises the conflict chooser; a clean one silently refreshes from disk and rebuilds.
  // No-op when the path belongs to no live session (the caller has already handled open tabs).
  const reloadHtmlSubpage = useCallback(
    (path: string) => {
      for (const entryPath in htmlPagesRef.current) {
        const buf = htmlPagesRef.current[entryPath]?.[path];
        if (!buf) continue;
        if (buf.edited !== buf.original) {
          // Dirty buffer — let the user choose, and keep the draft marked stale until it is reloaded or saved.
          writeHtmlPages((p) => {
            const cur = p[entryPath]?.[path];
            if (!cur || cur.stale) return p;
            return { ...p, [entryPath]: { ...p[entryPath], [path]: { ...cur, stale: true } } };
          });
          setHtmlConflict({ entryPath, pagePath: path });
        } else {
          // Clean buffer — silently refresh it from disk and rebuild the preview.
          const readOriginal = buf.original;
          const readRevision = buf.revision;
          let raced = false;
          void readDocument(path)
            .then((d) => {
              writeHtmlPages((p) => {
                const live = p[entryPath]?.[path];
                if (!live) return p; // closed / navigated away meanwhile
                // Edits (or another reload) landed during the disk read: keep them, raise conflict (#363).
                if (
                  live.edited !== live.original ||
                  live.original !== readOriginal ||
                  live.revision !== readRevision
                ) {
                  if (live.edited !== live.original) {
                    raced = true;
                    return { ...p, [entryPath]: { ...p[entryPath], [path]: { ...live, stale: true } } };
                  }
                  return p; // superseded by a newer clean refresh
                }
                return {
                  ...p,
                  [entryPath]: {
                    ...p[entryPath],
                    [path]: {
                      original: d.content,
                      edited: d.content,
                      revision: (p[entryPath]?.[path]?.revision ?? 0) + 1,
                      encoding: d.encoding ?? "UTF-8",
                      fingerprint: d.fingerprint,
                      sourceSha256: d.sourceSha256,
                    },
                  },
                };
              });
              if (raced) setHtmlConflict({ entryPath, pagePath: path });
            })
            .catch(() => {
              // A transient external reload failure is retried by the existing file watcher.
            });
        }
        return;
      }
    },
    [htmlPagesRef, writeHtmlPages, setHtmlConflict],
  );

  return {
    ensureHtmlPage,
    setHtmlPageEdited,
    setHtmlActive,
    discardHtmlPage,
    saveHtmlPage,
    saveAllHtmlPages,
    discardAllHtmlPages,
    onHtmlSaveShortcut,
    htmlDirtyPages,
    htmlSaveCurrent,
    htmlSaveAll,
    htmlDiscardAll,
    htmlConfirmCancel,
    htmlConflictReload,
    htmlConflictKeep,
    reloadHtmlSubpage,
  };
}
