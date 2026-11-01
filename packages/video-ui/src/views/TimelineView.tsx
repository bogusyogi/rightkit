import "./FinishedEdit.css";
import type { ProjectShow } from "../lib/project";
import type { ProgressState } from "../hooks/useProgress";
import { TimelineEditorView, type PreviewHost } from "../editor/TimelineEditorView";

/**
 * Finished edit screen (docs/design/app-ui.md §3.4). Thin host: the whole
 * screen — stage, transport, side tabs, timeline — lives in
 * `TimelineEditorView`; render/export controls live in the Exports tab.
 * `project.show` stays the single source of truth; editing goes through the
 * canonical Rust timeline.
 */
export function TimelineView({
  show,
  progress,
  previewHost,
  onChanged,
}: {
  show: ProjectShow;
  progress: ProgressState;
  outputPath: string | null;
  rendering: boolean;
  /** Optional revision-bound preview adapter (native compositor / frames). */
  previewHost?: PreviewHost | null;
  onChanged: () => Promise<void>;
  onRender: (preset: "youtube" | "reels") => void;
}) {
  return (
    <TimelineEditorView
      show={show}
      previewHost={previewHost ?? null}
      onRefresh={onChanged}
      busy={progress.running}
    />
  );
}
