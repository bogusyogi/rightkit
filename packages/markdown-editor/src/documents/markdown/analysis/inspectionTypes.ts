/**
 * Inspection contracts.
 *
 * Inspection is the local equivalent of "hover to peek at the
 * parsed node." The types below describe what an inspection call
 * returns.
 */

import type { MarkdownNodeId } from "../core/MarkdownNodeIdFactory";
import type { MarkdownSourceRange } from "./types";

/**
 * One entry in the inspection response. The `snippet` is a
 * short excerpt of the source the editor displays under the
 * cursor; the `details` are free-form and may include external
 * resources for the brokered inspection pipeline.
 */
export interface MarkdownInspectionEntry {
  readonly nodeId: MarkdownNodeId;
  readonly kind: string;
  readonly range: MarkdownSourceRange;
  readonly snippet: string;
  readonly details: readonly MarkdownInspectionDetail[];
}

export type MarkdownInspectionDetail =
  | { readonly kind: "text"; readonly text: string }
  | {
      readonly kind: "key-value";
      readonly pairs: readonly { readonly key: string; readonly value: string }[];
    }
  | { readonly kind: "code"; readonly language: string; readonly source: string }
  | { readonly kind: "list"; readonly items: readonly MarkdownInspectionEntry[] }
  | { readonly kind: "missing-target"; readonly path: string };

/**
 * Inspection request. The cursor position is logical-source.
 */
export interface MarkdownInspectionRequest {
  readonly offset: number;
  readonly maxNodes?: number;
  readonly includeSnippets?: boolean;
  readonly snippetMaxLength?: number;
}

/**
 * Inspection response. Empty `entries` when the cursor is not
 * inside any tracked node.
 */
export interface MarkdownInspectionResponse {
  readonly revision: number;
  readonly entries: readonly MarkdownInspectionEntry[];
}
