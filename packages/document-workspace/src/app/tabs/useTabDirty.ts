import { useEffect, useState } from "react";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";

/** Live dirty state for a tab: the caller's hint OR the subscribed per-document dirty signal. */
export function useTabDirty(
  doc: LoadedDocument,
  dirtyHint: boolean,
  isDirty: (doc: LoadedDocument) => boolean,
  subscribeDirty: (path: string, cb: () => void) => () => void,
) {
  const [observedDirty, setObservedDirty] = useState(() => isDirty(doc));
  useEffect(() => {
    const sync = () => setObservedDirty(isDirty(doc));
    sync();
    return subscribeDirty(doc.path, sync);
  }, [doc, isDirty, subscribeDirty]);
  return dirtyHint || observedDirty;
}
