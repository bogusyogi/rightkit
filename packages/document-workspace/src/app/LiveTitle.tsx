import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import { useEditText } from "./useEditText.js";

/**
 * A document title that stays live as you type — for untitled buffers whose name is derived from their
 * first line. It subscribes to its OWN doc's edit slice (via the edits store), so it re-renders on a
 * keystroke without re-rendering the memoized Sidebar/Toolbar around it. `compute` is the host's title
 * function (Sidebar's `titleFor`, the Toolbar's titlebar variant); it reads the same editsRef the
 * subscription tracks, so calling it after the subscription fires yields the fresh title.
 */
export function LiveTitle({
  doc,
  compute,
  subscribeEdits,
  getEditSnapshot,
}: {
  doc: LoadedDocument;
  compute: (doc: LoadedDocument) => string;
  subscribeEdits: (path: string, cb: () => void) => () => void;
  getEditSnapshot: (path: string) => string | undefined;
}) {
  // Subscribe for the re-render; the value itself is unused (compute reads the live ref).
  useEditText(subscribeEdits, getEditSnapshot, doc.path);
  return <>{compute(doc)}</>;
}
