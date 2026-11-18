// Host-supplied local speech-model capabilities.
import { dictationClient } from "../host.js";

export type LocalBackend = "parakeet-unified" | "parakeet-tdt";
export type LocalAsrPhase = "" | "downloading" | "extracting" | "preparing";
export type WhisperModelStatus = { installed: boolean; prepared: boolean };
export type LocalAsrModelStatus = {
  backend: LocalBackend;
  label: string;
  installed: boolean;
  source: "app-data" | "missing";
  replacement_required: boolean;
};

/** Local speech models: status, download, warm-up. Progress arrives as the `localAsrModel` and `whisperModel` events. */
export interface SpeechClient {
  localAsrModelStatus(backend: LocalBackend): Promise<LocalAsrModelStatus>;
  downloadLocalAsrModel(backend: LocalBackend): Promise<LocalAsrModelStatus>;
  whisperModelStatus(): Promise<WhisperModelStatus>;
  downloadWhisperModel(): Promise<void>;
  /** Loads the active engine so the next recording starts at once. */
  prewarmEngine(): Promise<void>;
  /** "Later" on the model-update prompt: the host asks again on its next launch. */
  dismissModelUpdate(): Promise<void>;
}

/** Resolve optional speech capabilities without configuring any product UI. */
export function sectionClient(group: "speech"): SpeechClient {
  const found = dictationClient()[group];
  if (!found) throw new Error("@rightkit/dictation-ui: DictationClient has no `speech` group");
  return found;
}
