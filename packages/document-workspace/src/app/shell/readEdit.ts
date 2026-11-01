import { capabilitiesFor } from "../../documents/capabilities.js";
import type { DocFormat, ReadEditMode } from "@rightkit/document-core/documents/types";

/** Every editable format opens editable with its tools showing — the host app edits inline with no
 *  extra step. Formats that cannot edit (web previews) stay read-only. */
export function effectiveReadEdit(doc: { format: DocFormat }): ReadEditMode {
  return capabilitiesFor(doc.format).canEdit ? "edit" : "read";
}
