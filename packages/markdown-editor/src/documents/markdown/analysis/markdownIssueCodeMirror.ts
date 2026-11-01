/**
 * CodeMirror diagnostics adapter.
 *
 * Reads the {@link MarkdownAnalysis} and emits a list of
 * `EditorView`-compatible diagnostic markers for the source
 * editor. The source editor uses these to underline problematic
 * spans with a severity-tinted color.
 */

import type { Diagnostic } from "@codemirror/lint";
import type { MarkdownAnalysis } from "./types";

/**
 * Build CodeMirror diagnostics from the analysis result. The result
 * is a flat list keyed by source range.
 */
export function buildCodeMirrorDiagnostics(analysis: MarkdownAnalysis): readonly Diagnostic[] {
  return analysis.syntaxIssues.map((issue) => ({
    from: issue.range.from,
    to: issue.range.to,
    severity: severityForCodeMirror(issue.severity),
    message: issue.message,
  }));
}

function severityForCodeMirror(
  severity: "error" | "warning" | "info",
): "error" | "warning" | "info" {
  return severity;
}
