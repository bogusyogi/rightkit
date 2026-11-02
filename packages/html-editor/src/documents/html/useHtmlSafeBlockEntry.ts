import {
  useCallback,
  useEffect,
  useRef,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import { logEvent } from "@rightkit/document-core/state/diagnostics";
import { toast, type ToastAction } from "@rightkit/platform-ui/headless";
import type { EditorView } from "@codemirror/view";
import type { BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import type { ViewMode } from "@rightkit/document-core/documents/types";
import { HtmlMutationError, type HtmlMutationPlan } from "./htmlMutationEngine.js";
import { resolveMutationPlan } from "./htmlMutationResolver.js";
import {
  findSourceRange,
  parseAndMapSource,
  resolveSafeBlockTarget,
  type HtmlSafeBlockTarget,
  type SourceRange,
} from "./sourceMap.js";
import type { HtmlSafeBlockEditorHandle } from "./HtmlSafeBlockEditor.js";

/** Matches the error toast lifetime: a refusal repeated while its toast is still visible is not re-shown. */
const BLOCK_TOAST_REPEAT_MS = 6_000;

interface Committed {
  source: string;
  inversePlan: HtmlMutationPlan;
}

interface Args {
  readOnly: boolean;
  mode: "interactive" | "preview";
  drawer: boolean;
  entryPath: string;
  activePath: string;
  blockDrafts: BlockDraftRegistry;
  mountedRef: MutableRefObject<boolean>;
  activeBufferRef: MutableRefObject<string>;
  sourceEditorRef: MutableRefObject<EditorView | null>;
  pendingInspectRangeRef: MutableRefObject<SourceRange | null>;
  richEditRef: MutableRefObject<HtmlSafeBlockEditorHandle | null>;
  pendingEditorCloseRef: MutableRefObject<{ srcDocAtCommit: string; timer: number } | null>;
  pendingSafeTargetRef: MutableRefObject<HtmlSafeBlockTarget | null>;
  restoreInteractiveAfterEditRef: MutableRefObject<boolean>;
  lastBlockCommitSourceRef: MutableRefObject<string | null>;
  onChangeView?: (view: ViewMode) => void;
  setFallbackView: Dispatch<SetStateAction<ViewMode | null>>;
  setDrawerOpen: Dispatch<SetStateAction<boolean>>;
  setSafeTarget: Dispatch<SetStateAction<HtmlSafeBlockTarget | null>>;
  revealSourceRange: (range: SourceRange) => boolean;
  restoreInteractiveAfterTransientEdit: () => void;
  revisionForSource: (source: string) => string | number;
  commitPageMutation: (source: string, plan: HtmlMutationPlan) => Committed;
  recordBlockCommit: (plan: HtmlMutationPlan, committed: Committed) => void;
}

/**
 * Entry into block editing: refusal toasts, "edit in Source" fallbacks, creating the draft against
 * the Safe DOM, and the Interactive -> Safe handoff for a double-click edit request.
 */
export function useHtmlSafeBlockEntry({
  readOnly,
  mode,
  drawer,
  entryPath,
  activePath,
  blockDrafts,
  mountedRef,
  activeBufferRef,
  sourceEditorRef,
  pendingInspectRangeRef,
  richEditRef,
  pendingEditorCloseRef,
  pendingSafeTargetRef,
  restoreInteractiveAfterEditRef,
  lastBlockCommitSourceRef,
  onChangeView,
  setFallbackView,
  setDrawerOpen,
  setSafeTarget,
  revealSourceRange,
  restoreInteractiveAfterTransientEdit,
  revisionForSource,
  commitPageMutation,
  recordBlockCommit,
}: Args) {
  // Block-selection refusals repeat on every click; show one notice per message rather than a stack.
  const lastBlockToastRef = useRef<{ message: string; at: number } | null>(null);
  const blockSelectionToast = useCallback((message: string, action?: ToastAction) => {
    const now = Date.now();
    const last = lastBlockToastRef.current;
    if (last && last.message === message && now - last.at < BLOCK_TOAST_REPEAT_MS) return;
    lastBlockToastRef.current = { message, at: now };
    toast(message, action ? { kind: "error", action } : "error");
  }, []);
  // Ambiguous or runtime-only targets have no exact Safe block, so Source is the editor left. Open it
  // at the best-known offset with the same stash-then-reveal handoff the inspect path uses.
  const openSourceAtRange = useCallback(
    (range: ReturnType<typeof findSourceRange>) => {
      if (drawer) {
        if (range) revealSourceRange(range);
        return;
      }
      pendingInspectRangeRef.current = range;
      setDrawerOpen(true);
    },
    [drawer, pendingInspectRangeRef, revealSourceRange, setDrawerOpen],
  );
  const openSourceAtTarget = useCallback(
    (target: HtmlSafeBlockTarget) =>
      openSourceAtRange(findSourceRange(parseAndMapSource(activeBufferRef.current), target)),
    [activeBufferRef, openSourceAtRange],
  );
  // Interactive double-click on content outside any editable block: fixed notice only, no target.
  const handleEditRefused = useCallback(
    (pagePath: string) => {
      if (readOnly || pagePath !== activePath) return;
      blockSelectionToast("This content can only be edited in Source", {
        label: "Edit in Source",
        run: () => openSourceAtRange(null),
      });
    },
    [activePath, blockSelectionToast, openSourceAtRange, readOnly],
  );
  const openSafeBlockTarget = useCallback(
    (target: HtmlSafeBlockTarget) => {
      if (drawer) {
        toast("Close Source before entering HTML rich edit.", "error");
        sourceEditorRef.current?.focus();
        return false;
      }
      if (target.pagePath !== activePath) return false;
      const match = resolveSafeBlockTarget(activeBufferRef.current, target, () =>
        logEvent("html:block-entry-refused"),
      );
      if (!match) {
        blockSelectionToast("This content can only be edited in Source", {
          label: "Edit in Source",
          run: () => openSourceAtTarget(target),
        });
        return false;
      }
      const identity = { documentPath: entryPath, pagePath: activePath };
      const existingDraft = blockDrafts.get(identity);
      if (existingDraft) {
        const existingRange = existingDraft.sourceRange;
        const sameTarget =
          !!existingRange &&
          existingRange.start === match.range.start &&
          existingRange.end === match.range.end;
        if (sameTarget) setSafeTarget(target);
        else {
          blockSelectionToast("Finish the current HTML edit before selecting another block.");
          richEditRef.current?.focus();
        }
        return sameTarget;
      }
      const started = blockDrafts.begin({
        identity,
        blockKind: `html:${match.tagName}`,
        originalRevision: revisionForSource(activeBufferRef.current),
        sourceRange: match.range,
        sourceChecksum: match.checksum,
        originalText: activeBufferRef.current.slice(match.innerRange.start, match.innerRange.end),
        selectionBookmark: target.textSelection
          ? { kind: "html-text", ...target.textSelection }
          : undefined,
        discard: () => {},
        commit: (snapshot) => {
          if (!mountedRef.current)
            return { ok: false, kind: "stale", reason: "Document closed before block commit." };
          const current = activeBufferRef.current;
          const range = snapshot.sourceRange;
          if (!range)
            return { ok: false, kind: "error", reason: "Block has no source range to commit." };
          const currentMatch = resolveSafeBlockTarget(current, target);
          if (!currentMatch || currentMatch.checksum !== snapshot.sourceChecksum)
            return {
              ok: false,
              kind: "stale",
              reason: "Block no longer maps uniquely to Safe preview.",
            };
          // Done path: the operation-aware resolver expresses the inner-range edit (and optional
          // block rename) as one minimal-patch plan that preserves untouched authored nodes;
          // commitHtmlMutation owns every invariant (revision, slices, containment, parse,
          // structure, shell, inverse).
          const resolution = resolveMutationPlan(
            current,
            {
              kind: "replaceText",
              target: currentMatch.range,
              newInnerHtml: snapshot.currentText,
              targetTag: snapshot.targetTag ?? undefined,
            },
            snapshot.originalRevision,
          );
          if (!resolution.ok) return { ok: false, kind: "error", reason: resolution.reason };
          const plan: HtmlMutationPlan = resolution.plan;
          let committed: Committed;
          try {
            committed = commitPageMutation(current, plan);
          } catch (error) {
            // A revision or exact-slice mismatch means the source moved under the draft — stale.
            if (
              error instanceof HtmlMutationError &&
              (error.code === "stale-revision" || error.code === "expected-text-mismatch")
            )
              return {
                ok: false,
                kind: "stale",
                reason: "Source changed while this block was open.",
              };
            return {
              ok: false,
              kind: "error",
              reason: error instanceof Error ? error.message : String(error),
            };
          }
          // Publish the fresh bytes for any chained operation (structural action) that must plan
          // against the post-commit source before the buffer state update re-renders.
          lastBlockCommitSourceRef.current = committed.source;
          recordBlockCommit(plan, committed);
          return { ok: true };
        },
      });
      if (started.ok) {
        // Retire previous session's deferred close before opening another draft.
        // Its timer/source rebuild must never close this new editor.
        const pendingClose = pendingEditorCloseRef.current;
        if (pendingClose) window.clearTimeout(pendingClose.timer);
        pendingEditorCloseRef.current = null;
        setSafeTarget(target);
      }
      return started.ok;
    },
    [
      activeBufferRef,
      activePath,
      blockDrafts,
      blockSelectionToast,
      commitPageMutation,
      drawer,
      entryPath,
      lastBlockCommitSourceRef,
      mountedRef,
      openSourceAtTarget,
      pendingEditorCloseRef,
      recordBlockCommit,
      revisionForSource,
      richEditRef,
      setSafeTarget,
      sourceEditorRef,
    ],
  );

  // Interactive content is scripted & immutable. A double-click is an edit request only: switch
  // to Safe first, then create the draft against Safe's in-frame DOM. This keeps no editor, draft,
  // or parent overlay alive over a running page.
  const handleSafeBlockTarget = useCallback(
    (target: HtmlSafeBlockTarget) => {
      // Read mode: block editing is an authoring surface — ignore the double-click entirely.
      if (readOnly) return;
      if (mode === "interactive") {
        restoreInteractiveAfterEditRef.current = true;
        pendingSafeTargetRef.current = target;
        if (onChangeView) onChangeView("preview");
        else setFallbackView("preview");
        return;
      }
      openSafeBlockTarget(target);
    },
    [
      mode,
      onChangeView,
      openSafeBlockTarget,
      pendingSafeTargetRef,
      readOnly,
      restoreInteractiveAfterEditRef,
      setFallbackView,
    ],
  );
  useEffect(() => {
    const target = pendingSafeTargetRef.current;
    if (mode !== "preview" || !target) return;
    pendingSafeTargetRef.current = null;
    // Announce the switch once, and only when the block editor opened; a refused target restores
    // Interactive and shows its own notice instead.
    if (!openSafeBlockTarget(target)) restoreInteractiveAfterTransientEdit();
    else toast("Switched to Safe view to edit. Scripts do not run.", "info");
  }, [mode, openSafeBlockTarget, pendingSafeTargetRef, restoreInteractiveAfterTransientEdit]);

  return { handleSafeBlockTarget, handleEditRefused, openSourceAtRange };
}
