/**
 * Issues panel: a small React component that renders the analysis
 * issue list and dispatches quick-fix commands back through the
 * core when the user clicks a fix button.
 */

import { useCallback, type ReactNode } from "react";
import type { MarkdownAnalysis, MarkdownQuickFix } from "./types";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore";

export interface MarkdownIssuesPanelProps {
  readonly analysis: MarkdownAnalysis | null;
  readonly core: MarkdownDocumentCore | undefined;
  readonly onJumpToRange?: (range: { from: number; to: number }) => void;
}

/**
 * Render the issue list. Returns `null` when `analysis` is `null`.
 */
export function MarkdownIssuesPanel({
  analysis,
  core,
  onJumpToRange,
}: MarkdownIssuesPanelProps): ReactNode {
  const onApplyCommand = useApplyCommand(core, analysis?.revision);

  if (!analysis) return null;
  if (analysis.syntaxIssues.length === 0) {
    return <div data-testid="issues-empty">No issues</div>;
  }
  return (
    <ul data-testid="issues-panel">
      {analysis.syntaxIssues.map((issue) => (
        <li key={issue.id} data-testid={`issue-${issue.code}`} data-severity={issue.severity}>
          <button type="button" onClick={() => onJumpToRange?.(issue.range)}>
            {issue.message}
          </button>
          {issue.quickFixes?.map((fix, idx) => (
            <QuickFixButton key={`${issue.id}-${idx}`} fix={fix} onApply={onApplyCommand} />
          ))}
        </li>
      ))}
    </ul>
  );
}

function QuickFixButton({
  fix,
  onApply,
}: {
  fix: MarkdownQuickFix;
  onApply: (fix: MarkdownQuickFix) => void;
}): ReactNode {
  const handleClick = useCallback(() => onApply(fix), [fix, onApply]);
  return (
    <button type="button" data-testid="quickfix" onClick={handleClick}>
      {fix.title}
    </button>
  );
}

function useApplyCommand(
  core: MarkdownDocumentCore | undefined,
  expectedRevision: number | undefined,
): (fix: MarkdownQuickFix) => void {
  return useCallback(
    (fix: MarkdownQuickFix) => {
      if (fix.kind !== "command") return;
      if (!core) return;
      core.applyCommand({
        edits: fix.edits,
        expectedRevision,
        origin: "diagnostics-fix",
        label: "Replace",
        isolateHistory: true,
      });
    },
    [core, expectedRevision],
  );
}
