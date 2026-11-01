import { useCallback, useMemo, type Dispatch, type MutableRefObject, type SetStateAction } from "react";
import { toast } from "@rightkit/platform-ui/headless";
import { errorMessage } from "@rightkit/document-core/state/errors";
import type { BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import { incHistoryEntries, incIframeLoads, incPreviewGenerations, incSourceWrites } from "@rightkit/document-core/documents/perfEventCounters";
import type { HtmlMutationPlan } from "./htmlMutationEngine";
import {
  HtmlOperationError,
  deleteNodePlan,
  imageInsertPlan,
  imageReplacePlan,
  insertSiblingPlan,
  locateElement,
  moveSiblingPlan,
  planStructuralOp,
  removeAttributePlan,
  removeLinkPlan,
  resolveInlineAnchor,
  setAttributePlan,
  setLinkPlan,
  unwrapPlan,
  wrapPlan,
  type RichBlockOp,
  type StructuralAction,
  type StructuralSurface,
} from "./htmlOperationPlanner";
import { decodeHtmlEntities, resolveSafeBlockTarget, sourceChecksum, type HtmlSafeBlockTarget } from "./sourceMap";
import type { HtmlBlockHistoryEntry } from "./useHtmlPreviewLifecycle";

interface Committed {
  source: string;
  inversePlan: HtmlMutationPlan;
}

interface RecordArgs {
  activePath: string;
  blockHistoryRef: MutableRefObject<HtmlBlockHistoryEntry[]>;
  redoHistoryRef: MutableRefObject<HtmlBlockHistoryEntry[]>;
  renderedRevisionRef: MutableRefObject<string | null>;
  commitSource: (source: string) => void;
  setApplied: Dispatch<SetStateAction<string>>;
}

/** One shared tail for every committed visual edit: history, redo invalidation, buffer, preview. */
export function useRecordBlockCommit({
  activePath,
  blockHistoryRef,
  redoHistoryRef,
  renderedRevisionRef,
  commitSource,
  setApplied,
}: RecordArgs) {
  return useCallback(
    (plan: HtmlMutationPlan, committed: Committed) => {
      blockHistoryRef.current.push({
        pagePath: activePath,
        plan,
        inversePlan: committed.inversePlan,
      });
      // Keep only last 50 per page to bound memory.
      if (blockHistoryRef.current.length > 50) blockHistoryRef.current.shift();
      // New committed edit invalidates redo stack
      redoHistoryRef.current = redoHistoryRef.current.filter((e) => e.pagePath !== activePath);
      commitSource(committed.source);
      // Mark the committed revision as already-rendered BEFORE setApplied so a later save of the
      // same bytes is a no-op instead of a reload (renderedRevision is sole owner).
      renderedRevisionRef.current = sourceChecksum(committed.source);
      setApplied(committed.source);
      incSourceWrites(1);
      incHistoryEntries(1);
      incPreviewGenerations(1);
      incIframeLoads(1);
    },
    [activePath, blockHistoryRef, commitSource, redoHistoryRef, renderedRevisionRef, setApplied],
  );
}

interface Args {
  entryPath: string;
  activePath: string;
  activeBuffer: string;
  activeBufferRef: MutableRefObject<string>;
  mountedRef: MutableRefObject<boolean>;
  blockDrafts: BlockDraftRegistry;
  safeTarget: HtmlSafeBlockTarget | null;
  lastBlockCommitSourceRef: MutableRefObject<string | null>;
  closeInlineEditor: () => void;
  revisionForSource: (source: string) => string | number;
  commitPageMutation: (source: string, plan: HtmlMutationPlan) => Committed;
  recordBlockCommit: (plan: HtmlMutationPlan, committed: Committed) => void;
}

/** Structural/rich operations on the selected block, each one atomic through commitHtmlMutation. */
export function useHtmlBlockOps({
  entryPath,
  activePath,
  activeBuffer,
  activeBufferRef,
  mountedRef,
  blockDrafts,
  safeTarget,
  lastBlockCommitSourceRef,
  closeInlineEditor,
  revisionForSource,
  commitPageMutation,
  recordBlockCommit,
}: Args) {
  // Commit the open draft if modified (else discard it), close the editor, and re-resolve the
  // rendered locator with its text fingerprint relaxed — the user may have just edited that text —
  // leaving the child path + tag + class identity. Returns null when the op must stop.
  const prepareOp = useCallback(async () => {
    if (!mountedRef.current) return null;
    const identity = { documentPath: entryPath, pagePath: activePath };
    if (!blockDrafts.get(identity) || !safeTarget) return null;
    // Only a commit issued BY THIS invocation may supply fresh bytes; a value left over from an
    // earlier operation must never leak into a later one.
    lastBlockCommitSourceRef.current = null;
    if (blockDrafts.hasModifiedDraft(identity)) {
      const result = await blockDrafts.resolve(identity, "manual-save", "done");
      if (result.outcome !== "done") return null; // commit rejected — source untouched, editor stays
    } else {
      blockDrafts.discard(identity);
    }
    closeInlineEditor();
    // The draft-commit path parks the committed source in lastBlockCommitSourceRef because the
    // buffer state update is async; planning against the pre-edit buffer would overwrite the
    // just-committed text edit.
    const current = lastBlockCommitSourceRef.current ?? activeBufferRef.current;
    lastBlockCommitSourceRef.current = null;
    const locator: HtmlSafeBlockTarget = {
      ...safeTarget,
      // tag|id|class prefix only — the text segment changes with the edit.
      fingerprint: [
        safeTarget.tagName ?? "",
        safeTarget.id ?? "",
        (safeTarget.classNames ?? []).join(" "),
      ].join("|"),
    };
    const reResolved = resolveSafeBlockTarget(current, locator);
    if (!reResolved) {
      toast("The block changed shape; click it again to edit.", "error");
      return null;
    }
    return { current, range: reResolved.range, target: safeTarget };
  }, [
    activeBufferRef,
    activePath,
    blockDrafts,
    closeInlineEditor,
    entryPath,
    lastBlockCommitSourceRef,
    mountedRef,
    safeTarget,
  ]);

  const commitPlan = useCallback(
    (current: string, plan: HtmlMutationPlan) => {
      let committed: Committed;
      try {
        committed = commitPageMutation(current, plan);
      } catch (error) {
        toast(errorMessage(error), "error");
        return;
      }
      recordBlockCommit(plan, committed);
    },
    [commitPageMutation, recordBlockCommit],
  );

  const runBlockStructuralOp = useCallback(
    async (surface: StructuralSurface, action: StructuralAction) => {
      const prepared = await prepareOp();
      if (!prepared) return;
      const { current, range } = prepared;
      let plan: HtmlMutationPlan;
      try {
        plan = planStructuralOp(current, surface, action, range, revisionForSource(current));
      } catch (error) {
        toast(errorMessage(error), "error");
        return;
      }
      commitPlan(current, plan);
    },
    [commitPlan, prepareOp, revisionForSource],
  );

  const runBlockRichOp = useCallback(
    async (op: RichBlockOp) => {
      const prepared = await prepareOp();
      if (!prepared) return;
      const { current, range, target } = prepared;
      let plan: HtmlMutationPlan;
      try {
        const revision = revisionForSource(current);
        switch (op.kind) {
          case "wrap":
            plan = wrapPlan(current, range, "div", revision);
            break;
          case "unwrap":
            plan = unwrapPlan(current, range, revision);
            break;
          case "deleteNode":
            plan = deleteNodePlan(current, range, revision);
            break;
          case "moveUp":
            plan = moveSiblingPlan(current, range, "up", revision);
            break;
          case "moveDown":
            plan = moveSiblingPlan(current, range, "down", revision);
            break;
          case "insertSibling":
            plan = insertSiblingPlan(current, range, op.tag, op.position, revision);
            break;
          case "setAttribute":
            // An image click opens its containing safe block for transient editing, but Properties
            // belongs to authored `<img>` itself. Resolve its reported source identity against
            // canonical bytes before planning so container attributes remain untouched.
            if (target.src !== undefined) {
              const image = resolveInlineAnchor(current, range, { src: target.src });
              if (!image || image.kind !== "image")
                throw new HtmlOperationError(
                  "target-unresolved",
                  "The image no longer resolves uniquely in source.",
                );
              plan = setAttributePlan(current, image.element.range, op.name, op.value, revision);
            } else plan = setAttributePlan(current, range, op.name, op.value, revision);
            break;
          case "removeAttribute":
            if (target.src !== undefined) {
              const image = resolveInlineAnchor(current, range, { src: target.src });
              if (!image || image.kind !== "image")
                throw new HtmlOperationError(
                  "target-unresolved",
                  "The image no longer resolves uniquely in source.",
                );
              plan = removeAttributePlan(current, image.element.range, op.name, revision);
            } else plan = removeAttributePlan(current, range, op.name, revision);
            break;
          case "setLink": {
            const anchor = resolveInlineAnchor(current, range, { href: target.href });
            if (!anchor || anchor.kind !== "link")
              throw new HtmlOperationError(
                "target-unresolved",
                "The link no longer resolves uniquely in source.",
              );
            plan = setLinkPlan(current, anchor.element.range, op.href, revision);
            break;
          }
          case "removeLink": {
            const anchor = resolveInlineAnchor(current, range, { href: target.href });
            if (!anchor || anchor.kind !== "link")
              throw new HtmlOperationError(
                "target-unresolved",
                "The link no longer resolves uniquely in source.",
              );
            plan = removeLinkPlan(current, anchor.element.range, revision);
            break;
          }
          case "imageReplace": {
            const anchor = resolveInlineAnchor(current, range, { src: target.src });
            if (!anchor || anchor.kind !== "image")
              throw new HtmlOperationError(
                "target-unresolved",
                "The image no longer resolves uniquely in source.",
              );
            plan = imageReplacePlan(current, anchor.element.range, op.src, revision);
            break;
          }
          case "imageInsert":
            plan = imageInsertPlan(current, range, op.src, op.position, revision);
            break;
        }
      } catch (error) {
        toast(errorMessage(error), "error");
        return;
      }
      commitPlan(current, plan);
    },
    [commitPlan, prepareOp, revisionForSource],
  );

  // Keep applicability beside source resolution, not in toolbar React. A command is enabled only
  // when its existing pure planner can make an atomic proposal against this exact canonical target.
  const canRunRichOp = useCallback(
    (op: RichBlockOp): boolean => {
      if (!safeTarget) return false;
      const current = activeBufferRef.current;
      const match = resolveSafeBlockTarget(current, safeTarget);
      if (!match) return false;
      const revision = revisionForSource(current);
      try {
        switch (op.kind) {
          case "wrap":
            wrapPlan(current, match.range, "div", revision);
            return true;
          case "unwrap":
            unwrapPlan(current, match.range, revision);
            return true;
          case "deleteNode":
            deleteNodePlan(current, match.range, revision);
            return true;
          case "moveUp":
            moveSiblingPlan(current, match.range, "up", revision);
            return true;
          case "moveDown":
            moveSiblingPlan(current, match.range, "down", revision);
            return true;
          case "insertSibling":
            insertSiblingPlan(current, match.range, op.tag, op.position, revision);
            return true;
          case "removeLink": {
            const anchor = resolveInlineAnchor(current, match.range, { href: safeTarget.href });
            if (!anchor || anchor.kind !== "link") return false;
            removeLinkPlan(current, anchor.element.range, revision);
            return true;
          }
          case "setLink": {
            const anchor = resolveInlineAnchor(current, match.range, { href: safeTarget.href });
            if (!anchor || anchor.kind !== "link") return false;
            setLinkPlan(current, anchor.element.range, op.href, revision);
            return true;
          }
          case "imageReplace": {
            const anchor = resolveInlineAnchor(current, match.range, { src: safeTarget.src });
            if (!anchor || anchor.kind !== "image") return false;
            imageReplacePlan(current, anchor.element.range, op.src, revision);
            return true;
          }
          case "imageInsert":
            imageInsertPlan(current, match.range, op.src, op.position, revision);
            return true;
          // Properties validates its arbitrary name/value only at Apply time; no fabricated
          // placeholder attribute may stand in for a user command.
          case "setAttribute":
            return true;
          case "removeAttribute": {
            const targetRange =
              safeTarget.src !== undefined
                ? resolveInlineAnchor(current, match.range, { src: safeTarget.src })?.element.range
                : match.range;
            if (!targetRange) return false;
            removeAttributePlan(current, targetRange, op.name, revision);
            return true;
          }
        }
      } catch {
        return false;
      }
    },
    [activeBufferRef, revisionForSource, safeTarget],
  );

  const canRunStructuralOp = useCallback(
    (surface: StructuralSurface, action: StructuralAction): boolean => {
      if (!safeTarget) return false;
      const current = activeBufferRef.current;
      const match = resolveSafeBlockTarget(current, safeTarget);
      if (!match) return false;
      try {
        planStructuralOp(current, surface, action, match.range, revisionForSource(current));
        return true;
      } catch {
        return false;
      }
    },
    [activeBufferRef, revisionForSource, safeTarget],
  );

  const propertyAttributes = useMemo(() => {
    if (!safeTarget) return [];
    const current = activeBuffer;
    const match = resolveSafeBlockTarget(current, safeTarget);
    if (!match) return [];
    const location =
      safeTarget.src !== undefined
        ? resolveInlineAnchor(current, match.range, { src: safeTarget.src })?.element
        : locateElement(current, match.range);
    if (!location) return [];
    return location.attrs.map((attribute) => ({
      name: attribute.name,
      value: location.attrValues[attribute.name]
        ? decodeHtmlEntities(
            current.slice(
              location.attrValues[attribute.name].start,
              location.attrValues[attribute.name].end,
            ),
          )
        : "",
    }));
  }, [activeBuffer, safeTarget]);

  return { runBlockStructuralOp, runBlockRichOp, canRunRichOp, canRunStructuralOp, propertyAttributes };
}
