// A single-flight store for replacing the local speech model. Every surface that shows the
// replacement (a prompt, a settings banner) reads one store and starts the download through it,
// so progress is identical everywhere and a second download never starts. The host supplies the
// transport, command and event names, and every label; the store holds no product copy.
import { useSyncExternalStore } from "react";

export type ModelUpdateStatus = "closed" | "prompt" | "downloading" | "done" | "error";
export type ModelUpdatePhase = "starting" | "downloading" | "verifying" | "preparing";

export type ModelUpdateState = {
  status: ModelUpdateStatus;
  /** True while a download is in flight, even if the prompt is hidden. */
  busy: boolean;
  phase: ModelUpdatePhase;
  pct: number;
  error: string;
  modelLabel: string;
  downloadBytes: number;
};

type ProgressPayload = { backend: string; phase: string; received: number; total: number };
type Unlisten = () => void;

export type ModelUpdateStoreOptions = {
  invoke: <T = unknown>(command: string, args?: Record<string, unknown>) => Promise<T>;
  listen: <T>(event: string, handler: (event: { payload: T }) => void) => Promise<Unlisten>;
  /** Backend id the download command and progress events carry. */
  backend: string;
  /** Label shown before the host reports one. */
  modelLabel: string;
  commands: {
    download: string;
    dismiss: string;
    /** Optional command run after a successful install (for example re-registering hotkeys). */
    afterInstall?: string;
  };
  /** Progress event name; payload `{ backend, phase, received, total }`. */
  progressEvent: string;
  /** Error text when the download returns `installed: false`. */
  notReadyMessage: string;
  /** Error text when the failure carries no message. */
  unknownErrorMessage: string;
};

export type ModelUpdateStore = {
  get(): ModelUpdateState;
  subscribe(fn: () => void): () => void;
  use(): ModelUpdateState;
  /** Show the prompt (idempotent; never starts a download). */
  open(info?: { modelLabel?: string; downloadBytes?: number }): void;
  /** "Later": tells the host, which may re-prompt on next launch. */
  dismiss(): void;
  /** Hide the prompt without dismissing; an in-flight download keeps running. */
  hide(): void;
  start(): Promise<void>;
};

export function createModelUpdateStore(options: ModelUpdateStoreOptions): ModelUpdateStore {
  const { invoke, listen, backend, commands } = options;
  let state: ModelUpdateState = {
    status: "closed",
    busy: false,
    phase: "starting",
    pct: 0,
    error: "",
    modelLabel: options.modelLabel,
    downloadBytes: 0,
  };
  const subscribers = new Set<() => void>();

  function set(patch: Partial<ModelUpdateState>) {
    state = { ...state, ...patch };
    for (const fn of subscribers) fn();
  }
  const get = () => state;
  const subscribe = (fn: () => void) => {
    subscribers.add(fn);
    return () => {
      subscribers.delete(fn);
    };
  };

  return {
    get,
    subscribe,
    use: () => useSyncExternalStore(subscribe, get, get),
    open(info) {
      const patch: Partial<ModelUpdateState> = {};
      if (info?.modelLabel) patch.modelLabel = info.modelLabel;
      if (info?.downloadBytes && info.downloadBytes > 0) patch.downloadBytes = info.downloadBytes;
      if (state.status === "closed") {
        // A running download reopens on its progress view.
        set({ ...patch, status: state.busy ? "downloading" : "prompt", error: "" });
      } else {
        set(patch);
      }
    },
    dismiss() {
      set({ status: "closed" });
      void invoke(commands.dismiss).catch(() => undefined);
    },
    hide() {
      set({ status: "closed" });
    },
    async start() {
      if (state.busy) {
        set({ status: "downloading" });
        return;
      }
      set({ status: "downloading", busy: true, phase: "starting", pct: 0, error: "" });
      let unlisten: Unlisten | undefined;
      try {
        unlisten = await listen<ProgressPayload>(options.progressEvent, (event) => {
          const { backend: from, phase, received, total } = event.payload;
          if (from !== backend || !state.busy) return;
          if (phase === "downloading") {
            set({ phase: "downloading", pct: total > 0 ? Math.round((received / total) * 100) : 0 });
          } else if (phase === "extracting" || phase === "verifying" || phase === "installing") {
            set({ phase: "verifying", pct: 100 });
          } else if (phase === "preparing") {
            set({ phase: "preparing", pct: 100 });
          }
        });
        const result = await invoke<{ installed?: boolean }>(commands.download, { backend });
        if (result && result.installed === false) {
          throw new Error(options.notReadyMessage);
        }
        if (commands.afterInstall) await invoke(commands.afterInstall).catch(() => undefined);
        set({ status: "done", busy: false, pct: 100 });
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        set({ status: "error", busy: false, error: message.trim() || options.unknownErrorMessage });
      } finally {
        try {
          unlisten?.();
        } catch {
          // Listener cleanup must not change the result of the download.
        }
      }
    },
  };
}
