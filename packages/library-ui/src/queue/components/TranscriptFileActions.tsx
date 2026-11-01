import { ExternalLink, FileText, FolderOpen } from "lucide-react";
import { invoke } from "../lib/ipc";
import { buttonClass } from "../lib/ui";
import { runTranscriptFileAction } from "../lib/transcriptActions";

type TranscriptFileActionsProps = {
  /** The saved transcript's id; null hides every action until the detail loads. */
  transcriptId: string | null;
  hasTranscript: boolean;
  hasMedia: boolean;
  onError: (message: string) => void;
};

export function TranscriptFileActions({
  transcriptId,
  hasTranscript,
  hasMedia,
  onError,
}: TranscriptFileActionsProps) {
  if (!transcriptId) return null;
  const run = (action: Parameters<typeof runTranscriptFileAction>[1]) =>
    runTranscriptFileAction(invoke, action, transcriptId, onError);
  return (
    <>
      {hasTranscript && (
        <>
          <button
            type="button"
            className={buttonClass}
            onClick={() => run("openTranscript")}
            title="Open the saved transcript file"
            aria-label="Open transcript file"
          >
            <FileText size={15} strokeWidth={1.8} />
            <span>Open file</span>
          </button>
          <button
            type="button"
            className={buttonClass}
            onClick={() => run("revealTranscript")}
            title="Show the saved transcript files in their folder"
            aria-label="Show transcript in folder"
          >
            <FolderOpen size={15} strokeWidth={1.8} />
            <span>Folder</span>
          </button>
        </>
      )}
      {hasMedia && (
        <>
          <button
            type="button"
            className={buttonClass}
            onClick={() => run("openMedia")}
            title="Open downloaded media"
            aria-label="Open download"
          >
            <ExternalLink size={15} strokeWidth={1.8} />
            <span>Open download</span>
          </button>
          <button
            type="button"
            className={buttonClass}
            onClick={() => run("revealMedia")}
            title="Select downloaded media in its folder"
            aria-label="Show download in folder"
          >
            <FolderOpen size={15} strokeWidth={1.8} />
            <span>Folder</span>
          </button>
        </>
      )}
    </>
  );
}
