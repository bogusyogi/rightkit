import { useCallback, useEffect, useMemo, useState } from "react";
import { useOptionalBlockDrafts } from "@rightkit/document-core/app/BlockDraftContext";
import { createBlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import type { BlockDraftRegistry, BlockDraftSnapshot } from "@rightkit/document-core/app/blockDrafts";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore.js";
import type { BlockKind } from "./types.js";

export function useMarkdownBlockDraft(
  core: MarkdownDocumentCore,
  kind: BlockKind,
  range: { from: number; to: number },
) {
  const outerRegistry = useOptionalBlockDrafts();
  const fallbackRegistry = useMemo<BlockDraftRegistry | null>(() => {
    if (outerRegistry) return null;
    return createBlockDraftRegistry();
  }, [outerRegistry]);
  const registry = (outerRegistry ?? fallbackRegistry) as BlockDraftRegistry;
  const identity = useMemo(
    () => ({
      documentPath: core.state.documentId,
      // A document can host several protected blocks at once. Keep each draft scoped to its
      // authoritative source range so opening a second editor cannot update or commit the first.
      pagePath: `${core.state.documentId}#markdown-block:${range.from}:${range.to}`,
    }),
    [core, range.from, range.to],
  );
  const original = useMemo(
    () => core.snapshot().logicalSource.slice(range.from, range.to),
    [core, range],
  );
  const [snapshot, setSnapshot] = useState<BlockDraftSnapshot | null>(() => registry.get(identity));

  useEffect(() => {
    const start = core.snapshot();
    const result = registry.begin({
      identity,
      blockKind: kind,
      originalRevision: start.revision,
      sourceRange: { start: range.from, end: range.to },
      originalText: original,
      commit: (draft) => {
        // The block editor unmounts whenever its tab goes to the background, but a pane holding a
        // draft stays live (drafts are protected from eviction), so its document core does too.
        // Commit through that core; refuse only once the core itself is gone.
        if (core.state.lifecycle !== "ready")
          return {
            ok: false as const,
            kind: "stale" as const,
            reason: "Editor closed before commit.",
          };
        const current = core.snapshot();
        if (
          current.revision !== draft.originalRevision ||
          current.logicalSource.slice(range.from, range.to) !== draft.originalText
        ) {
          return {
            ok: false as const,
            kind: "stale" as const,
            reason: "Block changed outside this editor.",
          };
        }
        core.applyCommand({
          edits: [
            { from: range.from, to: range.to, insert: draft.currentText, inheritMode: "preserve" },
          ],
          expectedRevision: current.revision,
          origin: "block-editor",
          label: "BlockEdit",
          isolateHistory: true,
          timestampMs: Math.round(performance.now()),
        });
        return { ok: true as const };
      },
      discard: () => undefined,
    });
    setSnapshot(result.snapshot);
  }, [core, identity, kind, original, range.from, range.to, registry]);

  useEffect(
    () => registry.subscribe(() => setSnapshot(registry.get(identity))),
    [identity, registry],
  );
  const update = useCallback(
    (text: string) => registry.update(identity, text),
    [identity, registry],
  );
  const done = useCallback(
    () => registry.resolve(identity, "manual-save", "done"),
    [identity, registry],
  );
  const discard = useCallback(
    () => registry.resolve(identity, "view-change", "discard"),
    [identity, registry],
  );
  return { snapshot, update, done, discard };
}
