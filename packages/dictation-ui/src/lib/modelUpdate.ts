// modelUpdate — the one shared state machine for the local speech model
// replacement (Parakeet TDT -> Parakeet Ultra). The Hub-root prompt and the
// Settings banner both read this store and both start the download through it,
// so progress is identical everywhere and a second download can never start.
import { useSyncExternalStore } from "react";
import { invoke, listen } from "./tauri.js";

type ModelUpdateStatus = "closed" | "prompt" | "downloading" | "done" | "error";

export type ModelUpdateState = {
  status: ModelUpdateStatus;
  /** True while a download is in flight, even if the prompt is hidden. */
  busy: boolean;
  phase: "starting" | "downloading" | "verifying" | "preparing";
  pct: number;
  error: string;
  modelLabel: string;
  downloadBytes: number;
};

const BACKEND = "parakeet-tdt";

let state: ModelUpdateState = {
  status: "closed",
  busy: false,
  phase: "starting",
  pct: 0,
  error: "",
  modelLabel: "Parakeet Ultra",
  downloadBytes: 0,
};
const subscribers = new Set<() => void>();

function set(patch: Partial<ModelUpdateState>) {
  state = { ...state, ...patch };
  for (const fn of subscribers) fn();
}

export function getModelUpdateState(): ModelUpdateState {
  return state;
}

export function subscribeModelUpdate(fn: () => void): () => void {
  subscribers.add(fn);
  return () => {
    subscribers.delete(fn);
  };
}

export function useModelUpdate(): ModelUpdateState {
  return useSyncExternalStore(subscribeModelUpdate, getModelUpdateState, getModelUpdateState);
}

/** Show the prompt (idempotent; never starts a download). */
export function openModelUpdatePrompt(info?: { modelLabel?: string; downloadBytes?: number }) {
  const patch: Partial<ModelUpdateState> = {};
  if (info?.modelLabel) patch.modelLabel = info.modelLabel;
  if (info?.downloadBytes && info.downloadBytes > 0) patch.downloadBytes = info.downloadBytes;
  if (state.status === "closed") {
    // A running download reopens on its progress view.
    set({ ...patch, status: state.busy ? "downloading" : "prompt", error: "" });
  } else {
    set(patch);
  }
}

/** "Later": the backend re-prompts on next launch. */
export function dismissModelUpdate() {
  set({ status: "closed" });
  void invoke("dismiss_model_update").catch(() => undefined);
}

/** Hide the prompt without dismissing; an in-flight download keeps running. */
export function hideModelUpdate() {
  set({ status: "closed" });
}

export async function startModelUpdateDownload(): Promise<void> {
  if (state.busy) {
    set({ status: "downloading" });
    return;
  }
  set({ status: "downloading", busy: true, phase: "starting", pct: 0, error: "" });
  const unlisten = listen<{ backend: string; phase: string; received: number; total: number }>(
    "local-asr-model",
    (event) => {
      const { backend, phase, received, total } = event.payload;
      if (backend !== BACKEND || !state.busy) return;
      if (phase === "downloading") {
        set({ phase: "downloading", pct: total > 0 ? Math.round((received / total) * 100) : 0 });
      } else if (phase === "extracting" || phase === "verifying" || phase === "installing") {
        set({ phase: "verifying", pct: 100 });
      } else if (phase === "preparing") {
        set({ phase: "preparing", pct: 100 });
      }
    },
  );
  try {
    const result = await invoke<{ installed?: boolean }>("download_local_asr_model", {
      backend: BACKEND,
    });
    if (result && result.installed === false) {
      throw new Error("The model finished downloading but is not ready yet.");
    }
    await invoke("register_hotkeys_now").catch(() => undefined);
    set({ status: "done", busy: false, pct: 100 });
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    set({ status: "error", busy: false, error: message.trim() || "Unknown error" });
  } finally {
    void unlisten.then((f) => f()).catch(() => undefined);
  }
}

export function formatDownloadSize(bytes: number): string {
  if (!(bytes > 0)) return "";
  const mb = bytes / 1_000_000;
  if (mb >= 1000) return `${(mb / 1000).toFixed(1)} GB`;
  return `${Math.round(mb)} MB`;
}

export function modelUpdatePhaseLabel(s: ModelUpdateState): string {
  switch (s.phase) {
    case "downloading":
      return `Downloading... ${s.pct}%`;
    case "verifying":
      return "Verifying and installing...";
    case "preparing":
      return "Getting the model ready...";
    default:
      return "Starting...";
  }
}
