import type { LoadedDocument } from "@rightkit/document-core/documents/types";

/** DOM id for a document's tab, keyed by the document's stable identity (tabKey, else path) so it
 *  never shifts when tabs reorder. Characters outside [A-Za-z0-9_-] are encoded: ids are used in
 *  space-separated aria idref lists. */
export function tabDomId(doc: Pick<LoadedDocument, "tabKey" | "path">): string {
  const raw = doc.tabKey ?? doc.path;
  return `vr-tab-${raw.replace(/[^A-Za-z0-9_-]/g, (c) => `_${c.charCodeAt(0).toString(16)}_`)}`;
}
