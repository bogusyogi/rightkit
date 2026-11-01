/**
 * Public contracts for the Markdown analysis layer.
 *
 * The analysis layer produces a structured list of {@link MarkdownIssue}s
 * over a {@link import("../core/MarkdownDocumentTypes").MarkdownCoreSnapshot}.
 * It never mutates the core; it only reads.
 */

import type { MarkdownNodeId } from "../core/MarkdownNodeIdFactory";
import type { MarkdownTextEdit } from "../core/types";
import type { MarkdownSourceNode } from "../core/MarkdownSourceMap";

/**
 * Severity assigned to a {@link MarkdownIssue}. `error` blocks export;
 * `warning` is shown in the UI but does not block; `info` is hidden
 * by default.
 */
export type MarkdownIssueSeverity = "error" | "warning" | "info";

/**
 * Stable identifier of a Markdown analysis rule. New rules add
 * new codes; existing codes never change semantics.
 */
export type MarkdownIssueCode =
  | "md.heading.skipped-level"
  | "md.heading.multiple-h1"
  | "md.heading.duplicate-id"
  | "md.link.missing-target"
  | "md.link.missing-anchor"
  | "md.image.missing-target"
  | "md.image.missing-alt"
  | "md.image.embedded-data"
  | "md.footnote.missing-definition"
  | "md.footnote.unused-definition"
  | "md.fence.unclosed"
  | "md.code.unknown-language"
  | "md.table.malformed"
  | "md.math.invalid"
  | "md.mermaid.invalid"
  | "md.html.blocked"
  | "md.trailing-whitespace"
  | "md.frontmatter.invalid";

/**
 * Quick-fix applied to a {@link MarkdownIssue}. A `command`-kind
 * fix is a batch of edits routed through the core's `applyCommand`
 * with `origin: "diagnostics-fix"`; an `action`-kind fix dispatches
 * to a UI surface (e.g. opening the file picker for a missing link
 * target). Actions never carry executable callbacks.
 */
export type MarkdownQuickFix =
  | {
      kind: "command";
      readonly title: string;
      readonly label: string;
      readonly origin: "diagnostics-fix";
      readonly edits: readonly MarkdownTextEdit[];
      readonly preserveNodeIds?: readonly MarkdownNodeId[];
    }
  | {
      kind: "action";
      readonly title: string;
      readonly action: "persist-data-image" | "pick-link-target" | "open-properties";
      readonly nodeId?: MarkdownNodeId;
    };

/**
 * One diagnostic emitted by the analysis layer. IDs are stable for
 * the lifetime of the document; an unchanged document produces the
 * same IDs in the same order.
 */
export interface MarkdownIssue {
  readonly id: string;
  readonly code: MarkdownIssueCode;
  readonly severity: MarkdownIssueSeverity;
  readonly message: string;
  readonly range: { readonly from: number; readonly to: number };
  readonly nodeId?: MarkdownNodeId;
  readonly relatedPath?: string;
  readonly quickFixes?: readonly MarkdownQuickFix[];
}

/**
 * Half-open logical-source range used by the analysis layer.
 */
export interface MarkdownSourceRange {
  readonly from: number;
  readonly to: number;
}

/**
 * One link or image reference resolved by the analysis layer.
 */
export interface MarkdownReferenceEntry {
  readonly rawUrl: string;
  readonly kind: "link" | "image";
  readonly range: MarkdownSourceRange;
  readonly nodeId: MarkdownNodeId | null;
  readonly localPath: string | null;
  readonly fragment: string | null;
}

/**
 * One footnote reference or definition entry.
 */
export interface MarkdownFootnoteEntry {
  readonly identifier: string;
  readonly range: MarkdownSourceRange;
  readonly nodeId: MarkdownNodeId | null;
}

/**
 * Full analysis result for a single snapshot.
 */
export interface MarkdownAnalysis {
  readonly revision: number;
  readonly headings: readonly MarkdownSourceNode[];
  readonly references: readonly MarkdownReferenceEntry[];
  readonly footnoteReferences: readonly MarkdownFootnoteEntry[];
  readonly footnoteDefinitions: readonly MarkdownFootnoteEntry[];
  readonly syntaxIssues: readonly MarkdownIssue[];
}

/**
 * Issue ID format: `${code}:${nodeId ?? "none"}:${range.from}:${range.to}`.
 */
export function makeIssueId(
  code: MarkdownIssueCode,
  nodeId: MarkdownNodeId | undefined | null,
  range: MarkdownSourceRange,
): string {
  return `${code}:${nodeId ?? "none"}:${range.from}:${range.to}`;
}
