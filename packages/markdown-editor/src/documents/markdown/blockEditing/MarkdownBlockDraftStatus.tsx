import type { ReactNode } from "react";
import type { BlockDraftSnapshot } from "@rightkit/document-core/app/blockDrafts";

/** Surface an unresolved block-draft failure without closing its local editor. */
export function MarkdownBlockDraftStatus({
  snapshot,
}: {
  snapshot: BlockDraftSnapshot | null;
}): ReactNode {
  if (!snapshot || (snapshot.status !== "stale" && !snapshot.error)) return null;
  return (
    <p role="alert" data-testid="markdown-block-draft-status" className="mb-2 text-sm text-danger">
      {snapshot.reason ?? snapshot.error ?? "Block changed outside this editor."}
    </p>
  );
}
