import type { ReactNode } from "react";

export function SourceEditorToolbar({
  folded,
  showInvisibles,
  actions,
  markdownControls,
  onFormat,
  onToggleFold,
  onGoToLine,
  onToggleInvisibles,
}: {
  folded: boolean;
  showInvisibles: boolean;
  actions?: ReactNode;
  /** Markdown formatting controls (heading, bold, lists, ...), flowed into the same row. */
  markdownControls?: ReactNode;
  onFormat: () => void;
  onToggleFold: () => void;
  onGoToLine: () => void;
  onToggleInvisibles: () => void;
}) {
  return (
    <div className="no-print vr-chrome vr-capsule-scroll flex min-h-9 min-w-0 flex-nowrap items-center gap-1 whitespace-nowrap px-1 text-chrome-xs text-muted">
      <button
        type="button"
        title="Format source"
        aria-label="Format source"
        onClick={onFormat}
        className="vr-press vr-focus rounded px-2 py-1 hover:bg-panel-2 hover:text-paper"
      >
        fmt
      </button>
      <span className="mx-1 h-5 w-px bg-line" />
      <button
        type="button"
        title={folded ? "Unfold all" : "Fold all"}
        aria-label={folded ? "Unfold all" : "Fold all"}
        aria-pressed={folded}
        onClick={onToggleFold}
        className={`vr-press vr-focus rounded px-2 py-1 hover:bg-panel-2 hover:text-paper ${folded ? "text-copper-hi" : ""}`}
      >
        {folded ? "unfold" : "fold"}
      </button>
      <button
        type="button"
        title="Go to line"
        aria-label="Go to line"
        onClick={onGoToLine}
        className="vr-press vr-focus rounded px-2 py-1 hover:bg-panel-2 hover:text-paper"
      >
        line
      </button>
      <span className="mx-1 h-5 w-px bg-line" />
      <button
        type="button"
        title={showInvisibles ? "Hide invisible characters" : "Show invisible characters"}
        aria-label={showInvisibles ? "Hide invisible characters" : "Show invisible characters"}
        aria-pressed={showInvisibles}
        onClick={onToggleInvisibles}
        className={`vr-press vr-focus rounded px-2 py-1 hover:bg-panel-2 hover:text-paper ${showInvisibles ? "text-copper-hi" : ""}`}
      >
        marks
      </button>
      {markdownControls && (
        <>
          <span className="mx-1 h-5 w-px bg-line" />
          {markdownControls}
        </>
      )}
      {actions && (
        <>
          <div className="min-w-2 flex-1" />
          <div className="flex shrink-0 items-center gap-1">{actions}</div>
        </>
      )}
    </div>
  );
}
