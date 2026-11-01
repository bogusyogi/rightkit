import { useCallback, useState } from "react";
import type { BlockDraftIntent, BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import { resolveDocumentDrafts } from "./draftResolution";

interface DraftConfirmGateDeps {
  blockDrafts: BlockDraftRegistry;
}

/**
 * The draft-confirm chooser state + its gate. `requestDraftAwareViewChange` wraps a view change so
 * a modified block draft first opens the Done/Discard/Cancel chooser (Done commits, Discard drops,
 * Cancel stays on the current view with the editor open); a draft-free document proceeds
 * immediately. Extracted verbatim from useDocuments; bodies are unchanged.
 */
export function useDraftConfirmGate({ blockDrafts }: DraftConfirmGateDeps) {
  const [draftConfirm, setDraftConfirm] = useState<{
    documentPath: string;
    intent: BlockDraftIntent;
    onResolved: () => void;
  } | null>(null);
  const confirmDraftDone = useCallback(async () => {
    const c = draftConfirm;
    if (!c) return;
    setDraftConfirm(null);
    const outcome = await resolveDocumentDrafts(blockDrafts, c.documentPath, c.intent, "done");
    if (outcome === "resolved") c.onResolved();
  }, [draftConfirm, blockDrafts]);
  const confirmDraftDiscard = useCallback(async () => {
    const c = draftConfirm;
    if (!c) return;
    setDraftConfirm(null);
    const outcome = await resolveDocumentDrafts(blockDrafts, c.documentPath, c.intent, "discard");
    if (outcome === "resolved") c.onResolved();
  }, [draftConfirm, blockDrafts]);
  const confirmDraftCancel = useCallback(() => setDraftConfirm(null), []);
  // Gate a view change on the active document's modified drafts: open the Done/Discard/Cancel
  // chooser when a draft exists, else proceed immediately.
  const requestDraftAwareViewChange = useCallback(
    (path: string, proceed: () => void) => {
      if (blockDrafts.snapshotModified(path).length > 0) {
        setDraftConfirm({ documentPath: path, intent: "view-change", onResolved: proceed });
        return;
      }
      proceed();
    },
    [blockDrafts],
  );
  return {
    draftConfirm,
    setDraftConfirm,
    confirmDraftDone,
    confirmDraftDiscard,
    confirmDraftCancel,
    requestDraftAwareViewChange,
  };
}
