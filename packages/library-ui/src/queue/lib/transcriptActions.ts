import { formatInvokeError } from "./errorMessage";
import type { InvokeFn } from "./transcriptLoad";

export type TranscriptFileAction =
  | "openSource"
  | "openTranscript"
  | "revealTranscript"
  | "openMedia"
  | "revealMedia";

export const TRANSCRIPT_FILE_COMMANDS: Record<TranscriptFileAction, string> = {
  openSource: "open_source_url",
  openTranscript: "open_file",
  revealTranscript: "reveal_in_folder",
  openMedia: "open_media_file",
  revealMedia: "reveal_media_in_folder",
};

/** Runs one file action for a saved transcript and reports failures through `onError`. */
export function runTranscriptFileAction(
  invokeFn: InvokeFn,
  action: TranscriptFileAction,
  id: string,
  onError: (message: string) => void,
): void {
  invokeFn(TRANSCRIPT_FILE_COMMANDS[action], { id }).catch((e) => onError(formatInvokeError(e)));
}
