import type { BlockDraftDecision, BlockDraftIntent, BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";

/** Resolve every modified draft for one document in order; first cancellation/failure wins. */
export async function resolveDocumentDrafts(
  registry: BlockDraftRegistry,
  documentPath: string,
  intent: BlockDraftIntent,
  decision: BlockDraftDecision,
): Promise<"resolved" | "cancelled" | "failed"> {
  const drafts = registry.snapshotModified(documentPath);
  for (const draft of drafts) {
    const result = await registry.resolve(draft.identity, intent, decision);
    if (result.outcome === "cancelled") return "cancelled";
    if (result.outcome === "failed") return "failed";
    // done / discarded / missing → continue to the next draft
  }
  return "resolved";
}

/** Resolve every modified draft in the registry (close-all / quit / setup replacement). */
export async function resolveAllDrafts(
  registry: BlockDraftRegistry,
  intent: BlockDraftIntent,
  decision: BlockDraftDecision,
): Promise<"resolved" | "cancelled" | "failed"> {
  const drafts = registry.snapshotModified();
  for (const draft of drafts) {
    const result = await registry.resolve(draft.identity, intent, decision);
    if (result.outcome === "cancelled") return "cancelled";
    if (result.outcome === "failed") return "failed";
  }
  return "resolved";
}
