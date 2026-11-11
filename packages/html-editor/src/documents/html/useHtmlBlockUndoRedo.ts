import {
  useCallback,
  useEffect,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import { createShortcutRegistry } from "@rightkit/react-shell";
import type { BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import { ctrlMetaChords } from "../../app/shortcutChords.js";
import type { HtmlSafeBlockTarget } from "./sourceMap.js";
import { sourceChecksum } from "./sourceMap.js";
import { incIframeLoads, incPreviewGenerations, incSourceWrites } from "@rightkit/document-core/documents/perfEventCounters";
import type { HtmlMutationPlan } from "./htmlMutationEngine.js";
import type { HtmlBlockHistoryEntry } from "./useHtmlPreviewLifecycle.js";
import { documentEvent } from "@rightkit/document-core/host";

interface HtmlBlockUndoRedoDeps {
  /** Inactive keep-alive panes stay mounted but must not own global shortcut events. */
  active: boolean;
  /** Wave 2 Read mode: undo/redo mutate page source, so both listeners stay off entirely. */
  readOnly?: boolean;
  blockDrafts: BlockDraftRegistry;
  entryPath: string;
  activePath: string;
  /** Page-scoped inverse history for block Done (one entry per committed plan). */
  blockHistoryRef: MutableRefObject<HtmlBlockHistoryEntry[]>;
  redoHistoryRef: MutableRefObject<HtmlBlockHistoryEntry[]>;
  activeBufferRef: MutableRefObject<string>;
  /** Route one page-source commit to whoever owns that page's buffer. */
  commitSource: (nextSource: string) => void;
  commitPageMutation: (
    source: string,
    plan: HtmlMutationPlan,
  ) => { source: string; inversePlan: HtmlMutationPlan };
  renderedRevisionRef: MutableRefObject<string | null>;
  setApplied: Dispatch<SetStateAction<string>>;
  setSafeTarget: Dispatch<SetStateAction<HtmlSafeBlockTarget | null>>;
}

/**
 * Page-scoped undo/redo for committed block edits: applies the exact inverse plan through the
 * transaction engine (same revision/exact-slice/containment machinery as forward edits), wired to
 * the global viewright:undo/redo events and Ctrl/Cmd+Z/Y while this doc is active. Extracted from
 * HtmlDocument verbatim — no behavior change.
 */
export function useHtmlBlockUndoRedo({
  active,
  readOnly = false,
  blockDrafts,
  entryPath,
  activePath,
  blockHistoryRef,
  redoHistoryRef,
  activeBufferRef,
  commitSource,
  commitPageMutation,
  renderedRevisionRef,
  setApplied,
  setSafeTarget,
}: HtmlBlockUndoRedoDeps) {
  // History/undo handler for block Done — applies the exact inverse plan through the transaction
  // engine (same revision/exact-slice/containment machinery as forward edits). A stale inverse
  // fails closed and drops the entry instead of guessing.
  // biome-ignore lint/correctness/useExhaustiveDependencies: refs/setters arrive as stable hook params
  const undoLastBlock = useCallback((): boolean => {
    const history = blockHistoryRef.current;
    let idx = -1;
    for (let i = history.length - 1; i >= 0; i--)
      if (history[i].pagePath === activePath) {
        idx = i;
        break;
      }
    if (idx === -1) return false;
    const entry = history[idx];
    const current = activeBufferRef.current;
    let undone: { source: string; inversePlan: HtmlMutationPlan };
    try {
      undone = commitPageMutation(current, entry.inversePlan);
    } catch {
      // Stale — the inverse no longer applies to current source.
      blockHistoryRef.current = history.filter((_, i) => i !== idx);
      return false;
    }
    const nextHistory = [...history];
    nextHistory.splice(idx, 1);
    blockHistoryRef.current = nextHistory;
    redoHistoryRef.current.push({
      pagePath: activePath,
      plan: undone.inversePlan, // inverse-of-inverse = the forward plan
      inversePlan: entry.inversePlan,
    });
    if (redoHistoryRef.current.length > 50) redoHistoryRef.current.shift();
    commitSource(undone.source);
    renderedRevisionRef.current = sourceChecksum(undone.source);
    setApplied(undone.source);
    incSourceWrites(1);
    incPreviewGenerations(1);
    incIframeLoads(1);
    setSafeTarget(null);
    return true;
  }, [activePath, commitPageMutation, commitSource]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: refs/setters arrive as stable hook params
  const redoLastBlock = useCallback((): boolean => {
    const history = redoHistoryRef.current;
    let idx = -1;
    for (let i = history.length - 1; i >= 0; i--)
      if (history[i].pagePath === activePath) {
        idx = i;
        break;
      }
    if (idx === -1) return false;
    const entry = history[idx];
    const current = activeBufferRef.current;
    let redone: { source: string; inversePlan: HtmlMutationPlan };
    try {
      redone = commitPageMutation(current, entry.plan);
    } catch {
      redoHistoryRef.current = history.filter((_, i) => i !== idx);
      return false;
    }
    const nextRedo = [...history];
    nextRedo.splice(idx, 1);
    redoHistoryRef.current = nextRedo;
    blockHistoryRef.current.push({
      pagePath: activePath,
      plan: entry.plan,
      inversePlan: redone.inversePlan,
    });
    if (blockHistoryRef.current.length > 50) blockHistoryRef.current.shift();
    commitSource(redone.source);
    renderedRevisionRef.current = sourceChecksum(redone.source);
    setApplied(redone.source);
    incSourceWrites(1);
    incPreviewGenerations(1);
    incIframeLoads(1);
    // Mirror undo: a redo must not leave a stale block target selected either.
    setSafeTarget(null);
    return true;
  }, [activePath, commitPageMutation, commitSource]);

  // Wire undo/redo to global viewright:undo/redo events and Ctrl/Cmd+Z/Y when this doc is active
  useEffect(() => {
    if (!active || readOnly) return;
    const onUndoEvent = (e: Event) => {
      // If a block draft is open, let its local ProseMirror history handle Mod+z
      if (blockDrafts.get({ documentPath: entryPath, pagePath: activePath })) return;
      if (undoLastBlock()) {
        // Mark as handled so other listeners don't double-handle
        (e as Event & { defaultPrevented?: boolean }).preventDefault?.();
      }
    };
    const onRedoEvent = (e: Event) => {
      if (blockDrafts.get({ documentPath: entryPath, pagePath: activePath })) return;
      if (redoLastBlock()) {
        (e as Event & { defaultPrevented?: boolean }).preventDefault?.();
      }
    };
    window.addEventListener(documentEvent("undo"), onUndoEvent);
    window.addEventListener(documentEvent("redo"), onRedoEvent);
    return () => {
      window.removeEventListener(documentEvent("undo"), onUndoEvent);
      window.removeEventListener(documentEvent("redo"), onRedoEvent);
    };
  }, [active, readOnly, blockDrafts, entryPath, activePath, undoLastBlock, redoLastBlock]);

  useEffect(() => {
    if (!active || readOnly) return;
    // A registry that the capture-phase listener delegates to (the listener must keep running before
    // the editors' own handlers). Ctrl, Meta or both with no Alt; an already-prevented event is skipped.
    const shortcuts = createShortcutRegistry();
    const blocked = (event: KeyboardEvent) => {
      // If a block draft is open, its ProseMirror history owns undo
      if (blockDrafts.get({ documentPath: entryPath, pagePath: activePath })) return true;
      // Don't hijack when focus is inside an input/textarea/contenteditable that's not our preview;
      // for other editors (source drawer), let CodeMirror handle
      const target = event.target as HTMLElement | null;
      return Boolean(target?.closest?.("input, textarea, [contenteditable='true']"));
    };
    shortcuts.register({
      id: "html-block.undo",
      label: "Undo block edit",
      chord: ctrlMetaChords("z"),
      hidden: true,
      allowComposing: true,
      run: (event) => {
        if (blocked(event) || !undoLastBlock()) return false;
        event.stopPropagation();
      },
    });
    shortcuts.register({
      id: "html-block.redo",
      label: "Redo block edit",
      chord: [...ctrlMetaChords("z", [true]), ...ctrlMetaChords("y", [false, true])],
      hidden: true,
      allowComposing: true,
      run: (event) => {
        if (blocked(event) || !redoLastBlock()) return false;
        event.stopPropagation();
      },
    });
    const onKeyDown = (event: KeyboardEvent) => {
      shortcuts.handle(event);
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [active, readOnly, blockDrafts, entryPath, activePath, undoLastBlock, redoLastBlock]);
}
